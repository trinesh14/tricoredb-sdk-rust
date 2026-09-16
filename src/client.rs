//! One session with a server: a connection plus a completed handshake.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::error::{Error, ErrorKind, Result};
use crate::frame::{self, Frame, Tag};
use crate::options::Options;
use crate::response::{decode_variant, Response};
use crate::transport::{self, Transport};

const PROTOCOL_NAME: &str = "tricore";
const PROTOCOL_VERSION: u32 = 1;

/// How long [`Client::close`] waits for the server's goodbye before dropping
/// the socket. The goodbye is optional; the teardown is not.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// One session with a TriCoreDB server.
///
/// A connection is a single request/response stream, so every call takes
/// `&mut self`: the borrow checker is what stops two threads interleaving
/// frames on one connection. Use a [`crate::Pool`] for concurrency.
///
/// The session ends when the value is dropped. Call [`Client::close`] to say
/// goodbye politely and learn whether the socket closed cleanly.
pub struct Client {
    transport: Transport,
    database: String,
    session_id: Option<String>,
    granted_features: u64,
    txn_open: bool,
    request_counter: u64,
    request_id_prefix: String,
    last_request_id: Option<String>,
    request_timeout_ms: Option<u64>,
    read_timeout: Option<Duration>,
    /// Set once this connection can no longer be trusted to be frame-aligned.
    /// Every later call fails with the same error rather than reading the
    /// previous reply as the next answer.
    fault: Option<Error>,
    closed: bool,
}

#[derive(Deserialize)]
struct HelloOk {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    message: String,
    #[serde(default)]
    features: u64,
}

#[derive(Deserialize)]
struct AuthOk {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    data: Value,
    #[serde(default)]
    diagnostics: Option<Diagnostics>,
}

#[derive(Deserialize, Default)]
struct Diagnostics {
    #[serde(default)]
    route: Option<String>,
    #[serde(default)]
    elapsed_ms: Option<i64>,
    #[serde(default)]
    warnings: Vec<String>,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    leader_hint: Option<String>,
}

impl Client {
    /// Connect, shake hands, and authenticate when [`Options::user`] is set.
    ///
    /// ```no_run
    /// # fn main() -> tricoredb::Result<()> {
    /// use tricoredb::{Client, Options};
    /// let mut db = Client::connect(&Options::new("127.0.0.1", 8427).user("admin").secret("pw"))?;
    /// db.ping()?;
    /// # Ok(()) }
    /// ```
    pub fn connect(options: &Options) -> Result<Client> {
        let tcp = transport::dial(&options.host, options.port, options.connect_timeout)?;

        let transport = match &options.tls {
            #[cfg(feature = "tls")]
            Some(tls) => {
                // The connect budget covers the TLS handshake too.
                let stream = Transport::Tcp(tcp);
                stream.set_read_timeout(options.connect_timeout)?;
                stream.set_write_timeout(options.connect_timeout)?;
                let Transport::Tcp(tcp) = stream else {
                    unreachable!("just constructed as Tcp")
                };
                crate::tls::wrap(tcp, tls)?
            }
            #[cfg(not(feature = "tls"))]
            Some(_) => {
                return Err(Error::invalid(
                    "this build has no TLS support: enable the crate's `tls` feature",
                ))
            }
            None => Transport::Tcp(tcp),
        };

        let mut client = Client {
            transport,
            database: options.database.clone(),
            session_id: None,
            granted_features: 0,
            txn_open: false,
            request_counter: 0,
            request_id_prefix: next_connection_prefix(),
            last_request_id: None,
            request_timeout_ms: None,
            read_timeout: None,
            fault: None,
            closed: false,
        };

        // The handshake gets the connect budget; the steady state gets its own.
        client.transport.set_read_timeout(options.connect_timeout)?;
        client
            .transport
            .set_write_timeout(options.connect_timeout)?;

        client.hello(&options.client_name, options.announced_features())?;
        if let Some(user) = &options.user {
            let secret = options.secret.clone().unwrap_or_default();
            client.auth(user, &secret)?;
        }

        client.transport.set_write_timeout(None)?;
        client.set_read_timeout(options.read_timeout)?;
        Ok(client)
    }

    fn hello(&mut self, client_name: &str, features: u64) -> Result<()> {
        let payload = json!({
            "protocol": PROTOCOL_NAME,
            "version": {"major": PROTOCOL_VERSION, "minor": 0},
            "client": client_name,
            "features": features,
        });
        let frame = self.exchange(Tag::Hello, Some(&payload))?;
        match frame.tag {
            Tag::HelloOk => {}
            Tag::Error => {
                return Err(Error::new(ErrorKind::Handshake, error_text(&frame.payload))
                    .with_code_opt(error_code(&frame.payload)))
            }
            other => {
                return Err(
                    self.poison(Error::protocol(format!("expected HELLO_OK, got {other:?}")))
                )
            }
        }
        let body: HelloOk = parse_body(&frame.payload, "HELLO_OK")?;
        if !body.ok {
            let message = if body.message.is_empty() {
                "the server refused the handshake".to_string()
            } else {
                body.message
            };
            return Err(Error::new(ErrorKind::Handshake, message));
        }
        self.granted_features = body.features;
        Ok(())
    }

    fn auth(&mut self, user: &str, secret: &str) -> Result<()> {
        let payload = json!({
            "username": user,
            "secret": crate::wire::byte_list(secret.as_bytes()),
        });
        let frame = self.exchange(Tag::Auth, Some(&payload))?;
        match frame.tag {
            Tag::AuthOk => {}
            Tag::Error => {
                return Err(Error::new(ErrorKind::Auth, error_text(&frame.payload))
                    .with_code_opt(error_code(&frame.payload)))
            }
            other => {
                return Err(self.poison(Error::protocol(format!("expected AUTH_OK, got {other:?}"))))
            }
        }
        let body: AuthOk = parse_body(&frame.payload, "AUTH_OK")?;
        // An AUTH_OK-tagged frame carrying `ok: false` is still a refusal: the
        // tag names the answer's shape, not its verdict.
        if !body.ok {
            let message = if body.message.is_empty() {
                "authentication refused".to_string()
            } else {
                body.message
            };
            return Err(Error::new(ErrorKind::Auth, message));
        }
        self.session_id = body.session_id;
        Ok(())
    }

    /// The session id the server assigned, or `None` when the connection never
    /// authenticated.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// The capabilities the server granted in the handshake.
    pub fn granted_features(&self) -> u64 {
        self.granted_features
    }

    /// Whether the server granted server-side parameter binding.
    pub fn server_params_granted(&self) -> bool {
        self.granted_features & crate::options::FEATURE_SERVER_PARAMS != 0
    }

    /// Whether the server granted session transactions.
    pub fn session_txn_granted(&self) -> bool {
        self.granted_features & crate::options::FEATURE_SESSION_TXN != 0
    }

    /// Whether a [`Client::begin`] block is open on this connection.
    ///
    /// A broken connection has no transaction: the server rolls one back the
    /// moment the socket goes.
    pub fn in_transaction(&self) -> bool {
        self.txn_open && self.fault.is_none() && !self.closed
    }

    pub(crate) fn set_txn_open(&mut self, open: bool) {
        self.txn_open = open;
    }

    /// The database named in every request from this connection.
    pub fn database(&self) -> &str {
        &self.database
    }

    /// Whether this connection was dropped after a failure it cannot recover
    /// from. A [`crate::Pool`] retires such a connection instead of reusing it.
    pub fn is_poisoned(&self) -> bool {
        self.fault.is_some()
    }

    /// Bound each later wait for a reply. `None` clears the bound.
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.read_timeout = timeout.filter(|d| !d.is_zero());
        self.transport.set_read_timeout(self.read_timeout)
    }

    /// Stamp a server-side deadline on every later request. `None` clears it.
    ///
    /// This is not the same as [`Client::set_read_timeout`]: abandoning the wait
    /// on this side leaves the server working. This makes the server stop.
    pub fn set_request_timeout(&mut self, timeout: Option<Duration>) {
        self.request_timeout_ms = timeout
            .filter(|d| !d.is_zero())
            .map(|d| d.as_millis().min(u64::MAX as u128) as u64);
    }

    /// The `request_id` most recently sent. Pass it to [`Client::cancel`] from a
    /// *second* connection to stop a running statement.
    pub fn last_request_id(&self) -> Option<&str> {
        self.last_request_id.as_deref()
    }

    /// Check liveness with a PING/PONG round trip.
    pub fn ping(&mut self) -> Result<()> {
        let frame = self.exchange(Tag::Ping, None)?;
        match frame.tag {
            Tag::Pong => Ok(()),
            other => Err(self.poison(Error::protocol(format!("expected PONG, got {other:?}")))),
        }
    }

    /// Ask the server to stop one of this principal's running statements.
    ///
    /// Send this on a **second** connection: the one running the statement is
    /// blocked reading its reply and will not see anything else. Returns how
    /// many statements were stopped; an unknown id stops none and is not an
    /// error.
    pub fn cancel(&mut self, request_id: &str) -> Result<u64> {
        if request_id.is_empty() {
            return Err(Error::invalid("a request id to cancel must not be empty"));
        }
        let frame = self.exchange(Tag::Cancel, Some(&json!({"request_id": request_id})))?;
        match frame.tag {
            Tag::CancelOk => {}
            Tag::Error => {
                return Err(Error::new(ErrorKind::Server, error_text(&frame.payload))
                    .with_code_opt(error_code(&frame.payload)))
            }
            other => {
                return Err(self.poison(Error::protocol(format!(
                    "expected CANCEL_OK, got {other:?}"
                ))))
            }
        }
        #[derive(Deserialize)]
        struct CancelOk {
            #[serde(default)]
            cancelled: u64,
        }
        let body: CancelOk = parse_body(&frame.payload, "CANCEL_OK")?;
        Ok(body.cancelled)
    }

    /// Send a raw operation and return the decoded response.
    ///
    /// The typed methods cover every operation a client should need; reach for
    /// this only when there is none — an operation that exists to be refused
    /// (`Cache::XGroup`), or one a newer server added ahead of this crate.
    /// Nothing here validates the shape, so a wrong one is refused by the
    /// server rather than by this crate.
    ///
    /// ```no_run
    /// # fn main() -> tricoredb::Result<()> {
    /// # let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
    /// use serde_json::json;
    /// let response = db.request(json!({"Cache": "Ping"}))?;
    /// # let _ = response; Ok(()) }
    /// ```
    pub fn request(&mut self, op: impl Into<Value>) -> Result<Response> {
        self.send(op.into())
    }

    pub(crate) fn send(&mut self, op: Value) -> Result<Response> {
        self.request_counter += 1;
        let request_id = format!("rs-{}-{}", self.request_id_prefix, self.request_counter);
        self.last_request_id = Some(request_id.clone());

        let mut envelope = Map::new();
        envelope.insert("request_id".into(), Value::String(request_id));
        envelope.insert("database".into(), Value::String(self.database.clone()));
        envelope.insert("op".into(), op);
        if let Some(ms) = self.request_timeout_ms {
            envelope.insert("options".into(), json!({"timeout_ms": ms}));
        }

        let frame = self.exchange(Tag::Request, Some(&Value::Object(envelope)))?;
        match frame.tag {
            Tag::Response => {}
            Tag::Error => {
                return Err(Error::new(ErrorKind::Refused, error_text(&frame.payload))
                    .with_code_opt(error_code(&frame.payload)))
            }
            other => {
                return Err(
                    self.poison(Error::protocol(format!("expected RESPONSE, got {other:?}")))
                )
            }
        }

        let wire: WireResponse = parse_body(&frame.payload, "RESPONSE")?;
        let diagnostics = wire.diagnostics.unwrap_or_default();
        let (kind, data) = decode_variant(wire.data);
        let response = Response {
            request_id: wire.request_id,
            status: wire.status,
            kind,
            data,
            route: diagnostics.route,
            elapsed_ms: diagnostics.elapsed_ms,
            warnings: diagnostics.warnings,
            error_code: diagnostics.error_code,
            leader_hint: diagnostics.leader_hint,
        };

        // Anything but `ok` means the operation did not happen, and must not
        // reach a caller wearing a success's clothes. Compared against `ok`
        // rather than a list of failures, so a status added later fails closed.
        if response.status != "ok" {
            let mut error = Error {
                kind: ErrorKind::Server,
                code: response.error_code.clone(),
                message: response.failure_message(),
                leader_hint: response.leader_hint.clone(),
            };
            if error.code.is_none() {
                error.code = None;
            }
            return Err(error);
        }
        Ok(response)
    }

    /// Write one frame and read its reply.
    fn exchange(&mut self, tag: Tag, payload: Option<&Value>) -> Result<Frame> {
        if let Some(fault) = &self.fault {
            return Err(Error::new(
                ErrorKind::Closed,
                format!(
                    "this connection was dropped after an earlier failure and cannot be reused: {}",
                    fault.message
                ),
            ));
        }
        if self.closed {
            return Err(Error::new(ErrorKind::Closed, "this connection is closed"));
        }

        let body = match payload {
            Some(value) => serde_json::to_vec(value)
                .map_err(|e| Error::invalid(format!("cannot encode the request: {e}")))?,
            None => Vec::new(),
        };

        // A frame refused before a byte was written leaves the stream intact,
        // so only a failed write poisons the connection.
        frame::encode(tag, &body)?;
        if let Err(e) = frame::write_frame(&mut self.transport, tag, &body) {
            return Err(self.poison(e));
        }

        match frame::read_frame(&mut self.transport) {
            Ok(frame) => Ok(frame),
            // Past this point the reply may still arrive, and reusing the
            // socket would read it as the answer to the next request.
            Err(e) => Err(self.poison(e)),
        }
    }

    /// Record `error` as fatal, drop the socket, and hand the error back.
    fn poison(&mut self, error: Error) -> Error {
        if self.fault.is_none() {
            self.transport.shutdown();
            self.txn_open = false;
            self.fault = Some(error.clone());
        }
        error
    }

    /// End the session politely and release the socket.
    ///
    /// A failure during the goodbye is not reported: the socket closes either
    /// way. Dropping a [`Client`] closes it too, without the goodbye.
    pub fn close(mut self) -> Result<()> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.txn_open = false;
        if self.fault.is_some() {
            self.transport.shutdown();
            return Ok(());
        }
        let _ = self.transport.set_read_timeout(Some(CLOSE_TIMEOUT));
        let _ = self.transport.set_write_timeout(Some(CLOSE_TIMEOUT));
        let _ = frame::write_frame(&mut self.transport, Tag::Close, &[]);
        // Best-effort BYE; a server that says nothing must not hold up a
        // caller's cleanup.
        let _ = frame::read_frame(&mut self.transport);
        self.transport.shutdown();
        Ok(())
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.close_inner();
    }
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("database", &self.database)
            .field("session_id", &self.session_id)
            .field("granted_features", &self.granted_features)
            .field("in_transaction", &self.in_transaction())
            .field("poisoned", &self.is_poisoned())
            .finish()
    }
}

fn parse_body<T: serde::de::DeserializeOwned>(payload: &[u8], what: &str) -> Result<T> {
    serde_json::from_slice(payload)
        .map_err(|e| Error::protocol(format!("malformed {what} body: {e}")))
}

/// The human-readable text of an `ERROR` frame.
fn error_text(payload: &[u8]) -> String {
    if payload.is_empty() {
        return "the server refused the connection without saying why".to_string();
    }
    let Ok(value) = serde_json::from_slice::<Value>(payload) else {
        return String::from_utf8_lossy(payload).into_owned();
    };
    for key in ["message", "error"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    value.to_string()
}

/// The machine-readable code of an `ERROR` frame, when it carries one.
fn error_code(payload: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(payload).ok()?;
    value
        .get("code")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
}

/// A prefix that makes this connection's request ids unique among every other
/// connection the same principal holds.
///
/// The server's cancel registry is keyed by request id **scoped to the
/// principal**, and it stops every entry that matches. With a bare per-connection
/// counter, a pool's connections all issue a `-1`, and one cancel would stop all
/// of them. Uniqueness is the requirement, not unguessability: the id is
/// guessable by construction and every lookup is already scoped to the principal.
fn next_connection_prefix() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!(
        "{:x}{:x}{:x}",
        std::process::id(),
        stamp,
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_prefix_is_unique_per_connection() {
        let a = next_connection_prefix();
        let b = next_connection_prefix();
        assert_ne!(a, b);
    }

    #[test]
    fn error_frames_are_read_for_message_and_code() {
        assert_eq!(error_text(br#"{"message":"nope"}"#), "nope");
        assert_eq!(error_text(br#"{"error":"nope"}"#), "nope");
        assert_eq!(
            error_code(br#"{"message":"x","code":"frame_too_large"}"#),
            Some("frame_too_large".to_string())
        );
        assert_eq!(error_code(br#"{"message":"x"}"#), None);
        assert!(!error_text(b"").is_empty());
    }
}
