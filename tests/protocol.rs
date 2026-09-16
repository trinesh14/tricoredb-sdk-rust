//! How this client reads the protocol, proved against scripted peers rather
//! than a server.
//!
//! A real server cannot be made to answer `not_leader` on demand, nor to hang
//! up mid-frame, nor to declare a payload it does not send. A peer that speaks
//! the handshake and then plays one scripted answer can — and what is under
//! test is the client's reading of that answer, which is the same shape a real
//! cluster sends. These tests need no server binary and always run.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use tricoredb::{params, Client, ErrorKind, Options};

const TAG_REQUEST: u8 = 2;
const TAG_RESPONSE: u8 = 3;
const TAG_ERROR: u8 = 6;
const TAG_HELLO_OK: u8 = 8;
const TAG_AUTH_OK: u8 = 9;

fn write_frame(stream: &mut TcpStream, tag: u8, body: &str) -> std::io::Result<()> {
    let mut frame = vec![1u8, tag];
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body.as_bytes());
    stream.write_all(&frame)?;
    stream.flush()
}

fn read_frame(stream: &mut TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 6];
    stream.read_exact(&mut header)?;
    let length = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;
    let mut body = vec![0u8; length];
    if length > 0 {
        stream.read_exact(&mut body)?;
    }
    Ok((header[1], body))
}

/// A peer that plays `script` once a client has connected, and the options to
/// reach it. The listener lives until the returned handle is dropped.
struct Peer {
    options: Options,
    _thread: thread::JoinHandle<()>,
}

fn peer(script: impl FnOnce(&mut TcpStream) + Send + 'static) -> Peer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            script(&mut stream);
        }
    });
    Peer {
        options: Options::new("127.0.0.1", port)
            .user("admin")
            .secret("pw")
            .connect_timeout(Duration::from_secs(5)),
        _thread: handle,
    }
}

/// Complete the handshake as a healthy server would, granting every capability.
fn handshake(stream: &mut TcpStream) {
    let _ = read_frame(stream); // HELLO
    let _ = write_frame(
        stream,
        TAG_HELLO_OK,
        r#"{"ok":true,"server_version":{"major":1,"minor":0},"message":"ok","features":7}"#,
    );
    let _ = read_frame(stream); // AUTH
    let _ = write_frame(stream, TAG_AUTH_OK, r#"{"ok":true,"session_id":"s-1"}"#);
}

/// Answer the first request with `response`, then wait for the client to hang
/// up so the reply is not lost to a close race.
fn answer_one(stream: &mut TcpStream, response: &str) {
    let _ = read_frame(stream);
    let _ = write_frame(stream, TAG_RESPONSE, response);
    let _ = read_frame(stream);
}

#[test]
fn a_not_leader_refusal_is_typed_and_carries_the_leader_address() {
    let peer = peer(|stream| {
        handshake(stream);
        answer_one(
            stream,
            r#"{"request_id":"r1","status":"error","data":{"Message":"not the raft leader — send writes to `n2`"},"diagnostics":{"error_code":"not_leader","leader_hint":"10.9.9.7:8427"}}"#,
        );
    });

    let mut db = Client::connect(&peer.options).expect("connect");
    let error = db
        .execute_params("INSERT INTO t VALUES (?)", &params![1])
        .expect_err("a follower refuses the write");

    assert_eq!(error.kind, ErrorKind::Server);
    assert_eq!(error.code(), Some("not_leader"));
    assert!(
        error.is_redirect(),
        "the code, not the message, decides this"
    );
    assert_eq!(error.leader_hint(), Some("10.9.9.7:8427"));
    assert!(!error.is_connection_fatal(), "the connection still works");
    assert!(error.to_string().contains("10.9.9.7:8427"), "{error}");
}

#[test]
fn a_mid_election_refusal_has_a_code_but_no_address() {
    let peer = peer(|stream| {
        handshake(stream);
        answer_one(
            stream,
            r#"{"request_id":"r1","status":"error","data":{"Message":"not the raft leader"},"diagnostics":{"error_code":"not_leader"}}"#,
        );
    });

    let mut db = Client::connect(&peer.options).expect("connect");
    let error = db.execute("INSERT INTO t VALUES (1)").expect_err("refused");
    assert!(error.is_redirect());
    assert_eq!(
        error.leader_hint(),
        None,
        "an absent hint means the destination is unknown, not that there was no redirect"
    );
}

#[test]
fn an_ordinary_failure_is_not_read_as_a_redirect() {
    let peer = peer(|stream| {
        handshake(stream);
        answer_one(
            stream,
            r#"{"request_id":"r1","status":"error","data":{"Message":"syntax error"},"diagnostics":{"error_code":"request.invalid"}}"#,
        );
    });

    let mut db = Client::connect(&peer.options).expect("connect");
    let error = db.execute("NOT SQL").expect_err("refused");
    assert!(!error.is_redirect());
    assert_eq!(error.code(), Some("request.invalid"));
    assert_eq!(error.message(), "syntax error");
}

#[test]
fn a_handshake_refusal_is_reported_as_one() {
    let peer = peer(|stream| {
        let _ = read_frame(stream);
        let _ = write_frame(
            stream,
            TAG_HELLO_OK,
            r#"{"ok":false,"message":"unsupported protocol version","code":"handshake.version"}"#,
        );
        let _ = read_frame(stream);
    });

    let error = Client::connect(&peer.options).expect_err("the handshake was refused");
    assert_eq!(error.kind, ErrorKind::Handshake);
    assert!(error.message().contains("unsupported protocol"), "{error}");
}

#[test]
fn an_auth_ok_frame_carrying_ok_false_is_still_a_refusal() {
    let peer = peer(|stream| {
        let _ = read_frame(stream);
        let _ = write_frame(
            stream,
            TAG_HELLO_OK,
            r#"{"ok":true,"message":"ok","features":7}"#,
        );
        let _ = read_frame(stream);
        // The tag says AUTH_OK; the body says no. The body is the verdict.
        let _ = write_frame(
            stream,
            TAG_AUTH_OK,
            r#"{"ok":false,"message":"bad password"}"#,
        );
        let _ = read_frame(stream);
    });

    let error = Client::connect(&peer.options).expect_err("authentication was refused");
    assert_eq!(error.kind, ErrorKind::Auth);
    assert_eq!(error.message(), "bad password");
}

#[test]
fn a_connection_level_error_frame_is_refused_with_its_code() {
    let peer = peer(|stream| {
        handshake(stream);
        let _ = read_frame(stream);
        let _ = write_frame(
            stream,
            TAG_ERROR,
            r#"{"message":"frame too large","code":"frame_too_large"}"#,
        );
        let _ = read_frame(stream);
    });

    let mut db = Client::connect(&peer.options).expect("connect");
    let error = db
        .execute("SELECT 1")
        .expect_err("the server refused the frame");
    assert_eq!(error.kind, ErrorKind::Refused);
    assert_eq!(error.code(), Some("frame_too_large"));
    assert!(error.is_connection_fatal());
}

#[test]
fn a_declared_payload_above_the_ceiling_is_refused_before_it_is_read() {
    let peer = peer(|stream| {
        handshake(stream);
        let _ = read_frame(stream);
        // A control frame claiming 64 KiB + 1 bytes, with none of them sent. A
        // client that trusted the length would allocate and then block forever.
        let mut header = vec![1u8, TAG_AUTH_OK];
        header.extend_from_slice(&((64 * 1024u32) + 1).to_be_bytes());
        let _ = stream.write_all(&header);
        let _ = stream.flush();
        let _ = read_frame(stream);
    });

    let mut db = Client::connect(&peer.options).expect("connect");
    let error = db.ping().expect_err("the frame is refused");
    assert_eq!(error.kind, ErrorKind::Protocol, "{error}");
    assert!(error.message().contains("limit"), "{error}");
    assert!(db.is_poisoned(), "the stream can no longer be trusted");
}

#[test]
fn a_frame_version_this_client_cannot_read_is_refused() {
    let peer = peer(|stream| {
        handshake(stream);
        let _ = read_frame(stream);
        let _ = stream.write_all(&[2u8, TAG_RESPONSE, 0, 0, 0, 0]);
        let _ = stream.flush();
        let _ = read_frame(stream);
    });

    let mut db = Client::connect(&peer.options).expect("connect");
    let error = db.ping().expect_err("a newer frame layout is refused");
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert!(error.message().contains("version 2"), "{error}");
}

#[test]
fn a_peer_that_hangs_up_mid_frame_poisons_the_connection() {
    let peer = peer(|stream| {
        handshake(stream);
        let _ = read_frame(stream);
        // Six bytes of header promising ten bytes of body, then nothing.
        let _ = stream.write_all(&[1u8, TAG_RESPONSE, 0, 0, 0, 10, b'{']);
        let _ = stream.flush();
        // Dropping the stream closes it.
    });

    let mut db = Client::connect(&peer.options).expect("connect");
    let error = db.execute("SELECT 1").expect_err("the peer went away");
    assert!(
        matches!(error.kind, ErrorKind::Io | ErrorKind::Protocol),
        "{error}"
    );
    assert!(db.is_poisoned());

    // Every later call fails the same way rather than reading stale bytes.
    let again = db.ping().expect_err("a poisoned connection stays refused");
    assert_eq!(again.kind, ErrorKind::Closed, "{again}");
}

#[test]
fn a_reply_that_never_arrives_ends_at_the_read_timeout() {
    let peer = peer(|stream| {
        handshake(stream);
        let _ = read_frame(stream);
        // Answer nothing at all, but hold the socket open.
        thread::sleep(Duration::from_secs(3));
    });

    let mut db = Client::connect(&peer.options).expect("connect");
    db.set_read_timeout(Some(Duration::from_millis(150)))
        .expect("set the read timeout");
    let started = std::time::Instant::now();
    let error = db.execute("SELECT 1").expect_err("no reply came");
    assert_eq!(error.kind, ErrorKind::Timeout, "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "it did not wait"
    );
    assert!(
        db.is_poisoned(),
        "the reply may still arrive, so the socket cannot be reused"
    );
}

#[test]
fn a_status_this_client_does_not_know_is_treated_as_a_failure() {
    let peer = peer(|stream| {
        handshake(stream);
        answer_one(
            stream,
            r#"{"request_id":"r1","status":"not_implemented","data":{"Message":"Cache::XGroup is refused in V1"}}"#,
        );
    });

    let mut db = Client::connect(&peer.options).expect("connect");
    let error = db
        .request(serde_json::json!({"Cache": {"XGroup": {}}}))
        .expect_err("anything but ok is a failure");
    assert_eq!(error.kind, ErrorKind::Server);
    assert!(error.message().contains("XGroup"), "{error}");
}

#[test]
fn warnings_reach_the_caller_on_a_successful_response() {
    let peer = peer(|stream| {
        handshake(stream);
        answer_one(
            stream,
            r#"{"request_id":"r1","status":"ok","data":{"Message":"done"},"diagnostics":{"route":"local","elapsed_ms":4,"warnings":["shard 2 was unreachable"]}}"#,
        );
    });

    let mut db = Client::connect(&peer.options).expect("connect");
    let response = db.execute("CREATE TABLE t (id INT)").expect("ok");
    assert_eq!(response.status, "ok");
    assert_eq!(response.warnings, vec!["shard 2 was unreachable"]);
    assert_eq!(response.route.as_deref(), Some("local"));
    assert_eq!(response.elapsed_ms, Some(4));
}

#[test]
fn a_server_that_never_negotiated_capabilities_grants_none() {
    let peer = peer(|stream| {
        // An older server omits the features field entirely.
        let _ = read_frame(stream);
        let _ = write_frame(stream, TAG_HELLO_OK, r#"{"ok":true,"message":"ok"}"#);
        let _ = read_frame(stream);
        let _ = write_frame(stream, TAG_AUTH_OK, r#"{"ok":true,"session_id":"s-1"}"#);
        let _ = read_frame(stream);
    });

    let mut db = Client::connect(&peer.options).expect("connect");
    assert_eq!(db.granted_features(), 0);
    assert!(!db.server_params_granted());
    assert!(!db.session_txn_granted());

    let error = db
        .query_params("SELECT * FROM t WHERE id = ?", &params![1])
        .expect_err("binding needs the capability");
    assert_eq!(error.kind, ErrorKind::FeatureNotGranted);
    assert!(!db.is_poisoned(), "nothing was sent");
}

#[test]
fn the_request_envelope_carries_the_database_and_a_unique_id() {
    let peer = peer(|stream| {
        handshake(stream);
        let (tag, body) = read_frame(stream).expect("a request");
        assert_eq!(tag, TAG_REQUEST);
        let envelope: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(envelope["database"], "reporting");
        assert!(envelope["request_id"].as_str().unwrap().starts_with("rs-"));
        assert_eq!(envelope["op"]["Sql"]["Query"]["sql"], "SELECT 1");
        assert_eq!(envelope["options"]["timeout_ms"], 2500);
        let _ = write_frame(
            stream,
            TAG_RESPONSE,
            r#"{"request_id":"r1","status":"ok","data":{"Rows":{"columns":["n"],"rows":[["1"]]}}}"#,
        );
        let _ = read_frame(stream);
    });

    let mut options = peer.options.clone();
    options.database = "reporting".to_string();
    let mut db = Client::connect(&options).expect("connect");
    db.set_request_timeout(Some(Duration::from_millis(2500)));
    let rows = db.query("SELECT 1").expect("query");
    assert_eq!(rows.rows[0][0], "1");
}
