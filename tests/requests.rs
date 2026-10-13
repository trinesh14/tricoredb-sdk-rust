//! The exact request each 0.3.0 method sends, and how it reads the answer.
//!
//! A scripted peer completes the handshake, records the one request it is
//! sent and plays back a canned answer. The answers are the payloads a
//! 0.1.0-rc.3 server returned for the same operation, so each check proves two
//! things: the operation is spelled the way the server spells it, and the
//! server's real answer decodes. No server binary is needed.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{json, Map, Value};
use tricoredb::{
    BitOpKind, BulkOp, CacheCommand, CacheEntry, CacheExpiry, CacheReply, CentralityMeasure,
    CentralityOptions, Client, CollectionOptions, CommunityOptions, ComponentKind,
    ComponentOptions, ConstraintKind, ConstraintTarget, DocumentFilter, DocumentSchema,
    DocumentUpdate, ErrorKind, ExplainOptions, FieldRule, FieldType, FreshState, FusionMethod,
    GraphDirection, GraphEdgeInput, GraphNodeInput, KPathOptions, ListEnd, NodeUpdate, Options,
    PageRankOptions, RecallOptions, ReplaceOptions, Result, ReturnDocument, ScoreBound,
    ScoredMember, SetCondition, SimilarityMetric, SimilarityOptions, SortKey, TextUpsert,
    TraverseOptions, VectorConfigChange, VectorExample, VectorMetric, VectorSearchOptions,
    VectorUpsert,
};

const TAG_RESPONSE: u8 = 3;
const TAG_HELLO_OK: u8 = 8;
const TAG_AUTH_OK: u8 = 9;

fn write_frame(stream: &mut TcpStream, tag: u8, body: &str) -> std::io::Result<()> {
    let mut frame = vec![1u8, tag];
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body.as_bytes());
    stream.write_all(&frame)?;
    stream.flush()
}

fn read_frame(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut header = [0u8; 6];
    stream.read_exact(&mut header)?;
    let length = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body)?;
    Ok(body)
}

/// Run `call` against a peer that answers its one request with `data`, and
/// return the request envelope the peer received beside what the call made of
/// the answer.
fn exchange<T>(data: Value, call: impl FnOnce(&mut Client) -> Result<T>) -> (Value, Result<T>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (sender, receiver) = mpsc::channel();
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let _ = read_frame(&mut stream); // HELLO
        let _ = write_frame(
            &mut stream,
            TAG_HELLO_OK,
            r#"{"ok":true,"message":"ok","features":7}"#,
        );
        let _ = read_frame(&mut stream); // AUTH
        let _ = write_frame(
            &mut stream,
            TAG_AUTH_OK,
            r#"{"ok":true,"session_id":"s-1"}"#,
        );
        if let Ok(request) = read_frame(&mut stream) {
            let _ = sender.send(request);
            let reply = json!({"request_id": "r1", "status": "ok", "data": data});
            let _ = write_frame(&mut stream, TAG_RESPONSE, &reply.to_string());
        }
        let _ = read_frame(&mut stream); // CLOSE, or the hang-up
    });
    let mut db = Client::connect(
        &Options::new("127.0.0.1", port)
            .user("admin")
            .secret("pw")
            .connect_timeout(Duration::from_secs(5)),
    )
    .expect("connect");
    let outcome = call(&mut db);
    drop(db);
    let request = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("the peer saw a request");
    peer.join().expect("peer");
    (
        serde_json::from_slice(&request).expect("a JSON request"),
        outcome,
    )
}

/// Assert `call` sends exactly `op`, and return what it decoded from `data`.
fn sends<T>(op: Value, data: Value, call: impl FnOnce(&mut Client) -> Result<T>) -> T {
    let (request, outcome) = exchange(data, call);
    assert_eq!(request["op"], op, "the request's op");
    assert_eq!(request["database"], "main");
    assert!(
        request.get("options").is_none(),
        "no options were asked for: {request}"
    );
    outcome.expect("the server's own answer decodes")
}

fn cache(variant: &str, body: Value) -> Value {
    json!({"Cache": {variant: body}})
}

fn document(variant: &str, body: Value) -> Value {
    json!({"Document": {variant: body}})
}

fn vector(variant: &str, body: Value) -> Value {
    json!({"Vector": {variant: body}})
}

fn graph(variant: &str, body: Value) -> Value {
    json!({"Graph": {variant: body}})
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().expect("an object").clone()
}

// -- cache -------------------------------------------------------------------

#[test]
fn cache_many_key_and_conditional_writes() {
    let got = sends(
        cache("MGet", json!({"namespace": "n", "keys": ["a", "zz", "a"]})),
        json!({"CacheValues": [[1], null, [1]]}),
        |db| db.cache_mget("n", &["a", "zz", "a"]),
    );
    assert_eq!(got, vec![Some(vec![1]), None, Some(vec![1])]);

    let entries = [CacheEntry::new("a", [1u8]), CacheEntry::new("b", [2u8])];
    let got = sends(
        cache(
            "MSet",
            json!({"namespace": "n", "entries": [{"key": "a", "value": [1]}, {"key": "b", "value": [2]}], "ttl_ms": null}),
        ),
        json!({"Json": {"set": 2}}),
        |db| db.cache_mset("n", &entries, None),
    );
    assert_eq!(got, 2);

    let got = sends(
        cache(
            "Warm",
            json!({"namespace": "n", "entries": [{"key": "a", "value": [1]}, {"key": "b", "value": [2]}], "ttl_ms": 5000}),
        ),
        json!({"Json": {"alreadyCached": 1, "written": 1}}),
        |db| db.cache_warm("n", &entries, Some(Duration::from_secs(5))),
    );
    assert_eq!((got.written, got.already_cached), (1, 1));

    let got = sends(
        cache(
            "SetIf",
            json!({"namespace": "n", "key": "k", "value": [5], "condition": "IfPresent", "expiry": {"Set": {"ttl_ms": 60000}}}),
        ),
        json!({"Json": {"set": true}}),
        |db| {
            db.cache_set_if(
                "n",
                "k",
                &[5],
                SetCondition::IfPresent,
                CacheExpiry::Set(Duration::from_secs(60)),
            )
        },
    );
    assert!(got);
    sends(
        cache(
            "SetIf",
            json!({"namespace": "n", "key": "k", "value": [5], "condition": "IfAbsent", "expiry": "Keep"}),
        ),
        json!({"Json": {"set": false}}),
        |db| db.cache_set_if("n", "k", &[5], SetCondition::IfAbsent, CacheExpiry::Keep),
    );
    sends(
        cache(
            "SetIf",
            json!({"namespace": "n", "key": "k", "value": [5], "condition": "Always", "expiry": "Clear"}),
        ),
        json!({"Json": {"set": true}}),
        |db| db.cache_set_if("n", "k", &[5], SetCondition::Always, CacheExpiry::Clear),
    );

    let got = sends(
        cache(
            "CompareAndSwap",
            json!({"namespace": "n", "key": "k", "expected": [1], "value": [2], "ttl_ms": 5000}),
        ),
        json!({"Json": {"current": "hi", "swapped": false}}),
        |db| {
            db.cache_compare_and_swap(
                "n",
                "k",
                Some(&[1]),
                Some(&[2]),
                Some(Duration::from_secs(5)),
            )
        },
    );
    assert!(!got.swapped);
    assert_eq!(got.current.as_deref(), Some(&b"hi"[..]));
    let got = sends(
        cache(
            "CompareAndSwap",
            json!({"namespace": "n", "key": "k", "expected": null, "value": null, "ttl_ms": null}),
        ),
        json!({"Json": {"currentIsBinary": true, "swapped": false}}),
        |db| db.cache_compare_and_swap("n", "k", None, None, None),
    );
    assert!(got.current_is_binary && got.current.is_none());
}

#[test]
fn cache_atomic_reads_and_deadlines() {
    let got = sends(
        cache("GetDel", json!({"namespace": "n", "key": "k"})),
        json!({"CacheValue": [2]}),
        |db| db.cache_get_del("n", "k"),
    );
    assert_eq!(got, Some(vec![2]));

    let got = sends(
        cache(
            "GetSet",
            json!({"namespace": "n", "key": "k", "value": [1], "ttl_ms": null}),
        ),
        json!({"CacheValue": null}),
        |db| db.cache_get_set("n", "k", &[1], None),
    );
    assert_eq!(got, None);

    sends(
        cache(
            "GetEx",
            json!({"namespace": "n", "key": "k", "expiry": "Clear"}),
        ),
        json!({"CacheValue": [2]}),
        |db| db.cache_get_ex("n", "k", CacheExpiry::Clear),
    );

    let got = sends(
        cache(
            "Append",
            json!({"namespace": "n", "key": "k", "value": [1, 2]}),
        ),
        json!({"Json": {"length": 2}}),
        |db| db.cache_append("n", "k", &[1, 2]),
    );
    assert_eq!(got, 2);

    let got = sends(
        cache(
            "ExpireAt",
            json!({"namespace": "n", "key": "k", "at_ms": 4102444800000i64}),
        ),
        json!({"Json": {"deleted": false, "updated": true}}),
        |db| db.cache_expire_at("n", "k", 4102444800000),
    );
    assert!(got.updated && !got.deleted);
}

#[test]
fn cache_stale_while_revalidate() {
    let got = sends(
        cache(
            "SetFresh",
            json!({"namespace": "n", "key": "k", "value": [1, 2], "fresh_ms": 60000, "ttl_ms": null}),
        ),
        json!({"Json": {"freshUntilMs": 1791870307508i64, "set": true}}),
        |db| db.cache_set_fresh("n", "k", &[1, 2], Duration::from_secs(60), None),
    );
    assert_eq!(got, 1791870307508);

    let request = cache(
        "GetFresh",
        json!({"namespace": "n", "key": "k", "owner": "w1", "lease_ms": 1000}),
    );
    let read = |db: &mut Client| db.cache_get_fresh("n", "k", "w1", Duration::from_secs(1));
    let got = sends(
        request.clone(),
        json!({"Json": {"state": "fresh", "value": "hi"}}),
        read,
    );
    assert_eq!(got.state, FreshState::Fresh);
    assert_eq!(got.value.as_deref(), Some(&b"hi"[..]));
    let got = sends(
        request.clone(),
        json!({"Json": {"state": "fresh", "valueHex": "fffe00"}}),
        read,
    );
    assert_eq!(got.value, Some(vec![0xff, 0xfe, 0x00]), "a binary value");
    let got = sends(
        request.clone(),
        json!({"Json": {"refresh": true, "state": "stale", "value": "\t"}}),
        read,
    );
    assert!(got.state == FreshState::Stale && got.refresh);
    let got = sends(
        request.clone(),
        json!({"Json": {"lease": false, "retryAfterMs": 991, "state": "miss"}}),
        read,
    );
    assert!(got.state == FreshState::Miss && !got.lease);
    assert_eq!(got.retry_after_ms, Some(991));
    let got = sends(request, json!({"Json": {"state": "missing"}}), read);
    assert_eq!(got.state, FreshState::Missing);

    sends(
        cache(
            "SetMissing",
            json!({"namespace": "n", "key": "k", "ttl_ms": 60000}),
        ),
        json!({"Json": {"set": true}}),
        |db| db.cache_set_missing("n", "k", Duration::from_secs(60)),
    );
}

#[test]
fn cache_sorted_sets() {
    let got = sends(
        cache(
            "ZAdd",
            json!({"namespace": "n", "key": "z", "members": [{"score": 1.5, "member": [97]}, {"score": 2.0, "member": [98]}]}),
        ),
        json!({"Json": {"added": 2, "updated": 0}}),
        |db| {
            db.cache_zadd(
                "n",
                "z",
                &[ScoredMember::new(1.5, "a"), ScoredMember::new(2.0, "b")],
            )
        },
    );
    assert_eq!((got.added, got.updated), (2, 0));

    let got = sends(
        cache(
            "ZRem",
            json!({"namespace": "n", "key": "z", "members": [[97], [1]]}),
        ),
        json!({"Json": {"removed": 1}}),
        |db| db.cache_zrem("n", "z", &[&[97u8][..], &[1u8][..]]),
    );
    assert_eq!(got, 1);

    let request = cache(
        "ZScore",
        json!({"namespace": "n", "key": "z", "member": [97]}),
    );
    let got = sends(request.clone(), json!({"Json": {"score": 9.0}}), |db| {
        db.cache_zscore("n", "z", b"a")
    });
    assert_eq!(got, Some(9.0));
    let got = sends(request, json!({"Json": {"score": null}}), |db| {
        db.cache_zscore("n", "z", b"a")
    });
    assert_eq!(got, None);

    let got = sends(
        cache("ZCard", json!({"namespace": "n", "key": "z"})),
        json!({"Json": {"cardinality": 3}}),
        |db| db.cache_zcard("n", "z"),
    );
    assert_eq!(got, 3);

    let got = sends(
        cache(
            "ZRank",
            json!({"namespace": "n", "key": "z", "member": [97], "rev": true}),
        ),
        json!({"Json": {"rank": null}}),
        |db| db.cache_zrank("n", "z", b"a", true),
    );
    assert_eq!(got, None);

    let got = sends(
        cache(
            "ZRange",
            json!({"namespace": "n", "key": "z", "start": 0, "stop": -1, "rev": false, "with_scores": false}),
        ),
        json!({"Json": {"count": 2, "members": [[98], [99]]}}),
        |db| db.cache_zrange("n", "z", 0, -1, false),
    );
    assert_eq!(got, vec![vec![98], vec![99]]);
    let got = sends(
        cache(
            "ZRange",
            json!({"namespace": "n", "key": "z", "start": 0, "stop": -1, "rev": true, "with_scores": true}),
        ),
        json!({"Json": {"count": 1, "members": [{"member": [97], "score": 9.0}]}}),
        |db| db.cache_zrange_with_scores("n", "z", 0, -1, true),
    );
    assert_eq!(got, vec![ScoredMember::new(9.0, "a")]);

    let got = sends(
        cache(
            "ZRangeByScore",
            json!({"namespace": "n", "key": "z", "min": {"value": 2.0, "exclusive": true}, "max": {"value": f64::MAX, "exclusive": false}, "rev": false, "with_scores": false, "limit": [0, 5]}),
        ),
        json!({"Json": {"count": 1, "members": [[99]]}}),
        |db| {
            db.cache_zrange_by_score(
                "n",
                "z",
                ScoreBound::exclusive(2.0),
                ScoreBound::highest(),
                false,
                Some((0, 5)),
            )
        },
    );
    assert_eq!(got, vec![vec![99]]);
    let got = sends(
        cache(
            "ZRangeByScore",
            json!({"namespace": "n", "key": "z", "min": {"value": -f64::MAX, "exclusive": false}, "max": {"value": 100.0, "exclusive": false}, "rev": true, "with_scores": true, "limit": null}),
        ),
        json!({"Json": {"count": 1, "members": [{"member": [99], "score": 3.0}]}}),
        |db| {
            db.cache_zrange_by_score_with_scores(
                "n",
                "z",
                ScoreBound::lowest(),
                ScoreBound::inclusive(100.0),
                true,
                None,
            )
        },
    );
    assert_eq!(got[0].score, 3.0);

    let got = sends(
        cache(
            "ZIncrBy",
            json!({"namespace": "n", "key": "z", "member": [98], "by": 2.5}),
        ),
        json!({"Json": {"score": 4.5}}),
        |db| db.cache_zincr_by("n", "z", b"b", 2.5),
    );
    assert_eq!(got, 4.5);
}

#[test]
fn a_score_that_is_not_finite_is_refused_before_it_is_sent() {
    // The peer answers one request only, and that one is the ping at the end:
    // each refusal below is raised here, before anything is written.
    let (_, outcome) = exchange(json!("Empty"), |db| {
        let nan = db.cache_zadd("n", "z", &[ScoredMember::new(f64::NAN, "a")]);
        assert_eq!(nan.unwrap_err().kind, ErrorKind::InvalidArgument);
        let unbounded = db.cache_zrange_by_score(
            "n",
            "z",
            ScoreBound::inclusive(f64::NEG_INFINITY),
            ScoreBound::highest(),
            false,
            None,
        );
        assert_eq!(unbounded.unwrap_err().kind, ErrorKind::InvalidArgument);
        assert_eq!(
            db.cache_zincr_by("n", "z", b"a", f64::INFINITY)
                .unwrap_err()
                .kind,
            ErrorKind::InvalidArgument
        );
        let nothing = db.vector_set_collection_config("c", &VectorConfigChange::default());
        assert_eq!(nothing.unwrap_err().kind, ErrorKind::InvalidArgument);
        // The one request the peer waits for.
        db.cache_ping()
    });
    assert!(outcome.is_ok());
}

#[test]
fn cache_locks() {
    let got = sends(
        cache(
            "LockAcquire",
            json!({"namespace": "n", "key": "lk", "owner": "o1", "ttl_ms": 30000}),
        ),
        json!({"Json": {"acquired": true, "expiresAtMs": 1791870277688i64, "holder": "o1", "token": 1}}),
        |db| db.cache_lock_acquire("n", "lk", "o1", Duration::from_secs(30)),
    );
    assert!(got.acquired);
    assert_eq!(got.token, Some(1));
    assert_eq!(got.holder.as_deref(), Some("o1"));
    assert_eq!(got.expires_at_ms, Some(1791870277688));

    let got = sends(
        cache(
            "LockRenew",
            json!({"namespace": "n", "key": "lk", "owner": "o2", "ttl_ms": 30000}),
        ),
        json!({"Json": {"expiresAtMs": null, "renewed": false, "token": null}}),
        |db| db.cache_lock_renew("n", "lk", "o2", Duration::from_secs(30)),
    );
    assert!(!got.renewed && got.token.is_none());

    let got = sends(
        cache(
            "LockRelease",
            json!({"namespace": "n", "key": "lk", "owner": "o1"}),
        ),
        json!({"Json": {"released": true}}),
        |db| db.cache_lock_release("n", "lk", "o1"),
    );
    assert!(got);
}

#[test]
fn cache_bitmaps_and_hyperloglog() {
    let got = sends(
        cache(
            "SetBit",
            json!({"namespace": "n", "key": "b", "offset": 7, "value": true}),
        ),
        json!({"Json": {"previous": 1}}),
        |db| db.cache_set_bit("n", "b", 7, true),
    );
    assert!(got);
    let got = sends(
        cache("GetBit", json!({"namespace": "n", "key": "b", "offset": 7})),
        json!({"Json": {"bit": 0}}),
        |db| db.cache_get_bit("n", "b", 7),
    );
    assert!(!got);
    let got = sends(
        cache(
            "BitCount",
            json!({"namespace": "n", "key": "b", "range": [0, -1]}),
        ),
        json!({"Json": {"count": 1}}),
        |db| db.cache_bit_count("n", "b", Some((0, -1))),
    );
    assert_eq!(got, 1);
    let got = sends(
        cache(
            "BitPos",
            json!({"namespace": "n", "key": "b", "bit": true, "range": null}),
        ),
        json!({"Json": {"position": -1}}),
        |db| db.cache_bit_pos("n", "b", true, None),
    );
    assert_eq!(got, -1);
    let got = sends(
        cache(
            "BitOp",
            json!({"namespace": "n", "op": "Xor", "dest": "d", "keys": ["a", "b"]}),
        ),
        json!({"Json": {"length": 1}}),
        |db| db.cache_bit_op("n", BitOpKind::Xor, "d", &["a", "b"]),
    );
    assert_eq!(got, 1);

    let got = sends(
        cache(
            "PfAdd",
            json!({"namespace": "n", "key": "h", "elements": [[1], [2]]}),
        ),
        json!({"Json": {"changed": true}}),
        |db| db.cache_pf_add("n", "h", &[[1u8], [2u8]]),
    );
    assert!(got);
    let got = sends(
        cache("PfCount", json!({"namespace": "n", "keys": ["h", "h2"]})),
        json!({"Json": {"count": 4}}),
        |db| db.cache_pf_count("n", &["h", "h2"]),
    );
    assert_eq!(got, 4);
    sends(
        cache(
            "PfMerge",
            json!({"namespace": "n", "dest": "h3", "sources": ["h", "h2"]}),
        ),
        json!({"Json": {"merged": true}}),
        |db| db.cache_pf_merge("n", "h3", &["h", "h2"]),
    );
}

#[test]
fn cache_lists_sets_and_hashes() {
    let got = sends(
        cache(
            "LMove",
            json!({"namespace": "n", "source": "a", "destination": "b", "from": "Left", "to": "Right"}),
        ),
        json!({"CacheValue": [1]}),
        |db| db.cache_lmove("n", "a", "b", ListEnd::Left, ListEnd::Right),
    );
    assert_eq!(got, Some(vec![1]));

    let got = sends(
        cache(
            "LTrim",
            json!({"namespace": "n", "key": "a", "start": 0, "stop": 0}),
        ),
        json!({"Json": {"length": 1, "removed": 1}}),
        |db| db.cache_ltrim("n", "a", 0, 0),
    );
    assert_eq!((got.length, got.removed), (1, 1));

    let got = sends(
        cache(
            "BLPop",
            json!({"namespace": "n", "keys": ["a", "l2"], "timeout_ms": 100}),
        ),
        json!({"CacheValues": [[108, 50], [1]]}),
        |db| db.cache_blpop("n", &["a", "l2"], Duration::from_millis(100)),
    );
    assert_eq!(got, Some(("l2".to_string(), vec![1])));
    let got = sends(
        cache(
            "BRPop",
            json!({"namespace": "n", "keys": ["a"], "timeout_ms": 100}),
        ),
        json!({"CacheValues": [null, null]}),
        |db| db.cache_brpop("n", &["a"], Duration::from_millis(100)),
    );
    assert_eq!(got, None);

    let members = json!({"Json": {"count": 2, "members": [[1], [2]]}});
    let keys = json!({"namespace": "n", "keys": ["s1", "s2"]});
    let got = sends(cache("SUnion", keys.clone()), members.clone(), |db| {
        db.cache_sunion("n", &["s1", "s2"])
    });
    assert_eq!(got, vec![vec![1], vec![2]]);
    sends(cache("SInter", keys.clone()), members.clone(), |db| {
        db.cache_sinter("n", &["s1", "s2"])
    });
    sends(cache("SDiff", keys), members.clone(), |db| {
        db.cache_sdiff("n", &["s1", "s2"])
    });
    sends(
        cache(
            "SRandMember",
            json!({"namespace": "n", "key": "s1", "count": -3}),
        ),
        members.clone(),
        |db| db.cache_srand_member("n", "s1", Some(-3)),
    );
    sends(
        cache(
            "SRandMember",
            json!({"namespace": "n", "key": "s1", "count": null}),
        ),
        members,
        |db| db.cache_srand_member("n", "s1", None),
    );

    let got = sends(
        cache(
            "HIncrBy",
            json!({"namespace": "n", "key": "h", "field": [110], "by": 5}),
        ),
        json!({"Json": {"value": 5}}),
        |db| db.cache_hincr_by("n", "h", b"n", 5),
    );
    assert_eq!(got, 5);
}

#[test]
fn cache_pipelines_transactions_and_scripts() {
    let ops = [
        CacheCommand::set("n", "p", &[1], None),
        CacheCommand::get("n", "p"),
        CacheCommand::incr("n", "p", 1),
        CacheCommand::unit("Ping"),
        CacheCommand::exists("n", "p"),
        CacheCommand::delete("n", "p"),
        CacheCommand::new("LLen", json!({"namespace": "n", "key": "l"})),
    ];
    let wire_ops = json!([
        {"Set": {"namespace": "n", "key": "p", "value": [1], "ttl_ms": null}},
        {"Get": {"namespace": "n", "key": "p"}},
        {"Incr": {"namespace": "n", "key": "p", "by": 1}},
        "Ping",
        {"Exists": {"namespace": "n", "key": "p"}},
        {"Delete": {"namespace": "n", "key": "p"}},
        {"LLen": {"namespace": "n", "key": "l"}},
    ]);
    let results = json!({"Json": {"results": [
        {"ok": {"Json": {"set": true}}},
        {"ok": {"CacheValue": [1]}},
        {"error": "invalid request: cache incr: value at n:p is not a valid i64 integer"},
        {"ok": {"Message": "pong"}},
    ]}});

    let got = sends(
        cache("Pipeline", json!({"ops": wire_ops})),
        results.clone(),
        |db| db.cache_pipeline(&ops),
    );
    assert_eq!(got.len(), 4);
    assert_eq!(
        got[0],
        CacheReply::Ok {
            kind: "Json".into(),
            data: json!({"set": true})
        }
    );
    assert_eq!(got[1].value(), Some(vec![1]));
    assert!(matches!(&got[2], CacheReply::Error(text) if text.contains("not a valid i64")));
    assert!(got[3].is_ok());

    sends(
        cache("Transaction", json!({"ops": wire_ops})),
        results.clone(),
        |db| db.cache_transaction(&ops),
    );

    sends(
        cache("ScriptLoad", json!({"name": "s", "ops": wire_ops})),
        json!({"Json": {"loaded": "s", "ops": 7}}),
        |db| db.cache_script_load("s", &ops),
    );
    let got = sends(
        cache("ScriptRun", json!({"name": "s", "args": ["k1", "k2"]})),
        results,
        |db| db.cache_script_run("s", &["k1", "k2"]),
    );
    assert_eq!(got.len(), 4);
}

#[test]
fn cache_publish_poll_and_administration() {
    let got = sends(
        cache("Publish", json!({"channel": "c", "message": "hello"})),
        json!({"Json": {"channel": "c", "id": 1}}),
        |db| db.cache_publish("c", "hello"),
    );
    assert_eq!(got, 1);

    let got = sends(
        cache(
            "Poll",
            json!({"channels": ["c", "d*"], "after": 1, "limit": 10}),
        ),
        json!({"Json": {"messages": [{"at_ms": 1791870248123i64, "channel": "c", "id": 2, "message": "again"}], "next": 2}}),
        |db| db.cache_poll(&["c", "d*"], 1, Some(10)),
    );
    assert_eq!(got.next, 2);
    assert_eq!(got.messages[0].message, "again");
    sends(
        cache("Poll", json!({"channels": ["c"], "after": 0})),
        json!({"Json": {"messages": [], "next": 0}}),
        |db| db.cache_poll(&["c"], 0, None),
    );

    let got = sends(
        json!({"Cache": "Stats"}),
        json!({"Json": {"eviction": {"evicted": 0, "policy": "lru"}, "hitRatio": 0.75, "hits": 3, "memory": {"enabled": true, "lowWaterBytes": 241591860, "maxBytes": 268435456, "measurements": 0, "residentBytes": null}, "misses": 1}}),
        |db| db.cache_stats(),
    );
    assert_eq!((got.hits, got.misses), (3, 1));
    assert_eq!(got.eviction.policy, "lru");
    assert_eq!(got.memory.max_bytes, Some(268435456));

    let got = sends(
        json!({"Cache": "FlushDatabase"}),
        json!({"Json": {"cleared": 1}}),
        |db| db.cache_flush_database(),
    );
    assert_eq!(got, 1);
    let got = sends(
        json!({"Cache": "PurgeExpired"}),
        json!({"Json": {"purged": 0}}),
        |db| db.cache_purge_expired(),
    );
    assert_eq!(got, 0);

    // The payload is passed through: the live server refused this operation,
    // so its shape was never observed.
    sends(
        cache("HotKeys", json!({"limit": 3})),
        json!({"Json": {}}),
        |db| db.cache_hot_keys(Some(3)),
    );
    sends(cache("HotKeys", json!({})), json!({"Json": {}}), |db| {
        db.cache_hot_keys(None)
    });

    let limit = json!({"Json": {"bytes": 0, "keys": 0, "max_bytes": 100, "max_keys": null, "namespace": "n"}});
    let got = sends(
        cache(
            "SetNamespaceLimit",
            json!({"namespace": "n", "max_keys": null, "max_bytes": 100}),
        ),
        limit.clone(),
        |db| db.cache_set_namespace_limit("n", None, Some(100)),
    );
    assert_eq!((got.max_keys, got.max_bytes), (None, Some(100)));
    sends(
        cache("GetNamespaceLimit", json!({"namespace": "n"})),
        limit,
        |db| db.cache_get_namespace_limit("n"),
    );
}

// -- documents ---------------------------------------------------------------

#[test]
fn document_replace_and_batches() {
    let got = sends(
        document(
            "Replace",
            json!({"collection": "c", "id": "a", "document": {"n": 1}, "upsert": false}),
        ),
        json!({"Json": {"id": "a", "inserted": false, "replaced": true}}),
        |db| db.document_replace("c", "a", &json!({"n": 1}), &ReplaceOptions::default()),
    );
    assert!(got.replaced && got.document.is_none());

    let options = ReplaceOptions {
        upsert: true,
        return_document: ReturnDocument::After,
        expected_version: Some(3),
    };
    let got = sends(
        document(
            "Replace",
            json!({"collection": "c", "id": "a", "document": {"n": 1}, "upsert": true, "return": "After", "expected_version": 3}),
        ),
        json!({"Json": {"document": {"_id": "a", "_version": 4, "n": 1}, "id": "a", "inserted": false, "replaced": true}}),
        |db| db.document_replace("c", "a", &json!({"n": 1}), &options),
    );
    assert_eq!(got.document.expect("the after-image")["_version"], 4);

    let got = sends(
        document(
            "InsertMany",
            json!({"collection": "c", "documents": [{"_id": "a", "n": 1}, {"n": 2}]}),
        ),
        json!({"Json": {"ids": ["a", "71528832217926cb"], "inserted": 2}}),
        |db| db.document_insert_many("c", &[json!({"_id": "a", "n": 1}), json!({"n": 2})]),
    );
    assert_eq!(got, vec!["a", "71528832217926cb"]);

    let got = sends(
        document(
            "DeleteMany",
            json!({"collection": "c", "filter": {"Gte": {"field": "n", "value": 100}}}),
        ),
        json!({"Json": {"deleted": 1}}),
        |db| db.document_delete_many("c", &DocumentFilter::gte("n", 100)),
    );
    assert_eq!(got, 1);

    let ops = [
        BulkOp::insert_with_id("b1", json!({"n": 1})),
        BulkOp::insert(json!({"n": 2})),
        BulkOp::update_one("b1", DocumentUpdate::new().set("n", 5)),
        BulkOp::update_one("b2", DocumentUpdate::new().inc("n", 1))
            .upsert()
            .expected_version(0),
        BulkOp::replace("b", json!({"n": 20})).expected_version(2),
        BulkOp::delete("old"),
    ];
    let got = sends(
        document(
            "BulkWrite",
            json!({"collection": "c", "ops": [
                {"Insert": {"id": "b1", "document": {"n": 1}}},
                {"Insert": {"id": null, "document": {"n": 2}}},
                {"UpdateOne": {"id": "b1", "update": {"set": {"n": 5}}, "upsert": false, "expected_version": null}},
                {"UpdateOne": {"id": "b2", "update": {"inc": {"n": 1}}, "upsert": true, "expected_version": 0}},
                {"Replace": {"id": "b", "document": {"n": 20}, "upsert": false, "expected_version": 2}},
                {"Delete": {"id": "old", "expected_version": null}},
            ]}),
        ),
        json!({"Json": {"deleted": 1, "inserted": 2, "inserted_ids": ["b1", "08d167b34ddc29df"], "replaced": 1, "updated": 1, "upserted": 1}}),
        |db| db.document_bulk_write("c", &ops),
    );
    assert_eq!(
        (
            got.inserted,
            got.updated,
            got.upserted,
            got.replaced,
            got.deleted
        ),
        (2, 1, 1, 1, 1)
    );
    assert_eq!(got.inserted_ids.len(), 2);
}

#[test]
fn document_collections_and_indexes() {
    sends(
        document("RenameCollection", json!({"collection": "c", "to": "d"})),
        json!({"Json": {"documents": 5, "from": "c", "indexes": 1, "renamed": true, "to": "d"}}),
        |db| db.document_rename_collection("c", "d"),
    );

    let got = sends(
        document("PurgeExpired", json!({"collection": "c"})),
        json!({"Json": {"more": false, "purged": 0}}),
        |db| db.document_purge_expired("c"),
    );
    assert_eq!((got.purged, got.more), (0, false));

    let plan = json!({"Json": {"elapsed_us": 49, "estimated_cost": 1000, "estimated_rows": 1000, "index": null, "index_used": false, "plan": "FindScan", "rows_examined": 4, "rows_returned": 1, "scan_reason": "no index on field `kind`", "scan_source": "memory_index"}});
    let got = sends(
        document(
            "Explain",
            json!({"collection": "c", "filter": {"Eq": {"field": "kind", "value": "x"}}}),
        ),
        plan.clone(),
        |db| {
            db.document_explain(
                "c",
                &DocumentFilter::eq("kind", "x"),
                &ExplainOptions::default(),
            )
        },
    );
    assert_eq!(got.plan, "FindScan");
    assert_eq!((got.rows_examined, got.rows_returned), (4, 1));
    assert!(!got.index_used && got.index.is_none());
    let options = ExplainOptions {
        limit: Some(2),
        sort: vec![SortKey::descending("n")],
        skip: Some(1),
    };
    sends(
        document(
            "Explain",
            json!({"collection": "c", "filter": "All", "limit": 2, "sort": [{"field": "n", "descending": true}], "skip": 1}),
        ),
        plan,
        |db| db.document_explain("c", &DocumentFilter::all(), &options),
    );

    let got = sends(
        document(
            "RebuildIndex",
            json!({"collection": "c", "index_name": "by_kind"}),
        ),
        json!({"Json": {"documents": 4, "entries": 2, "rebuilt": "by_kind"}}),
        |db| db.document_rebuild_index("c", "by_kind"),
    );
    assert_eq!(got.index_name, "by_kind");
    assert_eq!((got.documents, got.entries), (4, 2));

    let options = CollectionOptions {
        schema: Some(DocumentSchema {
            required: vec!["n".into()],
            fields: BTreeMap::from([(
                "n".to_string(),
                FieldRule {
                    minimum: Some(0.0),
                    ..FieldRule::of_type(FieldType::Number)
                },
            )]),
            additional_fields: true,
        }),
        defaults: object(json!({"kind": "z"})),
        versioned: true,
    };
    let wire_options = json!({"schema": {"required": ["n"], "fields": {"n": {"type": "number", "minimum": 0.0}}, "additional_fields": true}, "defaults": {"kind": "z"}, "versioned": true});
    let got = sends(
        document(
            "SetCollectionOptions",
            json!({"collection": "c", "options": wire_options}),
        ),
        json!({"Json": {"documents_checked": 4, "updated": true}}),
        |db| db.document_set_collection_options("c", &options),
    );
    assert_eq!(got, 4);

    let got = sends(
        document("GetCollectionOptions", json!({"collection": "c"})),
        json!({"Json": {"collection": "c", "options": wire_options}}),
        |db| db.document_get_collection_options("c"),
    );
    assert_eq!(got, options, "what was set is what reads back");
    let got = sends(
        document("GetCollectionOptions", json!({"collection": "c"})),
        json!({"Json": {"collection": "c", "options": {"versioned": false}}}),
        |db| db.document_get_collection_options("c"),
    );
    assert_eq!(got, CollectionOptions::default());
}

#[test]
fn document_transactions_name_themselves_in_the_request_options() {
    let got = sends(
        document("BeginTransaction", json!({"serializable": true})),
        json!({"Json": {"serializable": true, "txn": "dtx-1-2"}}),
        |db| db.document_begin_transaction(true),
    );
    assert_eq!(got, "dtx-1-2");

    let (request, outcome) = exchange(json!({"Json": {"committed": true, "writes": 2}}), |db| {
        db.document_commit_transaction("dtx-1-2")
    });
    assert_eq!(request["op"], json!({"Document": "CommitTransaction"}));
    assert_eq!(request["options"], json!({"txn": "dtx-1-2"}));
    let commit = outcome.expect("commit");
    assert!(commit.committed);
    assert_eq!(commit.writes, 2);

    let (request, outcome) = exchange(json!({"Json": {"rolled_back": true}}), |db| {
        db.document_rollback_transaction("dtx-1-3")
    });
    assert_eq!(request["op"], json!({"Document": "RollbackTransaction"}));
    assert_eq!(request["options"], json!({"txn": "dtx-1-3"}));
    outcome.expect("rollback");

    // A request sent while a transaction is named carries it, beside any
    // deadline, and a commit stops naming it.
    let (request, outcome) = exchange(json!({"Json": {"id": "t1", "inserted": true}}), |db| {
        db.set_request_txn(Some("dtx-9"));
        db.set_request_timeout(Some(Duration::from_secs(2)));
        assert_eq!(db.request_txn(), Some("dtx-9"));
        db.document_insert_with_id("c", "t1", &json!({"n": 1}))
    });
    assert_eq!(
        request["options"],
        json!({"timeout_ms": 2000, "txn": "dtx-9"})
    );
    outcome.expect("insert");

    let (_, outcome) = exchange(json!({"Json": {"committed": true, "writes": 0}}), |db| {
        db.set_request_txn(Some("dtx-9"));
        db.document_commit_transaction("dtx-9")?;
        Ok(db.request_txn().map(str::to_string))
    });
    assert_eq!(outcome.expect("commit"), None);
}

// -- vectors -----------------------------------------------------------------

#[test]
fn vector_writes() {
    let got = sends(
        vector("Embed", json!({"texts": ["hello world"], "dimension": 4})),
        json!({"Json": {"dimension": 4, "embedder": "hash-v1", "sparse": [{"indices": [822003, 11189515], "values": [0.7071067690849304, 0.7071067690849304]}], "vectors": [[0.5773502588272095, 0.5773502588272095, 0.0, -0.5773502588272095]]}}),
        |db| db.vector_embed(&["hello world"], 4),
    );
    assert_eq!(got.embedder, "hash-v1");
    assert_eq!(got.vectors[0].len(), 4);
    assert_eq!(got.sparse[0].indices, vec![822003, 11189515]);

    let got = sends(
        vector(
            "UpsertText",
            json!({"collection": "c", "items": [{"id": "t1", "text": "hello", "metadata": {"kind": "t"}}, {"id": "t2", "text": "bye"}]}),
        ),
        json!({"Json": {"embedder": "hash-v1", "upserted": 2}}),
        |db| {
            db.vector_upsert_text(
                "c",
                &[
                    TextUpsert::new("t1", "hello").metadata(object(json!({"kind": "t"}))),
                    TextUpsert::new("t2", "bye"),
                ],
            )
        },
    );
    assert_eq!(got, 2);

    let got = sends(
        vector(
            "UpsertMany",
            json!({"collection": "c", "items": [{"id": "a", "vector": [1.0, 0.0], "metadata": {"kind": "x"}}, {"id": "b", "vector": [0.0, 1.0]}]}),
        ),
        json!({"Json": {"upserted": 2}}),
        |db| {
            db.vector_upsert_many(
                "c",
                &[
                    VectorUpsert::new("a", [1.0, 0.0]).metadata(object(json!({"kind": "x"}))),
                    VectorUpsert::new("b", [0.0, 1.0]),
                ],
            )
        },
    );
    assert_eq!(got, 2);

    sends(
        vector(
            "UpsertNamed",
            json!({"collection": "c", "id": "n1", "vectors": {"default": [1.0, 1.0], "title": [1.0]}, "metadata": {"kind": "named"}}),
        ),
        json!({"Json": {"id": "n1", "vectors": 2}}),
        |db| {
            let vectors = BTreeMap::from([
                ("default".to_string(), vec![1.0, 1.0]),
                ("title".to_string(), vec![1.0]),
            ]);
            db.vector_upsert_named("c", "n1", &vectors, Some(&object(json!({"kind": "named"}))))
        },
    );

    sends(
        vector(
            "AddNamedVector",
            json!({"collection": "c", "name": "title", "dimension": 2, "metric": "dot"}),
        ),
        json!({"Json": {"collection": "c", "dimension": 2, "name": "title"}}),
        |db| db.vector_add_named_vector("c", "title", 2, VectorMetric::Dot),
    );

    let got = sends(
        vector(
            "DeleteMany",
            json!({"collection": "c", "ids": ["a", "nosuch"]}),
        ),
        json!({"Json": {"deleted": 1}}),
        |db| db.vector_delete_many("c", &["a", "nosuch"]),
    );
    assert_eq!(got, 1);
    let got = sends(
        vector(
            "DeleteByFilter",
            json!({"collection": "c", "filter": {"kind": "y"}}),
        ),
        json!({"Json": {"deleted": 1}}),
        |db| db.vector_delete_by_filter("c", &json!({"kind": "y"})),
    );
    assert_eq!(got, 1);
    let got = sends(
        vector(
            "UpdateByFilter",
            json!({"collection": "c", "filter": {"kind": "x"}, "set": {"seen": true}}),
        ),
        json!({"Json": {"updated": 1}}),
        |db| db.vector_update_by_filter("c", &json!({"kind": "x"}), &object(json!({"seen": true}))),
    );
    assert_eq!(got, 1);
}

#[test]
fn vector_searches() {
    let got = sends(
        vector(
            "SearchBatch",
            json!({"collection": "c", "vectors": [[1.0, 0.0], [0.0, 1.0]], "top_k": 2}),
        ),
        json!({"Json": {"count": 2, "results": [[{"id": "a", "metadata": {"kind": "x"}, "score": 1.0}], [{"id": "b", "metadata": null, "score": 1.0}]]}}),
        |db| {
            db.vector_search_batch(
                "c",
                &[vec![1.0, 0.0], vec![0.0, 1.0]],
                2,
                &VectorSearchOptions::default(),
            )
        },
    );
    assert_eq!(got.len(), 2);
    assert_eq!(got[0][0].id, "a");
    assert!(got[1][0].metadata.is_empty() && got[1][0].vector.is_none());

    let options = VectorSearchOptions {
        filter: Some(json!({"kind": "x"})),
        min_score: Some(0.5),
        include_vectors: true,
        include_metadata: Some(false),
        exact: true,
        using: Some("title".into()),
        explain: true,
        ..VectorSearchOptions::default()
    };
    let got = sends(
        vector(
            "SearchBatch",
            json!({"collection": "c", "vectors": [[1.0, 0.0]], "top_k": 2, "filter": {"kind": "x"}, "min_score": 0.5, "include_vectors": true, "include_metadata": false, "exact": true, "using": "title", "explain": true}),
        ),
        json!({"Json": {"count": 1, "results": [[{"id": "a", "reason": "cosine similarity 1.000 (very close)", "reason_source": "rules", "score": 1.0, "vector": [1.0, 0.0]}]]}}),
        |db| db.vector_search_batch("c", &[vec![1.0, 0.0]], 2, &options),
    );
    assert_eq!(got[0][0].vector, Some(vec![1.0, 0.0]));
    assert!(got[0][0].reason.as_deref().unwrap().contains("cosine"));

    let got = sends(
        vector(
            "SearchMulti",
            json!({"collection": "c", "vectors": [[1.0, 0.0], [0.0, 1.0]], "top_k": 3, "fusion": "max", "filter": {"kind": "x"}}),
        ),
        json!({"Json": {"depth": 12, "fusion": "max", "queries": 2, "results": [{"id": "a", "matched": 2, "metadata": {"kind": "x"}, "score": 1.0}]}}),
        |db| {
            db.vector_search_multi(
                "c",
                &[vec![1.0, 0.0], vec![0.0, 1.0]],
                3,
                FusionMethod::Max,
                Some(&json!({"kind": "x"})),
            )
        },
    );
    assert_eq!(got.fusion, FusionMethod::Max);
    assert_eq!((got.queries, got.depth), (2, 12));
    assert_eq!(got.results[0].matched, 2);
    sends(
        vector(
            "SearchMulti",
            json!({"collection": "c", "vectors": [[1.0, 0.0]], "top_k": 3, "fusion": "rrf"}),
        ),
        json!({"Json": {"depth": 12, "fusion": "rrf", "queries": 1, "results": []}}),
        |db| db.vector_search_multi("c", &[vec![1.0, 0.0]], 3, FusionMethod::Rrf, None),
    );

    let got = sends(
        vector(
            "SearchByExamples",
            json!({"collection": "c", "positive": ["a", [0.0, 1.0]], "negative": ["c"], "top_k": 3, "exclude_examples": true}),
        ),
        json!({"Json": {"examples": {"negative": 1, "positive": 2}, "index": "flat", "results": [{"id": "b", "metadata": {"kind": "y"}, "score": 0.5773502588272095}], "top_k": 5}}),
        |db| {
            db.vector_search_by_examples(
                "c",
                &[
                    VectorExample::id("a"),
                    VectorExample::Vector(vec![0.0, 1.0]),
                ],
                &[VectorExample::id("c")],
                3,
                true,
                &VectorSearchOptions::default(),
            )
        },
    );
    assert_eq!(got.index, "flat");
    assert_eq!(got.results[0].id, "b");

    let got = sends(
        vector("Count", json!({"collection": "c", "filter": {"kind": "x"}})),
        json!({"Json": {"count": 1, "filtered": true}}),
        |db| db.vector_count("c", Some(&json!({"kind": "x"}))),
    );
    assert_eq!(got, 1);
    sends(
        vector("Count", json!({"collection": "c"})),
        json!({"Json": {"count": 5, "filtered": false}}),
        |db| db.vector_count("c", None),
    );
}

#[test]
fn vector_collections_and_indexes() {
    sends(
        vector("SetAlias", json!({"alias": "al", "collection": "c"})),
        json!({"Json": {"alias": "al", "collection": "c"}}),
        |db| db.vector_set_alias("al", Some("c")),
    );
    sends(
        vector("SetAlias", json!({"alias": "al", "collection": null})),
        json!({"Json": {"alias": "al", "collection": null}}),
        |db| db.vector_set_alias("al", None),
    );
    let got = sends(
        json!({"Vector": "ListAliases"}),
        json!({"Json": {"aliases": [{"alias": "al", "collection": "c"}]}}),
        |db| db.vector_list_aliases(),
    );
    assert_eq!(
        (got[0].alias.as_str(), got[0].collection.as_str()),
        ("al", "c")
    );

    let got = sends(
        vector(
            "ListPartitions",
            json!({"collection": "c", "field": "_tenant"}),
        ),
        json!({"Json": {"field": "_tenant", "partitions": [{"count": 2, "name": "n1"}], "unpartitioned": 3}}),
        |db| db.vector_list_partitions("c", Some("_tenant")),
    );
    assert_eq!(got.partitions[0].name, "n1");
    assert_eq!(got.unpartitioned, 3);
    sends(
        vector("ListPartitions", json!({"collection": "c"})),
        json!({"Json": {"field": "_namespace", "partitions": [], "unpartitioned": 5}}),
        |db| db.vector_list_partitions("c", None),
    );

    let got = sends(
        vector("RenameCollection", json!({"collection": "c", "to": "d"})),
        json!({"Json": {"from": "c", "moved": 3, "renamed": true, "to": "d"}}),
        |db| db.vector_rename_collection("c", "d"),
    );
    assert_eq!(got, 3);
    let got = sends(
        vector("CopyCollection", json!({"collection": "c", "to": "d"})),
        json!({"Json": {"copied": 3, "to": "d"}}),
        |db| db.vector_copy_collection("c", "d"),
    );
    assert_eq!(got, 3);

    let got = sends(
        vector("RebuildIndex", json!({"collection": "c"})),
        json!({"Json": {"build_ms": 28, "persisted": true, "rebuilt": true, "tombstones_cleared": 0, "vectors": 6}}),
        |db| db.vector_rebuild_index("c"),
    );
    assert!(got.rebuilt && got.persisted);
    assert_eq!(got.vectors, 6);

    let got = sends(
        vector("IndexStats", json!({"collection": "c"})),
        json!({"Json": {"collection": "c", "flat_scan_threshold": 256, "index": "flat", "stored": 6}}),
        |db| db.vector_index_stats("c"),
    );
    assert_eq!(
        (got.index.as_str(), got.stored, got.layers),
        ("flat", 6, None)
    );
    let got = sends(
        vector("IndexStats", json!({"collection": "c"})),
        json!({"Json": {"collection": "c", "degree": {"max": 29, "mean": 10.5, "min": 2}, "entry": "v280", "index": "hnsw", "layers": 3, "live": 300, "live_per_layer": [300, 17, 1], "memory": {"overhead_bytes": 64922, "vector_bytes": 9600}, "nodes": 300, "parameters": {"ef_construction": 200, "ef_search": 64, "m": 16, "m0": 32}, "persisted": true, "reachable": 300, "should_rebuild": false, "stored": 300, "tombstones": 0, "unreachable": 0}}),
        |db| db.vector_index_stats("c"),
    );
    assert_eq!((got.layers, got.reachable), (Some(3), Some(300)));
    assert_eq!(got.live_per_layer, Some(vec![300, 17, 1]));

    let got = sends(
        vector(
            "MeasureRecall",
            json!({"collection": "c", "sample": 5, "top_k": 3, "ef_search": 50}),
        ),
        json!({"Json": {"collection": "c", "ef_search": 64, "flat_fallbacks": 0, "index": "hnsw", "measured": true, "min_recall": 1.0, "queries": 5, "recall": 1.0, "top_k": 3}}),
        |db| {
            db.vector_measure_recall(
                "c",
                &RecallOptions {
                    sample: Some(5),
                    top_k: Some(3),
                    ef_search: Some(50),
                },
            )
        },
    );
    assert!(got.measured);
    assert_eq!((got.recall, got.queries), (1.0, Some(5)));
    let got = sends(
        vector("MeasureRecall", json!({"collection": "c"})),
        json!({"Json": {"collection": "c", "index": "flat", "measured": false, "reason": "the collection is small enough that searches scan every vector", "recall": 1.0}}),
        |db| db.vector_measure_recall("c", &RecallOptions::default()),
    );
    assert!(!got.measured && got.reason.is_some());

    let got = sends(
        vector(
            "SetCollectionConfig",
            json!({"collection": "c", "normalize": true, "m": 8, "ef_construction": 100, "multi_tenant": false}),
        ),
        json!({"Json": {"collection": "c", "ef_construction": 100, "m": 8, "multi_tenant": false, "named": [{"dimension": 2, "metric": "dot", "name": "title"}], "normalize": true}}),
        |db| {
            db.vector_set_collection_config(
                "c",
                &VectorConfigChange {
                    normalize: Some(true),
                    m: Some(8),
                    ef_construction: Some(100),
                    multi_tenant: Some(false),
                },
            )
        },
    );
    assert_eq!((got.m, got.ef_construction, got.normalize), (8, 100, true));
    assert_eq!(got.named[0].metric, VectorMetric::Dot);
    sends(
        vector("SetCollectionConfig", json!({"collection": "c", "m": 16})),
        json!({"Json": {"collection": "c", "ef_construction": 200, "m": 16, "multi_tenant": false, "named": [], "normalize": false}}),
        |db| {
            db.vector_set_collection_config(
                "c",
                &VectorConfigChange {
                    m: Some(16),
                    ..VectorConfigChange::default()
                },
            )
        },
    );
}

// -- graphs ------------------------------------------------------------------

#[test]
fn graph_batch_writes_and_updates() {
    let got = sends(
        graph(
            "AddMany",
            json!({"graph": "g", "nodes": [{"id": "a", "labels": ["P"], "properties": {"name": "ada"}}, {"id": "b", "labels": []}], "edges": [{"id": "e1", "from": "a", "to": "b", "label": "K", "properties": {"w": 2}}, {"id": "e2", "from": "b", "to": "a", "label": "K"}]}),
        ),
        json!({"Json": {"atomic": true, "edges": 2, "nodes": 2}}),
        |db| {
            db.graph_add_many(
                "g",
                &[
                    GraphNodeInput::new("a", &["P"]).properties(object(json!({"name": "ada"}))),
                    GraphNodeInput::new("b", &[]),
                ],
                &[
                    GraphEdgeInput::new("e1", "a", "b", "K").properties(object(json!({"w": 2}))),
                    GraphEdgeInput::new("e2", "b", "a", "K"),
                ],
            )
        },
    );
    assert_eq!((got.nodes, got.edges), (2, 2));

    let got = sends(
        graph(
            "RemoveMany",
            json!({"graph": "g", "nodes": ["a"], "edges": ["e2"], "detach": true}),
        ),
        json!({"Json": {"atomic": true, "edges_removed": 4, "nodes_removed": 1}}),
        |db| db.graph_remove_many("g", &["a"], &["e2"], true),
    );
    assert_eq!((got.nodes_removed, got.edges_removed), (1, 4));

    let got = sends(
        graph(
            "UpdateNode",
            json!({"graph": "g", "id": "a", "properties": {"age": null, "city": "x"}, "add_labels": ["Q"], "remove_labels": ["P"]}),
        ),
        json!({"Json": {"id": "a", "labels": ["Q"], "properties": {"city": "x", "name": "ada"}}}),
        |db| {
            db.graph_update_node(
                "g",
                "a",
                &NodeUpdate {
                    properties: Some(json!({"age": null, "city": "x"})),
                    add_labels: vec!["Q".into()],
                    remove_labels: vec!["P".into()],
                },
            )
        },
    );
    assert_eq!(got.labels, vec!["Q"]);
    sends(
        graph("UpdateNode", json!({"graph": "g", "id": "a"})),
        json!({"Json": {"id": "a", "labels": [], "properties": null}}),
        |db| db.graph_update_node("g", "a", &NodeUpdate::default()),
    );

    let got = sends(
        graph(
            "UpdateEdge",
            json!({"graph": "g", "id": "e1", "properties": {"w": 3}, "label": "KNOWS"}),
        ),
        json!({"Json": {"from": "a", "id": "e1", "label": "KNOWS", "properties": {"w": 3}, "to": "b"}}),
        |db| db.graph_update_edge("g", "e1", Some(&json!({"w": 3})), Some("KNOWS")),
    );
    assert_eq!(got.label, "KNOWS");
    sends(
        graph("UpdateEdge", json!({"graph": "g", "id": "e1"})),
        json!({"Json": {"from": "a", "id": "e1", "label": "K", "properties": null, "to": "b"}}),
        |db| db.graph_update_edge("g", "e1", None, None),
    );

    let got = sends(
        graph("CopyGraph", json!({"graph": "g", "to": "g2"})),
        json!({"Json": {"copied": true, "edges": 5, "nodes": 7, "to": "g2"}}),
        |db| db.graph_copy("g", "g2"),
    );
    assert_eq!((got.nodes, got.edges), (7, 5));

    let got = sends(
        graph(
            "ImportCsv",
            json!({"graph": "g", "nodes": "id,labels,name\nx1,P,xavier\n", "edges": "id,from,to,label\nxe1,x1,x2,K\n"}),
        ),
        json!({"Json": {"atomic": true, "edges": 1, "nodes": 2}}),
        |db| {
            db.graph_import_csv(
                "g",
                Some("id,labels,name\nx1,P,xavier\n"),
                Some("id,from,to,label\nxe1,x1,x2,K\n"),
            )
        },
    );
    assert_eq!((got.nodes, got.edges), (2, 1));
    sends(
        graph("ImportCsv", json!({"graph": "g", "nodes": "id\nx\n"})),
        json!({"Json": {"atomic": true, "edges": 0, "nodes": 1}}),
        |db| db.graph_import_csv("g", Some("id\nx\n"), None),
    );
}

#[test]
fn graph_walks_and_paths() {
    let walk = TraverseOptions {
        direction: Some(GraphDirection::Both),
        label: Some("K".into()),
        max_depth: Some(2),
        limit: Some(10),
    };
    let got = sends(
        graph(
            "Dfs",
            json!({"graph": "g", "start": "a", "direction": "both", "label": "K", "max_depth": 2, "limit": 10}),
        ),
        json!({"Json": {"count": 2, "direction": "both", "limit": 10, "max_depth": 2, "nodes": [{"depth": 0, "id": "a", "labels": ["P"], "properties": {"name": "ada"}}, {"depth": 1, "id": "c", "labels": ["P"], "properties": null}], "order": "depth_first", "start": "a", "truncated": true}}),
        |db| db.graph_dfs("g", "a", &walk),
    );
    assert_eq!(got.count, 2);
    assert!(got.truncated);
    assert_eq!(got.nodes[1].depth, 1);
    sends(
        graph("Dfs", json!({"graph": "g", "start": "a"})),
        json!({"Json": {"count": 0, "nodes": [], "start": "a"}}),
        |db| db.graph_dfs("g", "a", &TraverseOptions::default()),
    );

    let got = sends(
        graph(
            "Subgraph",
            json!({"graph": "g", "seeds": ["a"], "direction": "both", "label": "K", "max_depth": 2, "limit": 10}),
        ),
        json!({"Json": {"direction": "both", "edge_count": 1, "edges": [{"from": "a", "id": "e1", "label": "K", "properties": {"w": 3}, "to": "b"}], "edges_truncated": false, "limit": 10, "max_depth": 2, "node_count": 2, "nodes": [{"depth": 0, "id": "a", "labels": ["P"], "properties": null}, {"depth": 1, "id": "b", "labels": [], "properties": null}], "nodes_truncated": false}}),
        |db| db.graph_subgraph("g", &["a"], &walk),
    );
    assert_eq!((got.node_count, got.edge_count), (2, 1));
    assert_eq!(got.edges[0].id, "e1");

    let got = sends(
        graph(
            "KShortestPaths",
            json!({"graph": "g", "from": "a", "to": "c", "k": 2, "direction": "outgoing", "label": "K", "weight_property": "w"}),
        ),
        json!({"Json": {"count": 2, "direction": "outgoing", "from": "a", "k": 2, "paths": [{"cost": 1.0, "edges": ["e4"], "hops": 1, "nodes": ["a", "c"]}, {"cost": 4.0, "edges": ["e1", "e2"], "hops": 2, "nodes": ["a", "b", "c"]}], "to": "c", "weight_property": "w"}}),
        |db| {
            db.graph_k_shortest_paths(
                "g",
                "a",
                "c",
                &KPathOptions {
                    k: Some(2),
                    direction: Some(GraphDirection::Outgoing),
                    label: Some("K".into()),
                    weight_property: Some("w".into()),
                },
            )
        },
    );
    assert_eq!(got.paths[1].cost, 4.0);
    assert_eq!(got.complete, None);

    let got = sends(
        graph(
            "AllPaths",
            json!({"graph": "g", "from": "a", "to": "c", "direction": "both", "label": "K", "max_depth": 2, "limit": 10}),
        ),
        json!({"Json": {"complete": true, "count": 1, "direction": "both", "from": "a", "limit": 10, "max_depth": 2, "paths": [{"cost": 1.0, "edges": ["e4"], "hops": 1, "nodes": ["a", "c"]}], "stopped_by": null, "to": "c"}}),
        |db| db.graph_all_paths("g", "a", "c", &walk),
    );
    assert_eq!(got.complete, Some(true));
    assert_eq!(got.paths[0].nodes, vec!["a", "c"]);
}

#[test]
fn graph_algorithms() {
    let got = sends(
        graph(
            "ConnectedComponents",
            json!({"graph": "g", "kind": "strong", "label": "K", "limit": 5}),
        ),
        json!({"Json": {"components": [{"nodes": ["a", "b", "c"], "size": 3, "truncated": false}, {"nodes": ["d"], "size": 1, "truncated": false}], "count": 2, "kind": "strong", "largest": 3, "truncated": false}}),
        |db| {
            db.graph_connected_components(
                "g",
                &ComponentOptions {
                    kind: Some(ComponentKind::Strong),
                    label: Some("K".into()),
                    limit: Some(5),
                },
            )
        },
    );
    assert_eq!(got.kind, ComponentKind::Strong);
    assert_eq!((got.count, got.largest), (2, 3));
    sends(
        graph("ConnectedComponents", json!({"graph": "g"})),
        json!({"Json": {"components": [], "count": 0, "kind": "weak", "largest": 0, "truncated": false}}),
        |db| db.graph_connected_components("g", &ComponentOptions::default()),
    );

    let got = sends(
        graph("DetectCycle", json!({"graph": "g"})),
        json!({"Json": {"cycle": {"edges": ["e1", "e2", "e3"], "nodes": ["a", "b", "c"]}, "directed": true, "hasCycle": true}}),
        |db| db.graph_detect_cycle("g", None, None),
    );
    assert!(got.has_cycle);
    assert_eq!(got.cycle.expect("evidence").edges.len(), 3);
    let got = sends(
        graph(
            "DetectCycle",
            json!({"graph": "g", "directed": false, "label": "K"}),
        ),
        json!({"Json": {"cycle": null, "directed": false, "hasCycle": false}}),
        |db| db.graph_detect_cycle("g", Some(false), Some("K")),
    );
    assert!(!got.has_cycle && got.cycle.is_none());

    let got = sends(
        graph(
            "Centrality",
            json!({"graph": "g", "measure": "eigenvector", "directed": false, "label": "K", "limit": 2, "iterations": 50, "tolerance": 0.0001}),
        ),
        json!({"Json": {"converged": true, "directed": false, "iterations": 10, "measure": "eigenvector", "nodes": 4, "results": [{"id": "c", "score": 0.726517421933266}]}}),
        |db| {
            db.graph_centrality(
                "g",
                CentralityMeasure::Eigenvector,
                &CentralityOptions {
                    directed: Some(false),
                    label: Some("K".into()),
                    limit: Some(2),
                    iterations: Some(50),
                    tolerance: Some(0.0001),
                },
            )
        },
    );
    assert_eq!(got.measure, CentralityMeasure::Eigenvector);
    assert_eq!((got.converged, got.iterations), (Some(true), Some(10)));
    let got = sends(
        graph(
            "Centrality",
            json!({"graph": "g", "measure": "betweenness"}),
        ),
        json!({"Json": {"directed": true, "measure": "betweenness", "nodes": 4, "results": [{"id": "a", "score": 0.16666666666666666}]}}),
        |db| {
            db.graph_centrality(
                "g",
                CentralityMeasure::Betweenness,
                &CentralityOptions::default(),
            )
        },
    );
    assert_eq!(got.results[0].id, "a");
    assert_eq!(got.converged, None);

    let got = sends(
        graph(
            "Communities",
            json!({"graph": "g", "label": "K", "iterations": 5, "limit": 2}),
        ),
        json!({"Json": {"algorithm": "louvain", "communities": [{"nodes": ["a", "b", "c"], "size": 3, "truncated": false}], "converged": true, "count": 2, "iterations": 3, "modularity": 0.0}}),
        |db| {
            db.graph_communities(
                "g",
                &CommunityOptions {
                    label: Some("K".into()),
                    iterations: Some(5),
                    limit: Some(2),
                },
            )
        },
    );
    assert_eq!(got.algorithm, "louvain");
    assert_eq!(got.communities[0].size, 3);

    let got = sends(
        graph(
            "TriangleCount",
            json!({"graph": "g", "label": "K", "limit": 2}),
        ),
        json!({"Json": {"average_clustering": 0.75, "nodes": [{"clustering": 1.0, "id": "a", "triangles": 1}], "transitivity": 1.0, "triangles": 1}}),
        |db| db.graph_triangle_count("g", Some("K"), Some(2)),
    );
    assert_eq!((got.triangles, got.average_clustering), (1, 0.75));

    let got = sends(
        graph(
            "NodeSimilarity",
            json!({"graph": "g", "node": "a", "direction": "outgoing", "label": "K", "metric": "overlap", "limit": 2}),
        ),
        json!({"Json": {"count": 1, "direction": "outgoing", "metric": "overlap", "node": "a", "results": [{"id": "b", "score": 1.0}]}}),
        |db| {
            db.graph_node_similarity(
                "g",
                "a",
                &SimilarityOptions {
                    direction: Some(GraphDirection::Outgoing),
                    label: Some("K".into()),
                    metric: Some(SimilarityMetric::Overlap),
                    limit: Some(2),
                },
            )
        },
    );
    assert_eq!((got[0].id.as_str(), got[0].score), ("b", 1.0));

    let got = sends(
        graph(
            "PageRank",
            json!({"graph": "g", "damping": 0.5, "iterations": 10, "tolerance": 0.001, "label": "K", "limit": 2}),
        ),
        json!({"Json": {"converged": true, "count": 4, "damping": 0.5, "iterations": 6, "ranks": [{"id": "c", "score": 0.3297452926635742}], "truncated": true}}),
        |db| {
            db.graph_page_rank(
                "g",
                &PageRankOptions {
                    damping: Some(0.5),
                    iterations: Some(10),
                    tolerance: Some(0.001),
                    label: Some("K".into()),
                    limit: Some(2),
                },
            )
        },
    );
    assert!(got.converged && got.truncated);
    assert_eq!(got.ranks[0].id, "c");
    sends(
        graph("PageRank", json!({"graph": "g"})),
        json!({"Json": {"converged": false, "count": 0, "damping": 0.85, "iterations": 20, "ranks": [], "truncated": false}}),
        |db| db.graph_page_rank("g", &PageRankOptions::default()),
    );
}

#[test]
fn graph_descriptions_and_schema_objects() {
    let got = sends(
        graph("GraphStats", json!({"graph": "g"})),
        json!({"Json": {"avgOutDegree": 1.0, "danglingEdges": 0, "edgeTypes": {"K": 4}, "edges": 4, "isolatedNodes": 1, "maxInDegree": 2, "maxOutDegree": 2, "nodeLabels": {"P": 3, "Q": 1}, "nodes": 4, "selfLoops": 0, "unlabeledNodes": 1}}),
        |db| db.graph_stats("g"),
    );
    assert_eq!((got.nodes, got.edges, got.isolated_nodes), (4, 4, 1));
    assert_eq!(got.node_labels["P"], 3);
    assert_eq!(got.edge_types["K"], 4);

    let got = sends(
        graph("GraphSchema", json!({"graph": "g"})),
        json!({"Json": {"edge_types": [{"count": 4, "properties": [{"count": 1, "key": "w", "types": ["number"]}], "type": "K"}], "edges": 4, "node_labels": [{"count": 3, "label": "P", "properties": [{"count": 2, "key": "name", "types": ["string"]}]}], "nodes": 4, "unlabeled_nodes": {"count": 1, "properties": []}}}),
        |db| db.graph_schema("g"),
    );
    assert_eq!(got.edge_types[0].edge_type, "K");
    assert_eq!(got.node_labels[0].properties[0].key, "name");
    assert_eq!(got.unlabeled_nodes.count, 1);

    let got = sends(
        graph("CheckConsistency", json!({"graph": "g"})),
        json!({"Json": {"consistent": true, "dangling_edges": {"count": 0, "listed": []}, "edges": 4, "mirror": {"differences": {"count": 0, "listed": []}, "loaded": true}, "misfiled": {"count": 0, "listed": []}, "nodes": 4, "undecodable": {"count": 0, "listed": []}}}),
        |db| db.graph_check_consistency("g"),
    );
    assert!(got.consistent && got.mirror.loaded);
    assert_eq!(got.dangling_edges.count, 0);

    sends(
        graph(
            "CreateNodeIndex",
            json!({"graph": "g", "name": "by_name", "label": "P", "properties": ["name"]}),
        ),
        json!({"Json": {"created": true, "name": "by_name"}}),
        |db| db.graph_create_node_index("g", "by_name", Some("P"), &["name"]),
    );
    sends(
        graph(
            "CreateNodeIndex",
            json!({"graph": "g", "name": "by_nc", "properties": ["name", "city"]}),
        ),
        json!({"Json": {"created": true, "name": "by_nc"}}),
        |db| db.graph_create_node_index("g", "by_nc", None, &["name", "city"]),
    );
    sends(
        graph(
            "CreateConstraint",
            json!({"graph": "g", "name": "u", "label": "P", "property": "name", "kind": "unique", "target": "node"}),
        ),
        json!({"Json": {"created": true, "kind": "unique", "name": "u", "target": "node"}}),
        |db| {
            db.graph_create_constraint(
                "g",
                "u",
                "P",
                "name",
                ConstraintKind::Unique,
                ConstraintTarget::Node,
            )
        },
    );
    sends(
        graph(
            "CreateConstraint",
            json!({"graph": "g", "name": "r", "label": "K", "property": "w", "kind": "required", "target": "edge"}),
        ),
        json!({"Json": {"created": true, "kind": "required", "name": "r", "target": "edge"}}),
        |db| {
            db.graph_create_constraint(
                "g",
                "r",
                "K",
                "w",
                ConstraintKind::Required,
                ConstraintTarget::Edge,
            )
        },
    );
    sends(
        graph("DropSchemaObject", json!({"graph": "g", "name": "by_nc"})),
        json!({"Json": {"dropped": true, "name": "by_nc"}}),
        |db| db.graph_drop_schema_object("g", "by_nc"),
    );
    let got = sends(
        graph("ListSchemaObjects", json!({"graph": "g"})),
        json!({"Json": {"constraints": [{"kind": "unique", "label": "P", "name": "u", "property": "name", "target": "node"}], "indexes": [{"label": "P", "name": "by_name", "properties": ["name"]}, {"label": null, "name": "by_nc", "properties": ["name", "city"]}]}}),
        |db| db.graph_list_schema_objects("g"),
    );
    assert_eq!(got.constraints[0].kind, ConstraintKind::Unique);
    assert_eq!(got.indexes[1].label, None);

    let got = sends(
        graph(
            "FindNodes",
            json!({"graph": "g", "label": "P", "properties": {"name": "ada"}, "limit": 1}),
        ),
        json!({"Json": {"count": 1, "nodes": [{"id": "a", "labels": ["Q", "P"], "properties": {"city": "x", "name": "ada"}}], "plan": "index `by_name`", "truncated": false}}),
        |db| {
            db.graph_find_nodes(
                "g",
                Some("P"),
                Some(&object(json!({"name": "ada"}))),
                Some(1),
            )
        },
    );
    assert_eq!(got.plan, "index `by_name`");
    assert_eq!(got.nodes[0].id, "a");
    sends(
        graph("FindNodes", json!({"graph": "g"})),
        json!({"Json": {"count": 0, "nodes": [], "plan": "scan", "truncated": false}}),
        |db| db.graph_find_nodes("g", None, None, None),
    );
}

#[test]
fn graph_cypher_writes_and_transactions() {
    let got = sends(
        graph(
            "Execute",
            json!({"graph": "g", "cypher": "CREATE (n:P {name: 'zed'})"}),
        ),
        json!({"Json": {"graph": "g", "labelsAdded": 0, "labelsRemoved": 0, "nodesCreated": 1, "nodesDeleted": 0, "propertiesSet": 0, "relationshipsCreated": 0, "relationshipsDeleted": 0, "statements": 1}}),
        |db| db.graph_execute("g", "CREATE (n:P {name: 'zed'})"),
    );
    assert_eq!(
        (got.nodes_created, got.statements, got.queued),
        (1, 1, None)
    );

    let got = sends(
        graph("BeginTransaction", json!({"graph": "g"})),
        json!({"Json": {"transaction": "gtx-1-7"}}),
        |db| db.graph_begin_transaction("g"),
    );
    assert_eq!(got, "gtx-1-7");

    let (request, outcome) = exchange(
        json!({"Json": {"queued": 1, "transaction": "gtx-1-7"}}),
        |db| {
            db.set_request_txn(Some("gtx-1-7"));
            db.graph_execute("g", "CREATE (n:P)")
        },
    );
    assert_eq!(request["options"], json!({"txn": "gtx-1-7"}));
    assert_eq!(outcome.expect("queued").queued, Some(1));

    let (request, outcome) = exchange(
        json!({"Json": {"committed": 1, "cypher": "CREATE (n:P)", "graph": "g", "labelsAdded": 0, "labelsRemoved": 0, "nodesCreated": 1, "nodesDeleted": 0, "propertiesSet": 0, "relationshipsCreated": 0, "relationshipsDeleted": 0, "statements": 1}}),
        |db| db.graph_commit_transaction("g", "gtx-1-7"),
    );
    assert_eq!(
        request["op"],
        graph("CommitTransaction", json!({"graph": "g"}))
    );
    assert_eq!(request["options"], json!({"txn": "gtx-1-7"}));
    let commit = outcome.expect("commit");
    assert_eq!((commit.committed, commit.nodes_created), (Some(1), 1));

    let (request, outcome) = exchange(json!({"Json": {"rolled_back": 1}}), |db| {
        db.graph_rollback_transaction("g", "gtx-1-8")
    });
    assert_eq!(
        request["op"],
        graph("RollbackTransaction", json!({"graph": "g"}))
    );
    assert_eq!(request["options"], json!({"txn": "gtx-1-8"}));
    assert_eq!(outcome.expect("rollback"), 1);
}
