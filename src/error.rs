//! The error type every fallible call returns.

use std::fmt;
use std::io;

/// The error code a Raft follower attaches when it cannot serve a request.
pub const NOT_LEADER: &str = "not_leader";

/// Result alias used throughout the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What category of failure an [`Error`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// A socket read, write or connect failed.
    Io,
    /// A client-side deadline elapsed. The connection is closed afterwards,
    /// because the reply may still arrive and would be read as the next answer.
    Timeout,
    /// The peer sent something this client cannot parse or did not expect.
    Protocol,
    /// Authentication was refused (an `AUTH_OK` frame with `ok: false`, or an
    /// `ERROR` frame in answer to `AUTH`).
    Auth,
    /// The server refused the handshake (`HELLO_OK` with `ok: false`).
    Handshake,
    /// The server answered a request with `status: "error"` or
    /// `status: "not_implemented"`. The connection remains usable.
    Server,
    /// The server sent a connection-level `ERROR` frame.
    Refused,
    /// The operation needs a protocol feature the server did not grant.
    FeatureNotGranted,
    /// An argument was refused before anything was sent.
    InvalidArgument,
    /// TLS configuration or handshake failed.
    Tls,
    /// The connection was already closed.
    Closed,
    /// A pooled connection was not available in time, or the pool is closed.
    Pool,
    /// A pooled callback returned with a session transaction still open.
    TransactionLeftOpen,
}

/// Any failure from the transport, the protocol or the server.
///
/// Branch on [`Error::kind`] and [`Error::code`], never on the message text.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Error {
    /// The failure category.
    pub kind: ErrorKind,
    /// The server's stable machine-readable code when it sent one:
    /// `diagnostics.error_code` on a response, or `code` on an `ERROR` frame.
    pub code: Option<String>,
    /// Human-readable description.
    pub message: String,
    /// On a `not_leader` refusal, the leader's client-facing `host:port` when
    /// the cluster knows one. Absent means wait and retry.
    pub leader_hint: Option<String>,
}

impl Error {
    /// Build an error with no code.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            code: None,
            message: message.into(),
            leader_hint: None,
        }
    }

    /// Attach a machine-readable code.
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub(crate) fn from_io(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Error::new(
                ErrorKind::Timeout,
                format!("no reply within the read deadline: {e}"),
            ),
            io::ErrorKind::UnexpectedEof => Error::new(
                ErrorKind::Io,
                "the server closed the connection mid-frame".to_string(),
            ),
            _ => Error::new(ErrorKind::Io, e.to_string()),
        }
    }

    /// Attach a machine-readable code when the peer sent one.
    pub(crate) fn with_code_opt(mut self, code: Option<String>) -> Self {
        self.code = code;
        self
    }

    /// An operation refused here because the server did not grant the
    /// capability it needs. Nothing was sent.
    pub(crate) fn feature_refusal(message: impl Into<String>) -> Self {
        Error::new(ErrorKind::FeatureNotGranted, message).with_code("feature_not_granted")
    }

    /// Whether this client refused the call itself, so no request reached the
    /// server and nothing about the session changed.
    pub(crate) fn was_refused_locally(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::InvalidArgument | ErrorKind::FeatureNotGranted
        )
    }

    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Error::new(ErrorKind::Protocol, message)
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Error::new(ErrorKind::InvalidArgument, message)
    }

    /// The failure category.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The server's stable code, if any.
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// The leader address hint, if any.
    pub fn leader_hint(&self) -> Option<&str> {
        self.leader_hint.as_deref()
    }

    /// The human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// True for every `not_leader` refusal, whether or not a hint is present.
    ///
    /// This client never follows the hint on its own: the address may be
    /// unreachable from here, a new connection must authenticate again, and a
    /// session transaction cannot move to another node.
    pub fn is_redirect(&self) -> bool {
        self.code.as_deref() == Some(NOT_LEADER)
    }

    /// Whether the connection that produced this error can no longer be used.
    pub fn is_connection_fatal(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::Io
                | ErrorKind::Timeout
                | ErrorKind::Protocol
                | ErrorKind::Refused
                | ErrorKind::Closed
                | ErrorKind::Tls
                | ErrorKind::Handshake
        )
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .field("leader_hint", &self.leader_hint)
            .finish()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.code {
            Some(code) => write!(f, "{:?} [{code}]: {}", self.kind, self.message)?,
            None => write!(f, "{:?}: {}", self.kind, self.message)?,
        }
        if let Some(hint) = &self.leader_hint {
            write!(f, " (leader at {hint})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::from_io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_not_leader_code_is_a_redirect_with_or_without_a_hint() {
        let mut e = Error::new(ErrorKind::Server, "x").with_code(NOT_LEADER);
        assert!(e.is_redirect());
        assert_eq!(e.leader_hint(), None);
        e.leader_hint = Some("10.0.0.2:8427".into());
        assert!(e.is_redirect());
        assert_eq!(e.leader_hint(), Some("10.0.0.2:8427"));
        assert!(e.to_string().contains("leader at 10.0.0.2:8427"));
    }

    #[test]
    fn other_codes_are_not_redirects() {
        assert!(!Error::new(ErrorKind::Server, "x")
            .with_code("request.invalid")
            .is_redirect());
        assert!(!Error::new(ErrorKind::Io, "x").is_redirect());
    }

    #[test]
    fn a_server_refusal_keeps_the_connection_usable() {
        assert!(!Error::new(ErrorKind::Server, "bad sql").is_connection_fatal());
        assert!(!Error::new(ErrorKind::FeatureNotGranted, "x").is_connection_fatal());
        assert!(Error::new(ErrorKind::Timeout, "x").is_connection_fatal());
        assert!(Error::new(ErrorKind::Refused, "x").is_connection_fatal());
    }

    #[test]
    fn io_timeouts_map_to_timeout() {
        let e = Error::from_io(io::Error::new(io::ErrorKind::TimedOut, "t"));
        assert_eq!(e.kind, ErrorKind::Timeout);
        let e = Error::from_io(io::Error::new(io::ErrorKind::WouldBlock, "t"));
        assert_eq!(e.kind, ErrorKind::Timeout);
        let e = Error::from_io(io::Error::new(io::ErrorKind::ConnectionReset, "r"));
        assert_eq!(e.kind, ErrorKind::Io);
    }

    #[test]
    fn a_missing_feature_is_named_and_carries_a_code() {
        let e = Error::feature_refusal("begin() needs SESSION_TXN");
        assert_eq!(e.kind, ErrorKind::FeatureNotGranted);
        assert_eq!(e.code(), Some("feature_not_granted"));
        assert!(e.was_refused_locally());
        assert!(!Error::new(ErrorKind::Server, "x").was_refused_locally());
    }
}
