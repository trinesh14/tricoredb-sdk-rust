//! Following the leader: what the client does with `not_leader` by default.
//!
//! Scripted peers stand in for a follower and a leader. A real follower's
//! refusal was applied nowhere, which is what makes sending the request again
//! safe. These tests need no server binary.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::Value;
use tricoredb::{Client, Options, Pool, NOT_LEADER, OUTCOME_UNKNOWN};

const HELLO: u8 = 0;
const AUTH: u8 = 1;
const REQUEST: u8 = 2;
const RESPONSE: u8 = 3;
const CLOSE: u8 = 7;
const HELLO_OK: u8 = 8;
const AUTH_OK: u8 = 9;
const BYE: u8 = 10;

fn write_frame(stream: &mut TcpStream, tag: u8, body: &str) -> std::io::Result<()> {
    let mut frame = vec![1u8, tag];
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body.as_bytes());
    stream.write_all(&frame)
}

fn read_frame(stream: &mut TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 6];
    stream.read_exact(&mut header)?;
    let length = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body)?;
    Ok((header[1], body))
}

#[derive(Default)]
struct Seen {
    connections: usize,
    users: Vec<String>,
    request_ids: Vec<String>,
}

/// A peer serving any number of connections; `reply(request_id, n)` answers
/// the n-th request it has seen.
struct Peer {
    port: u16,
    seen: Arc<Mutex<Seen>>,
}

impl Peer {
    fn start(reply: impl Fn(&str, usize) -> String + Send + Sync + 'static) -> Peer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let reply = Arc::new(reply);
        let shared = seen.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                shared.lock().unwrap().connections += 1;
                let (seen, reply) = (shared.clone(), reply.clone());
                thread::spawn(move || serve(&mut stream, &seen, &*reply));
            }
        });
        Peer { port, seen }
    }

    fn address(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    fn options(&self) -> Options {
        Options::new("127.0.0.1", self.port)
            .user("u")
            .secret("s")
            .connect_timeout(Duration::from_secs(5))
            .leader_retry_delay(Duration::from_millis(1))
    }

    fn connections(&self) -> usize {
        self.seen.lock().unwrap().connections
    }

    fn requests(&self) -> usize {
        self.seen.lock().unwrap().request_ids.len()
    }
}

fn serve(stream: &mut TcpStream, seen: &Mutex<Seen>, reply: &dyn Fn(&str, usize) -> String) {
    while let Ok((tag, body)) = read_frame(stream) {
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let result = match tag {
            HELLO => write_frame(stream, HELLO_OK, r#"{"ok":true,"features":7}"#),
            AUTH => {
                let user = body["username"].as_str().unwrap_or_default().to_string();
                seen.lock().unwrap().users.push(user);
                write_frame(stream, AUTH_OK, r#"{"ok":true,"session_id":"s-1"}"#)
            }
            CLOSE => {
                let _ = write_frame(stream, BYE, "{}");
                return;
            }
            REQUEST => {
                let rid = body["request_id"].as_str().unwrap_or_default().to_string();
                let n = {
                    let mut seen = seen.lock().unwrap();
                    seen.request_ids.push(rid.clone());
                    seen.request_ids.len()
                };
                write_frame(stream, RESPONSE, &reply(&rid, n))
            }
            _ => Ok(()),
        };
        if result.is_err() {
            return;
        }
    }
}

fn ok(rid: &str) -> String {
    format!(
        r#"{{"request_id":"{rid}","status":"ok","data":{{"Json":{{"transaction":"began"}}}},"diagnostics":{{}}}}"#
    )
}

fn refusal(rid: &str, code: &str, leader: Option<&str>) -> String {
    let hint = leader.map_or(String::new(), |l| format!(r#","leader_hint":"{l}""#));
    format!(
        r#"{{"request_id":"{rid}","status":"error","data":{{"Message":"not the raft leader"}},"diagnostics":{{"error_code":"{code}"{hint}}}}}"#
    )
}

#[test]
fn a_refused_request_goes_to_the_leader_and_stays_there() {
    let leader = Peer::start(|rid, _| ok(rid));
    let hint = leader.address();
    let follower = Peer::start(move |rid, _| refusal(rid, NOT_LEADER, Some(&hint)));

    let mut db = Client::connect(&follower.options()).expect("connect");
    db.execute("INSERT INTO t VALUES (1)")
        .expect("served by the leader");
    assert_eq!(db.address(), leader.address());
    db.execute("INSERT INTO t VALUES (2)")
        .expect("served by the leader");

    assert_eq!(follower.requests(), 1);
    assert_eq!(leader.requests(), 2);
    assert_eq!(leader.connections(), 1);
    let leader_seen = leader.seen.lock().unwrap();
    assert_eq!(leader_seen.users, ["u"], "authenticated as before");
    let follower_seen = follower.seen.lock().unwrap();
    assert_eq!(
        leader_seen.request_ids[0], follower_seen.request_ids[0],
        "the same request, not a new one"
    );
}

#[test]
fn with_no_leader_named_the_client_waits_and_asks_again() {
    let peer = Peer::start(|rid, n| {
        if n < 3 {
            refusal(rid, NOT_LEADER, None)
        } else {
            ok(rid)
        }
    });
    let mut db = Client::connect(&peer.options()).expect("connect");
    db.execute("INSERT INTO t VALUES (1)")
        .expect("served on the third try");
    assert_eq!((peer.connections(), peer.requests()), (1, 3));
}

#[test]
fn retries_are_bounded_and_reported() {
    let peer = Peer::start(|rid, _| refusal(rid, NOT_LEADER, None));
    let mut db = Client::connect(&peer.options().leader_retries(2)).expect("connect");
    let error = db
        .execute("INSERT INTO t VALUES (1)")
        .expect_err("never served");
    assert!(error.is_redirect());
    assert!(error.message().contains("sent again 2 times"), "{error}");
    assert_eq!(peer.requests(), 3);
}

#[test]
fn an_unreachable_leader_leaves_the_connection_where_it_was() {
    let nowhere = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().to_string()
    };
    let peer = Peer::start(move |rid, n| {
        if n == 1 {
            refusal(rid, NOT_LEADER, Some(&nowhere))
        } else {
            ok(rid)
        }
    });
    let mut db = Client::connect(&peer.options()).expect("connect");
    db.execute("INSERT INTO t VALUES (1)")
        .expect("served on the second try");
    assert_eq!(db.address(), peer.address());
    assert_eq!(peer.requests(), 2);
}

#[test]
fn nothing_is_sent_again_inside_a_session_transaction() {
    let peer = Peer::start(|rid, n| {
        if n == 1 {
            ok(rid)
        } else {
            refusal(rid, NOT_LEADER, None)
        }
    });
    let mut db = Client::connect(&peer.options()).expect("connect");
    db.begin().expect("begin");
    let error = db.execute("INSERT INTO t VALUES (1)").expect_err("refused");
    assert!(error.is_redirect());
    assert_eq!(peer.requests(), 2);
}

#[test]
fn outcome_unknown_is_never_sent_again() {
    let peer = Peer::start(|rid, _| refusal(rid, OUTCOME_UNKNOWN, None));
    let mut db = Client::connect(&peer.options()).expect("connect");
    let error = db.execute("INSERT INTO t VALUES (1)").expect_err("unknown");
    assert_eq!(error.code(), Some(OUTCOME_UNKNOWN));
    assert!(!error.is_redirect());
    assert_eq!(peer.requests(), 1);
}

#[test]
fn follow_leader_off_returns_the_first_refusal() {
    let leader = Peer::start(|rid, _| ok(rid));
    let hint = leader.address();
    let follower = Peer::start(move |rid, _| refusal(rid, NOT_LEADER, Some(&hint)));
    let mut db = Client::connect(&follower.options().follow_leader(false)).expect("connect");
    let error = db.execute("INSERT INTO t VALUES (1)").expect_err("refused");
    assert_eq!(error.leader_hint(), Some(leader.address().as_str()));
    assert_eq!(leader.connections(), 0);
}

#[test]
fn pooled_connections_follow_the_leader() {
    let leader = Peer::start(|rid, _| ok(rid));
    let hint = leader.address();
    let follower = Peer::start(move |rid, _| refusal(rid, NOT_LEADER, Some(&hint)));
    let pool = Pool::new(follower.options(), 2).expect("pool");
    for i in 0..2 {
        pool.with_connection(|db| {
            db.execute(&format!("INSERT INTO t VALUES ({i})"))
                .map(|_| ())
        })
        .expect("served by the leader");
    }
    assert_eq!(follower.requests(), 1);
    assert_eq!(leader.requests(), 2);
}
