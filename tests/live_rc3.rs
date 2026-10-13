//! The operations added for server 0.1.0-rc.3, against a real server.
//!
//! The server is the one `TRICORE_TEST_ADDR` names (with `TRICORE_TEST_USER`
//! and `TRICORE_TEST_SECRET`), or else one started from a local binary as
//! `tests/live.rs` does. With neither, these tests print why and pass.
//!
//! A named server is shared, so every name here is unique to the run and only
//! what a test created is removed.

mod common;

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Map, Value};
use tricoredb::{
    BitOpKind, BulkOp, CacheCommand, CacheEntry, CacheExpiry, CacheReply, CentralityMeasure,
    CentralityOptions, Client, CollectionOptions, CommunityOptions, ComponentOptions,
    ConstraintKind, ConstraintTarget, DocumentFilter, DocumentSchema, DocumentUpdate, ErrorKind,
    ExplainOptions, FieldRule, FieldType, FreshState, FusionMethod, GraphDirection, GraphEdgeInput,
    GraphNodeInput, KPathOptions, ListEnd, NodeUpdate, Options, PageRankOptions, RecallOptions,
    ReplaceOptions, ReturnDocument, ScoreBound, ScoredMember, SetCondition, SimilarityOptions,
    TextUpsert, TraverseOptions, VectorConfigChange, VectorExample, VectorMetric,
    VectorSearchOptions, VectorUpsert,
};

use common::{external_server, start_server, unique, Server};

/// How to reach the server these tests use, or `None` to skip.
fn options() -> Option<Options> {
    match external_server() {
        Ok(Some(options)) => return Some(options),
        Ok(None) => {}
        Err(reason) => panic!("{reason}"),
    }
    static SERVER: OnceLock<Result<Server, String>> = OnceLock::new();
    match SERVER.get_or_init(start_server) {
        Ok(server) => Some(server.options()),
        Err(reason) => {
            eprintln!("skipping: {reason}");
            None
        }
    }
}

fn live(test: impl FnOnce(&mut Client)) {
    let Some(options) = options() else { return };
    let mut client = Client::connect(&options).expect("connect to the test server");
    test(&mut client);
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().expect("an object").clone()
}

#[test]
fn a_nested_begin_opens_a_block_inside_the_transaction() {
    let Some(options) = options() else { return };
    let mut db = Client::connect(&options).expect("connect");
    let table = unique("rs_nested");
    db.execute(&format!("CREATE TABLE {table} (id INT PRIMARY KEY)"))
        .expect("create");

    // Another client's schema change aborts an open transaction on a shared
    // server, which is the server's rule and not what is under test: try the
    // whole exchange again when that happens.
    let mut outcome = Err(tricoredb::Error::new(ErrorKind::Server, "not run"));
    for _ in 0..3 {
        if db.in_transaction() {
            let _ = db.rollback();
        }
        let _ = db.execute(&format!("DELETE FROM {table}"));
        outcome = nested_exchange(&mut db, &table);
        if outcome.is_ok() {
            break;
        }
    }
    outcome.expect("the nested exchange");

    let rows = db
        .query(&format!("SELECT id FROM {table} ORDER BY id"))
        .expect("read back");
    assert_eq!(
        rows.rows,
        vec![vec!["1".to_string()], vec!["3".to_string()]],
        "the outer write and the released block survive; the rolled-back block is gone"
    );
    assert!(db.commit().is_err(), "nothing is open any more");
    let _ = db.execute(&format!("DROP TABLE {table}"));
}

/// BEGIN, a write, a nested block rolled back, a nested block committed, and
/// the outer COMMIT — asserting at each step whether a transaction is open.
fn nested_exchange(db: &mut Client, table: &str) -> tricoredb::Result<()> {
    let outer = db.begin()?;
    assert_eq!(outer.outcome, "began");
    assert_eq!(outer.nested_depth, None);
    db.execute(&format!("INSERT INTO {table} VALUES (1)"))?;

    let inner = db.begin()?;
    assert_eq!(inner.nested_depth, Some(1), "a second BEGIN nests");
    db.execute(&format!("INSERT INTO {table} VALUES (2)"))?;
    let undone = db.rollback()?;
    assert_eq!(undone.nested_depth, Some(0));
    assert!(
        db.in_transaction(),
        "rolling a nested block back leaves the transaction open"
    );

    db.begin()?;
    db.execute(&format!("INSERT INTO {table} VALUES (3)"))?;
    let released = db.commit()?;
    assert_eq!(released.nested_depth, Some(0));
    assert!(
        db.in_transaction(),
        "committing a nested block leaves the transaction open"
    );

    let done = db.commit()?;
    assert_eq!(done.outcome, "committed");
    assert_eq!(done.nested_depth, None);
    assert!(!db.in_transaction());
    Ok(())
}

#[test]
fn cache_sorted_sets_order_rank_and_range() {
    live(|db| {
        let ns = unique("rs_zset");
        let counts = db
            .cache_zadd(
                &ns,
                "board",
                &[
                    ScoredMember::new(10.0, "ada"),
                    ScoredMember::new(30.0, "cy"),
                    ScoredMember::new(20.0, "bob"),
                ],
            )
            .expect("zadd");
        assert_eq!((counts.added, counts.updated), (3, 0));
        let counts = db
            .cache_zadd(&ns, "board", &[ScoredMember::new(40.0, "ada")])
            .expect("re-score");
        assert_eq!((counts.added, counts.updated), (0, 1));

        assert_eq!(db.cache_zcard(&ns, "board").expect("zcard"), 3);
        assert_eq!(
            db.cache_zscore(&ns, "board", b"ada").expect("zscore"),
            Some(40.0)
        );
        assert_eq!(
            db.cache_zscore(&ns, "board", b"nobody").expect("zscore"),
            None
        );
        assert_eq!(
            db.cache_zrank(&ns, "board", b"ada", false).expect("zrank"),
            Some(2)
        );
        assert_eq!(
            db.cache_zrank(&ns, "board", b"ada", true)
                .expect("zrevrank"),
            Some(0)
        );
        assert_eq!(
            db.cache_zrank(&ns, "board", b"nobody", false)
                .expect("zrank"),
            None
        );

        assert_eq!(
            db.cache_zrange(&ns, "board", 0, -1, false).expect("zrange"),
            vec![b"bob".to_vec(), b"cy".to_vec(), b"ada".to_vec()]
        );
        let top = db
            .cache_zrange_with_scores(&ns, "board", 0, 0, true)
            .expect("zrange rev");
        assert_eq!(top, vec![ScoredMember::new(40.0, "ada")]);

        let above = db
            .cache_zrange_by_score(
                &ns,
                "board",
                ScoreBound::exclusive(20.0),
                ScoreBound::highest(),
                false,
                None,
            )
            .expect("by score");
        assert_eq!(above, vec![b"cy".to_vec(), b"ada".to_vec()]);
        let page = db
            .cache_zrange_by_score_with_scores(
                &ns,
                "board",
                ScoreBound::lowest(),
                ScoreBound::highest(),
                false,
                Some((1, 1)),
            )
            .expect("paged");
        assert_eq!(page, vec![ScoredMember::new(30.0, "cy")]);

        assert_eq!(
            db.cache_zincr_by(&ns, "board", b"bob", 2.5)
                .expect("zincrby"),
            22.5
        );
        assert_eq!(
            db.cache_zrem(&ns, "board", &[b"bob", b"zed"])
                .expect("zrem"),
            1
        );
        assert_eq!(db.cache_zcard(&ns, "board").expect("zcard"), 2);

        db.cache_clear_namespace(&ns).expect("clean up");
    });
}

#[test]
fn cache_locks_fence_with_a_rising_token() {
    live(|db| {
        let ns = unique("rs_lock");
        let ttl = Duration::from_secs(30);
        let first = db
            .cache_lock_acquire(&ns, "job", "worker-1", ttl)
            .expect("acquire");
        assert!(first.acquired);
        let token = first.token.expect("a fencing token");
        assert_eq!(first.holder.as_deref(), Some("worker-1"));

        let again = db
            .cache_lock_acquire(&ns, "job", "worker-1", ttl)
            .expect("re-acquire");
        assert_eq!(
            again.token,
            Some(token),
            "a retried acquire keeps its token"
        );

        let other = db
            .cache_lock_acquire(&ns, "job", "worker-2", ttl)
            .expect("contend");
        assert!(!other.acquired);
        assert_eq!(other.token, None);
        assert_eq!(other.holder.as_deref(), Some("worker-1"));

        assert!(
            db.cache_lock_renew(&ns, "job", "worker-1", ttl)
                .expect("renew")
                .renewed
        );
        assert!(
            !db.cache_lock_renew(&ns, "job", "worker-2", ttl)
                .expect("renew")
                .renewed
        );
        assert!(!db
            .cache_lock_release(&ns, "job", "worker-2")
            .expect("release"));
        assert!(db
            .cache_lock_release(&ns, "job", "worker-1")
            .expect("release"));

        let next = db
            .cache_lock_acquire(&ns, "job", "worker-2", ttl)
            .expect("acquire");
        assert!(
            next.token.expect("a token") > token,
            "the token never repeats"
        );
        assert!(db
            .cache_lock_release(&ns, "job", "worker-2")
            .expect("release"));
        db.cache_clear_namespace(&ns).expect("clean up");
    });
}

#[test]
fn cache_bitmaps_and_hyperloglogs_count() {
    live(|db| {
        let ns = unique("rs_bits");
        assert!(!db.cache_set_bit(&ns, "a", 7, true).expect("setbit"));
        assert!(db.cache_set_bit(&ns, "a", 7, true).expect("setbit again"));
        assert!(db.cache_get_bit(&ns, "a", 7).expect("getbit"));
        assert!(!db.cache_get_bit(&ns, "a", 6).expect("getbit"));
        assert!(!db.cache_get_bit(&ns, "a", 999).expect("past the end"));
        assert_eq!(
            db.cache_get(&ns, "a").expect("get"),
            Some(vec![0b0000_0001])
        );

        db.cache_set_bit(&ns, "b", 0, true).expect("setbit");
        assert_eq!(db.cache_bit_count(&ns, "a", None).expect("bitcount"), 1);
        assert_eq!(
            db.cache_bit_count(&ns, "a", Some((0, -1)))
                .expect("bitcount"),
            1
        );
        assert_eq!(db.cache_bit_pos(&ns, "a", true, None).expect("bitpos"), 7);
        assert_eq!(
            db.cache_bit_pos(&ns, "nokey", true, None).expect("bitpos"),
            -1
        );
        assert_eq!(
            db.cache_bit_op(&ns, BitOpKind::Or, "c", &["a", "b"])
                .expect("bitop"),
            1
        );
        assert_eq!(
            db.cache_get(&ns, "c").expect("get"),
            Some(vec![0b1000_0001])
        );
        assert_eq!(
            db.cache_bit_op(&ns, BitOpKind::And, "d", &["a", "b"])
                .expect("bitop"),
            1
        );
        assert_eq!(db.cache_bit_count(&ns, "d", None).expect("bitcount"), 0);

        assert!(db
            .cache_pf_add(&ns, "h1", &[b"a", b"b", b"c"])
            .expect("pfadd"));
        assert!(db.cache_pf_add(&ns, "h2", &[b"c", b"d"]).expect("pfadd"));
        assert_eq!(db.cache_pf_count(&ns, &["h1"]).expect("pfcount"), 3);
        assert_eq!(db.cache_pf_count(&ns, &["h1", "h2"]).expect("union"), 4);
        db.cache_pf_merge(&ns, "h3", &["h1", "h2"])
            .expect("pfmerge");
        assert_eq!(db.cache_pf_count(&ns, &["h3"]).expect("pfcount"), 4);

        db.cache_clear_namespace(&ns).expect("clean up");
    });
}

#[test]
fn cache_many_key_conditional_and_atomic_operations() {
    live(|db| {
        let ns = unique("rs_multi");
        let written = db
            .cache_mset(
                &ns,
                &[
                    CacheEntry::new("a", "1"),
                    CacheEntry::new("b", [0xff, 0x00]),
                ],
                None,
            )
            .expect("mset");
        assert_eq!(written, 2);
        assert_eq!(
            db.cache_mget(&ns, &["a", "nope", "b", "a"]).expect("mget"),
            vec![
                Some(b"1".to_vec()),
                None,
                Some(vec![0xff, 0x00]),
                Some(b"1".to_vec())
            ]
        );
        let repeated = db
            .cache_mset(
                &ns,
                &[CacheEntry::new("x", "1"), CacheEntry::new("x", "2")],
                None,
            )
            .expect_err("a repeated key is refused");
        assert_eq!(repeated.kind, ErrorKind::Server);

        let warmed = db
            .cache_warm(
                &ns,
                &[CacheEntry::new("a", "9"), CacheEntry::new("w", "8")],
                None,
            )
            .expect("warm");
        assert_eq!((warmed.written, warmed.already_cached), (1, 1));
        assert_eq!(db.cache_get(&ns, "a").expect("get"), Some(b"1".to_vec()));

        assert!(!db
            .cache_set_if(&ns, "a", b"2", SetCondition::IfAbsent, CacheExpiry::Clear)
            .expect("nx"));
        assert!(!db
            .cache_set_if(
                &ns,
                "new",
                b"2",
                SetCondition::IfPresent,
                CacheExpiry::Clear
            )
            .expect("xx"));
        assert!(db
            .cache_set_if(
                &ns,
                "a",
                b"2",
                SetCondition::IfPresent,
                CacheExpiry::Set(Duration::from_secs(60))
            )
            .expect("xx with a ttl"));
        assert!(db.cache_ttl(&ns, "a").expect("ttl").is_some());
        assert!(db
            .cache_set_if(&ns, "a", b"3", SetCondition::Always, CacheExpiry::Keep)
            .expect("keepttl"));
        assert!(
            db.cache_ttl(&ns, "a").expect("ttl").is_some(),
            "the deadline was kept"
        );

        let swap = db
            .cache_compare_and_swap(&ns, "cas", None, Some(b"v1"), None)
            .expect("create");
        assert!(swap.swapped);
        let swap = db
            .cache_compare_and_swap(&ns, "cas", Some(b"wrong"), Some(b"v2"), None)
            .expect("mismatch");
        assert!(!swap.swapped);
        assert_eq!(swap.current.as_deref(), Some(&b"v1"[..]));
        let swap = db
            .cache_compare_and_swap(&ns, "b", Some(b"wrong"), None, None)
            .expect("binary mismatch");
        assert!(!swap.swapped && swap.current_is_binary);
        let swap = db
            .cache_compare_and_swap(&ns, "cas", Some(b"v1"), None, None)
            .expect("delete");
        assert!(swap.swapped);
        assert!(!db.cache_exists(&ns, "cas").expect("exists"));

        assert_eq!(
            db.cache_get_set(&ns, "gs", b"1", None).expect("getset"),
            None
        );
        assert_eq!(
            db.cache_get_set(&ns, "gs", b"2", None).expect("getset"),
            Some(b"1".to_vec())
        );
        assert_eq!(
            db.cache_get_ex(&ns, "gs", CacheExpiry::Set(Duration::from_secs(60)))
                .expect("getex"),
            Some(b"2".to_vec())
        );
        assert!(db.cache_ttl(&ns, "gs").expect("ttl").is_some());
        assert_eq!(
            db.cache_get_del(&ns, "gs").expect("getdel"),
            Some(b"2".to_vec())
        );
        assert_eq!(db.cache_get_del(&ns, "gs").expect("getdel"), None);

        assert_eq!(db.cache_append(&ns, "log", b"ab").expect("append"), 2);
        assert_eq!(db.cache_append(&ns, "log", b"cd").expect("append"), 4);
        assert_eq!(
            db.cache_get(&ns, "log").expect("get"),
            Some(b"abcd".to_vec())
        );

        let future = db
            .cache_expire_at(&ns, "log", 4_102_444_800_000)
            .expect("expireat");
        assert!(future.updated && !future.deleted);
        let past = db
            .cache_expire_at(&ns, "log", 1)
            .expect("expireat in the past");
        assert!(past.deleted);
        assert!(!db.cache_exists(&ns, "log").expect("exists"));
        assert!(
            !db.cache_expire_at(&ns, "log", 4_102_444_800_000)
                .expect("missing")
                .updated
        );

        db.cache_clear_namespace(&ns).expect("clean up");
    });
}

#[test]
fn cache_fresh_reads_hand_out_one_lease() {
    live(|db| {
        let ns = unique("rs_fresh");
        let lease = Duration::from_secs(5);
        let until = db
            .cache_set_fresh(&ns, "page", b"body", Duration::from_secs(60), None)
            .expect("setfresh");
        assert!(until > 0);
        let read = db
            .cache_get_fresh(&ns, "page", "w1", lease)
            .expect("getfresh");
        assert_eq!(read.state, FreshState::Fresh);
        assert_eq!(read.value.as_deref(), Some(&b"body"[..]));

        db.cache_set_fresh(
            &ns,
            "bin",
            &[0xff, 0xfe, 0x00],
            Duration::from_secs(60),
            None,
        )
        .expect("setfresh");
        let read = db
            .cache_get_fresh(&ns, "bin", "w1", lease)
            .expect("getfresh");
        assert_eq!(read.value, Some(vec![0xff, 0xfe, 0x00]), "binary survives");

        db.cache_set_fresh(&ns, "old", b"v", Duration::from_millis(1), None)
            .expect("setfresh");
        std::thread::sleep(Duration::from_millis(20));
        let first = db
            .cache_get_fresh(&ns, "old", "w1", lease)
            .expect("getfresh");
        let second = db
            .cache_get_fresh(&ns, "old", "w2", lease)
            .expect("getfresh");
        assert_eq!(first.state, FreshState::Stale);
        assert!(first.refresh && !second.refresh, "one caller refreshes");
        assert_eq!(second.value.as_deref(), Some(&b"v"[..]));

        let first = db
            .cache_get_fresh(&ns, "absent", "w1", lease)
            .expect("getfresh");
        let second = db
            .cache_get_fresh(&ns, "absent", "w2", lease)
            .expect("getfresh");
        assert_eq!(first.state, FreshState::Miss);
        assert!(first.lease && !second.lease, "one caller computes");
        assert!(second.retry_after_ms.is_some());

        db.cache_set_missing(&ns, "gone", Duration::from_secs(60))
            .expect("setmissing");
        let read = db
            .cache_get_fresh(&ns, "gone", "w1", lease)
            .expect("getfresh");
        assert_eq!(read.state, FreshState::Missing);

        db.cache_clear_namespace(&ns).expect("clean up");
    });
}

#[test]
fn cache_lists_sets_and_hashes_gain_their_missing_operations() {
    live(|db| {
        let ns = unique("rs_coll");
        db.cache_rpush(&ns, "src", &[b"a", b"b", b"c"])
            .expect("rpush");
        assert_eq!(
            db.cache_lmove(&ns, "src", "dst", ListEnd::Left, ListEnd::Right)
                .expect("lmove"),
            Some(b"a".to_vec())
        );
        assert_eq!(
            db.cache_lmove(&ns, "empty", "dst", ListEnd::Left, ListEnd::Right)
                .expect("lmove"),
            None
        );
        let trimmed = db.cache_ltrim(&ns, "src", 0, 0).expect("ltrim");
        assert_eq!((trimmed.length, trimmed.removed), (1, 1));
        assert_eq!(
            db.cache_lrange(&ns, "src", 0, -1).expect("lrange"),
            vec![b"b".to_vec()]
        );

        let wait = Duration::from_millis(100);
        assert_eq!(
            db.cache_blpop(&ns, &["empty", "dst"], wait).expect("blpop"),
            Some(("dst".to_string(), b"a".to_vec()))
        );
        assert_eq!(
            db.cache_brpop(&ns, &["src"], wait).expect("brpop"),
            Some(("src".to_string(), b"b".to_vec()))
        );
        assert_eq!(db.cache_brpop(&ns, &["empty"], wait).expect("brpop"), None);

        db.cache_sadd(&ns, "s1", &[b"a", b"b"]).expect("sadd");
        db.cache_sadd(&ns, "s2", &[b"b", b"c"]).expect("sadd");
        assert_eq!(
            db.cache_sunion(&ns, &["s1", "s2", "none"])
                .expect("sunion")
                .len(),
            3
        );
        assert_eq!(
            db.cache_sinter(&ns, &["s1", "s2"]).expect("sinter"),
            vec![b"b".to_vec()]
        );
        assert_eq!(
            db.cache_sdiff(&ns, &["s1", "s2"]).expect("sdiff"),
            vec![b"a".to_vec()]
        );
        assert_eq!(
            db.cache_srand_member(&ns, "s1", None)
                .expect("srandmember")
                .len(),
            1
        );
        assert_eq!(
            db.cache_srand_member(&ns, "s1", Some(5))
                .expect("distinct")
                .len(),
            2
        );
        assert_eq!(
            db.cache_srand_member(&ns, "s1", Some(-5))
                .expect("repeats")
                .len(),
            5
        );

        assert_eq!(db.cache_hincr_by(&ns, "h", b"n", 5).expect("hincrby"), 5);
        assert_eq!(db.cache_hincr_by(&ns, "h", b"n", -2).expect("hincrby"), 3);

        db.cache_clear_namespace(&ns).expect("clean up");
    });
}

#[test]
fn cache_pipelines_transactions_and_scripts_report_each_step() {
    live(|db| {
        let ns = unique("rs_pipe");
        let replies = db
            .cache_pipeline(&[
                CacheCommand::set(&ns, "k", b"text", None),
                CacheCommand::get(&ns, "k"),
                CacheCommand::incr(&ns, "k", 1),
                CacheCommand::unit("Ping"),
                CacheCommand::exists(&ns, "k"),
            ])
            .expect("pipeline");
        assert_eq!(replies.len(), 5);
        assert_eq!(replies[1].value(), Some(b"text".to_vec()));
        assert!(
            matches!(&replies[2], CacheReply::Error(_)),
            "a failing step is reported and the rest still run"
        );
        assert!(replies[3].is_ok() && replies[4].is_ok());

        let replies = db
            .cache_transaction(&[
                CacheCommand::incr(&ns, "n", 2),
                CacheCommand::incr(&ns, "n", 3),
                CacheCommand::get(&ns, "n"),
                CacheCommand::delete(&ns, "k"),
            ])
            .expect("transaction");
        assert_eq!(replies[2].value(), Some(b"5".to_vec()));
        assert!(!db.cache_exists(&ns, "k").expect("exists"));

        let script = unique("rs_script");
        db.cache_script_load(
            &script,
            &[
                CacheCommand::set(&ns, "$1", b"1", None),
                CacheCommand::incr(&ns, "$2", 1),
            ],
        )
        .expect("script load");
        let replies = db
            .cache_script_run(&script, &["first", "second"])
            .expect("script run");
        assert_eq!(replies.len(), 2);
        assert!(replies.iter().all(CacheReply::is_ok));
        assert_eq!(
            db.cache_get(&ns, "first").expect("get"),
            Some(b"1".to_vec())
        );
        assert_eq!(db.cache_incr(&ns, "second", 0).expect("incr"), 1);
        assert!(db.cache_script_run("rs_no_such_script", &[]).is_err());

        db.cache_clear_namespace(&ns).expect("clean up");
    });
}

#[test]
fn cache_publish_poll_limits_and_node_reads() {
    live(|db| {
        let channel = unique("rs_chan");
        let first = db.cache_publish(&channel, "one").expect("publish");
        let second = db.cache_publish(&channel, "two").expect("publish");
        assert!(second > first);

        let all = db.cache_poll(&[&channel], 0, None).expect("poll");
        let texts: Vec<&str> = all.messages.iter().map(|m| m.message.as_str()).collect();
        assert_eq!(texts, ["one", "two"]);
        assert_eq!(all.next, second);
        let after = db
            .cache_poll(&[&channel], first, Some(1))
            .expect("poll after");
        assert_eq!(after.messages.len(), 1);
        assert_eq!(after.messages[0].message, "two");
        assert_eq!(after.messages[0].channel, channel);
        let pattern = format!("{}*", &channel[..channel.len() - 1]);
        assert!(!db
            .cache_poll(&[&pattern], 0, None)
            .expect("pattern")
            .messages
            .is_empty());
        assert!(db
            .cache_poll(&[&channel], second, None)
            .expect("drained")
            .messages
            .is_empty());

        let ns = unique("rs_limit");
        let limit = db
            .cache_set_namespace_limit(&ns, Some(1), None)
            .expect("set limit");
        assert_eq!(
            (limit.max_keys, limit.max_bytes, limit.keys),
            (Some(1), None, 0)
        );
        db.cache_set(&ns, "a", b"1").expect("the first key fits");
        assert!(
            db.cache_set(&ns, "b", b"1").is_err(),
            "the second passes the cap"
        );
        let held = db.cache_get_namespace_limit(&ns).expect("get limit");
        assert_eq!((held.keys, held.bytes, held.max_keys), (1, 1, Some(1)));
        let lifted = db.cache_set_namespace_limit(&ns, None, None).expect("lift");
        assert_eq!(lifted.max_keys, None);
        db.cache_set(&ns, "b", b"1").expect("unlimited again");
        db.cache_clear_namespace(&ns).expect("clean up");

        let stats = db.cache_stats().expect("stats");
        assert!(!stats.eviction.policy.is_empty());
        assert!(db.cache_purge_expired().expect("purge") >= 0);

        // The shared server has hot-key tracking off, and says so by name.
        if let Err(refusal) = db.cache_hot_keys(Some(3)) {
            assert!(refusal.message().contains("hot_keys"), "{refusal}");
        }
    });
}

#[test]
fn cache_flush_empties_one_database_and_no_other() {
    let Some(options) = options() else { return };
    // A database of this test's own, so nothing another client holds is
    // within reach of the flush.
    let scratch = unique("rs_flushdb");
    let mut main = Client::connect(&options).expect("connect");
    let mut other = Client::connect(&options.clone().database(&scratch)).expect("connect");

    let ns = unique("rs_flush");
    main.cache_set(&ns, "kept", b"1").expect("set in main");
    other.cache_set(&ns, "a", b"1").expect("set");
    other.cache_set("rs_flush_second", "b", b"2").expect("set");

    assert_eq!(other.cache_flush_database().expect("flush"), 2);
    assert_eq!(other.cache_get(&ns, "a").expect("get"), None);
    assert_eq!(
        main.cache_get(&ns, "kept").expect("get"),
        Some(b"1".to_vec())
    );
    main.cache_clear_namespace(&ns).expect("clean up");
}

#[test]
fn documents_replace_batch_and_bulk_write() {
    live(|db| {
        let coll = unique("rs_docs");
        db.document_create_collection(&coll).expect("create");

        let ids = db
            .document_insert_many(
                &coll,
                &[
                    json!({"_id": "a", "n": 1, "kind": "x"}),
                    json!({"_id": "b", "n": 2, "kind": "x"}),
                    json!({"n": 3, "kind": "y"}),
                ],
            )
            .expect("insert many");
        assert_eq!(&ids[..2], ["a", "b"]);
        assert!(!ids[2].is_empty());
        let duplicate = db.document_insert_many(&coll, &[json!({"_id": "z"}), json!({"_id": "a"})]);
        assert!(duplicate.is_err());
        assert!(
            db.document_get(&coll, "z").expect("get").is_none(),
            "none of a failed batch landed"
        );

        let before = ReplaceOptions {
            return_document: ReturnDocument::Before,
            ..ReplaceOptions::default()
        };
        let replaced = db
            .document_replace(&coll, "a", &json!({"n": 10}), &before)
            .expect("replace");
        assert!(replaced.replaced && !replaced.inserted);
        assert_eq!(replaced.document.expect("the before-image")["kind"], "x");
        let stored = db.document_get(&coll, "a").expect("get").expect("present");
        assert!(!stored.contains_key("kind"), "a replace does not merge");
        assert!(db
            .document_replace(&coll, "nope", &json!({"n": 1}), &ReplaceOptions::default())
            .is_err());
        let upsert = ReplaceOptions {
            upsert: true,
            return_document: ReturnDocument::After,
            ..ReplaceOptions::default()
        };
        let created = db
            .document_replace(&coll, "new", &json!({"n": 7}), &upsert)
            .expect("upsert");
        assert!(created.inserted);
        assert_eq!(created.document.expect("the after-image")["n"], 7);

        let result = db
            .document_bulk_write(
                &coll,
                &[
                    BulkOp::insert_with_id("w1", json!({"n": 1})),
                    BulkOp::insert(json!({"n": 2})),
                    BulkOp::update_one("w1", DocumentUpdate::new().inc("n", 4)),
                    BulkOp::update_one("w2", DocumentUpdate::new().set("n", 6)).upsert(),
                    BulkOp::replace("b", json!({"n": 20})),
                    BulkOp::delete("new"),
                ],
            )
            .expect("bulk write");
        assert_eq!(
            (
                result.inserted,
                result.updated,
                result.upserted,
                result.replaced,
                result.deleted
            ),
            (2, 1, 1, 1, 1)
        );
        assert_eq!(result.inserted_ids[0], "w1");
        assert_eq!(
            db.document_get(&coll, "w1").expect("get").expect("present")["n"],
            5
        );
        assert!(db.document_get(&coll, "new").expect("get").is_none());

        let failing = db.document_bulk_write(
            &coll,
            &[
                BulkOp::insert_with_id("w9", json!({"n": 1})),
                BulkOp::replace("absent", json!({})),
            ],
        );
        assert!(failing.is_err());
        assert!(
            db.document_get(&coll, "w9").expect("get").is_none(),
            "the batch is atomic"
        );

        assert_eq!(
            db.document_delete_many(&coll, &DocumentFilter::eq("kind", "y"))
                .expect("delete many"),
            1
        );
        db.document_drop_collection(&coll).expect("drop");
    });
}

#[test]
fn document_transactions_isolate_until_commit() {
    live(|db| {
        let coll = unique("rs_txn");
        db.document_create_collection(&coll).expect("create");

        let txn = db.document_begin_transaction(false).expect("begin");
        db.set_request_txn(Some(&txn));
        db.document_insert_with_id(&coll, "t1", &json!({"n": 1}))
            .expect("insert inside");
        assert!(db.document_get(&coll, "t1").expect("get inside").is_some());
        db.set_request_txn(None);
        assert!(db.document_get(&coll, "t1").expect("get outside").is_none());
        db.set_request_txn(Some(&txn));
        let commit = db.document_commit_transaction(&txn).expect("commit");
        assert!(commit.committed);
        assert!(commit.writes >= 1);
        assert_eq!(
            db.request_txn(),
            None,
            "a commit stops naming the transaction"
        );
        assert!(db.document_get(&coll, "t1").expect("get").is_some());

        let txn = db
            .document_begin_transaction(true)
            .expect("begin serializable");
        db.set_request_txn(Some(&txn));
        db.document_insert_with_id(&coll, "t2", &json!({"n": 2}))
            .expect("insert inside");
        db.document_rollback_transaction(&txn).expect("rollback");
        assert!(db.document_get(&coll, "t2").expect("get").is_none());
        assert!(db.document_commit_transaction(&txn).is_err(), "it is over");

        db.document_drop_collection(&coll).expect("drop");
    });
}

#[test]
fn document_collection_options_explain_and_maintenance() {
    live(|db| {
        let coll = unique("rs_opts");
        db.document_create_collection(&coll).expect("create");
        db.document_insert_many(&coll, &[json!({"_id": "a", "n": 1, "kind": "x"})])
            .expect("insert");
        assert_eq!(
            db.document_get_collection_options(&coll).expect("options"),
            CollectionOptions::default()
        );

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
        assert_eq!(
            db.document_set_collection_options(&coll, &options)
                .expect("set"),
            1
        );
        assert_eq!(
            db.document_get_collection_options(&coll).expect("get"),
            options
        );

        assert!(
            db.document_insert(&coll, &json!({"kind": "q"})).is_err(),
            "`n` is required"
        );
        assert!(
            db.document_insert(&coll, &json!({"n": -1})).is_err(),
            "`n` has a minimum"
        );
        db.document_insert_with_id(&coll, "d", &json!({"n": 5}))
            .expect("insert");
        let stored = db.document_get(&coll, "d").expect("get").expect("present");
        assert_eq!(stored["kind"], "z", "the default was applied");
        assert_eq!(stored["_version"], 1);

        let stale = ReplaceOptions {
            expected_version: Some(99),
            ..ReplaceOptions::default()
        };
        assert!(db
            .document_replace(&coll, "d", &json!({"n": 6}), &stale)
            .is_err());
        let current = ReplaceOptions {
            expected_version: Some(1),
            return_document: ReturnDocument::After,
            ..ReplaceOptions::default()
        };
        let replaced = db
            .document_replace(&coll, "d", &json!({"n": 6}), &current)
            .expect("replace");
        assert_eq!(replaced.document.expect("after")["_version"], 2);
        assert!(db
            .document_bulk_write(&coll, &[BulkOp::delete("d").expected_version(1)])
            .is_err());
        db.document_bulk_write(&coll, &[BulkOp::delete("d").expected_version(2)])
            .expect("a matching version deletes");

        let refused = CollectionOptions {
            schema: Some(DocumentSchema {
                required: vec!["absent".into()],
                ..DocumentSchema::default()
            }),
            ..CollectionOptions::default()
        };
        assert!(db.document_set_collection_options(&coll, &refused).is_err());

        let scan = db
            .document_explain(
                &coll,
                &DocumentFilter::eq("kind", "x"),
                &ExplainOptions::default(),
            )
            .expect("explain");
        assert!(!scan.index_used);
        assert!(scan.scan_reason.is_some());
        db.document_create_index(&coll, "by_kind", "kind", false)
            .expect("index");
        let indexed = db
            .document_explain(
                &coll,
                &DocumentFilter::eq("kind", "x"),
                &ExplainOptions {
                    limit: Some(5),
                    ..ExplainOptions::default()
                },
            )
            .expect("explain");
        assert!(indexed.index_used, "{indexed:?}");
        assert_eq!(indexed.index.as_deref(), Some("by_kind"));
        assert_eq!(indexed.rows_returned, 1);

        let rebuilt = db
            .document_rebuild_index(&coll, "by_kind")
            .expect("rebuild");
        assert_eq!(rebuilt.index_name, "by_kind");
        assert_eq!(rebuilt.documents, 1);
        assert_eq!(db.document_purge_expired(&coll).expect("purge").purged, 0);

        let renamed = unique("rs_opts_renamed");
        db.document_rename_collection(&coll, &renamed)
            .expect("rename");
        assert!(db.document_get(&renamed, "a").expect("get").is_some());
        assert!(db.document_rename_collection(&coll, &renamed).is_err());
        db.document_drop_collection(&renamed).expect("drop");
    });
}

#[test]
fn vectors_batch_filter_count_and_search_by_example() {
    live(|db| {
        let coll = unique("rs_vec");
        db.vector_create_collection(&coll, 3, VectorMetric::Cosine)
            .expect("create");
        let written = db
            .vector_upsert_many(
                &coll,
                &[
                    VectorUpsert::new("a", [1.0, 0.0, 0.0])
                        .metadata(object(json!({"kind": "x", "_namespace": "n1"}))),
                    VectorUpsert::new("b", [0.0, 1.0, 0.0])
                        .metadata(object(json!({"kind": "y", "_namespace": "n1"}))),
                    VectorUpsert::new("c", [0.0, 0.0, 1.0]),
                ],
            )
            .expect("upsert many");
        assert_eq!(written, 3);
        let repeated = db.vector_upsert_many(
            &coll,
            &[
                VectorUpsert::new("r", [1.0, 0.0, 0.0]),
                VectorUpsert::new("r", [0.0, 1.0, 0.0]),
            ],
        );
        assert!(repeated.is_err());

        assert_eq!(db.vector_count(&coll, None).expect("count"), 3);
        assert_eq!(
            db.vector_count(&coll, Some(&json!({"kind": "x"})))
                .expect("count"),
            1
        );

        let lists = db
            .vector_search_batch(
                &coll,
                &[vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]],
                1,
                &VectorSearchOptions::default(),
            )
            .expect("search batch");
        assert_eq!(lists.len(), 2);
        assert_eq!(
            (lists[0][0].id.as_str(), lists[1][0].id.as_str()),
            ("a", "b")
        );
        let filtered = db
            .vector_search_batch(
                &coll,
                &[vec![1.0, 0.0, 0.0]],
                3,
                &VectorSearchOptions {
                    filter: Some(json!({"kind": "y"})),
                    include_vectors: true,
                    explain: true,
                    ..VectorSearchOptions::default()
                },
            )
            .expect("filtered batch");
        assert_eq!(filtered[0].len(), 1);
        assert_eq!(filtered[0][0].id, "b");
        assert_eq!(filtered[0][0].vector, Some(vec![0.0, 1.0, 0.0]));
        assert!(filtered[0][0].reason.is_some());

        let fused = db
            .vector_search_multi(
                &coll,
                &[vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]],
                3,
                FusionMethod::Max,
                None,
            )
            .expect("search multi");
        assert_eq!(fused.fusion, FusionMethod::Max);
        assert_eq!(fused.queries, 2);
        assert_eq!(fused.results.len(), 3);

        let by_example = db
            .vector_search_by_examples(
                &coll,
                &[VectorExample::id("a")],
                &[],
                3,
                true,
                &VectorSearchOptions::default(),
            )
            .expect("by examples");
        assert!(
            by_example.results.iter().all(|hit| hit.id != "a"),
            "the example is left out"
        );
        let with_example = db
            .vector_search_by_examples(
                &coll,
                &[VectorExample::Vector(vec![1.0, 0.0, 0.0])],
                &[VectorExample::id("c")],
                1,
                false,
                &VectorSearchOptions::default(),
            )
            .expect("by examples");
        assert_eq!(with_example.results[0].id, "a");

        assert_eq!(
            db.vector_update_by_filter(
                &coll,
                &json!({"kind": "x"}),
                &object(json!({"seen": true}))
            )
            .expect("update by filter"),
            1
        );
        assert_eq!(
            db.vector_get(&coll, "a")
                .expect("get")
                .expect("present")
                .metadata["seen"],
            true
        );
        let partitions = db.vector_list_partitions(&coll, None).expect("partitions");
        assert_eq!(partitions.field, "_namespace");
        assert_eq!(
            (
                partitions.partitions[0].name.as_str(),
                partitions.partitions[0].count
            ),
            ("n1", 2)
        );
        assert_eq!(partitions.unpartitioned, 1);

        let copy = unique("rs_vec_copy");
        assert_eq!(db.vector_copy_collection(&coll, &copy).expect("copy"), 3);
        assert!(db.vector_copy_collection(&coll, &copy).is_err());
        let renamed = unique("rs_vec_renamed");
        assert_eq!(
            db.vector_rename_collection(&copy, &renamed)
                .expect("rename"),
            3
        );
        assert_eq!(
            db.vector_delete_many(&renamed, &["a", "absent"])
                .expect("delete many"),
            1
        );
        assert_eq!(
            db.vector_delete_by_filter(&renamed, &json!({"kind": "y"}))
                .expect("delete by filter"),
            1
        );
        assert!(
            db.vector_delete_by_filter(&renamed, &json!({})).is_err(),
            "an empty filter is refused"
        );
        assert_eq!(db.vector_count(&renamed, None).expect("count"), 1);
        assert_eq!(
            db.vector_count(&coll, None).expect("count"),
            3,
            "the original is untouched"
        );

        db.vector_drop_collection(&renamed).expect("drop");
        db.vector_drop_collection(&coll).expect("drop");
    });
}

#[test]
fn vectors_named_aliases_text_and_index_maintenance() {
    live(|db| {
        let embedding = db
            .vector_embed(&["hello world", "goodbye"], 4)
            .expect("embed");
        assert_eq!(embedding.dimension, 4);
        assert_eq!(embedding.vectors.len(), 2);
        assert_eq!(embedding.vectors[0].len(), 4);
        assert_eq!(embedding.sparse.len(), 2);

        let coll = unique("rs_named");
        db.vector_create_collection(&coll, 3, VectorMetric::Cosine)
            .expect("create");
        let config = db
            .vector_set_collection_config(
                &coll,
                &VectorConfigChange {
                    normalize: Some(true),
                    m: Some(8),
                    ef_construction: Some(100),
                    multi_tenant: None,
                },
            )
            .expect("config");
        assert_eq!(
            (config.normalize, config.m, config.ef_construction),
            (true, 8, 100)
        );

        assert_eq!(
            db.vector_upsert_text(
                &coll,
                &[
                    TextUpsert::new("t1", "hello world").metadata(object(json!({"kind": "t"}))),
                    TextUpsert::new("t2", "goodbye"),
                ],
            )
            .expect("upsert text"),
            2
        );
        let stored = db.vector_get(&coll, "t1").expect("get").expect("present");
        assert_eq!(stored.metadata["text"], "hello world");
        assert_eq!(stored.vector.len(), 3);
        assert!(
            db.vector_set_collection_config(
                &coll,
                &VectorConfigChange {
                    normalize: Some(false),
                    ..VectorConfigChange::default()
                }
            )
            .is_err(),
            "normalize only changes on an empty collection"
        );

        let alias = unique("rs_alias");
        db.vector_set_alias(&alias, Some(&coll)).expect("set alias");
        assert!(db
            .vector_list_aliases()
            .expect("aliases")
            .iter()
            .any(|a| a.alias == alias && a.collection == coll));
        assert_eq!(
            db.vector_count(&alias, None)
                .expect("count through the alias"),
            2
        );
        db.vector_set_alias(&alias, None).expect("remove alias");
        assert!(db
            .vector_list_aliases()
            .expect("aliases")
            .iter()
            .all(|a| a.alias != alias));

        db.vector_add_named_vector(&coll, "title", 2, VectorMetric::Dot)
            .expect("named vector");
        let vectors = BTreeMap::from([
            ("default".to_string(), vec![1.0, 1.0, 0.0]),
            ("title".to_string(), vec![1.0, 0.0]),
        ]);
        db.vector_upsert_named(
            &coll,
            "n1",
            &vectors,
            Some(&object(json!({"kind": "named"}))),
        )
        .expect("upsert named");
        let config = db
            .vector_set_collection_config(
                &coll,
                &VectorConfigChange {
                    m: Some(8),
                    ..VectorConfigChange::default()
                },
            )
            .expect("config");
        assert_eq!(config.named.len(), 1);
        assert_eq!(
            (config.named[0].name.as_str(), config.named[0].dimension),
            ("title", 2)
        );
        assert_eq!(config.named[0].metric, VectorMetric::Dot);

        let stats = db.vector_index_stats(&coll).expect("index stats");
        assert_eq!(stats.index, "flat");
        assert_eq!(stats.stored, 3);
        let rebuilt = db.vector_rebuild_index(&coll).expect("rebuild");
        assert!(rebuilt.rebuilt);
        assert_eq!(rebuilt.vectors, 3);
        let recall = db
            .vector_measure_recall(&coll, &RecallOptions::default())
            .expect("recall");
        assert!(
            !recall.measured,
            "a scanned collection has nothing to measure"
        );
        assert_eq!(recall.recall, 1.0);

        db.vector_drop_collection(&coll).expect("drop");
    });
}

#[test]
fn a_graph_index_is_measured_once_a_collection_has_one() {
    live(|db| {
        let coll = unique("rs_hnsw");
        db.vector_create_collection(&coll, 4, VectorMetric::Cosine)
            .expect("create");
        let items: Vec<VectorUpsert> = (0..300)
            .map(|i| {
                let x = i as f32;
                VectorUpsert::new(&format!("v{i}"), [x.sin(), x.cos(), (x * 0.5).sin(), 1.0])
            })
            .collect();
        assert_eq!(
            db.vector_upsert_many(&coll, &items).expect("upsert many"),
            300
        );

        let stats = db.vector_index_stats(&coll).expect("index stats");
        assert_eq!(stats.index, "hnsw");
        assert_eq!(stats.live, Some(300));
        assert!(stats.layers.expect("layers") >= 1);
        let recall = db
            .vector_measure_recall(
                &coll,
                &RecallOptions {
                    sample: Some(5),
                    top_k: Some(3),
                    ef_search: None,
                },
            )
            .expect("recall");
        assert!(recall.measured);
        assert_eq!(recall.queries, Some(5));
        assert!(recall.recall > 0.0 && recall.recall <= 1.0);

        db.vector_drop_collection(&coll).expect("drop");
    });
}

/// A triangle a→b→c→a with a shortcut a→c, and an isolated node d.
fn seed_graph(db: &mut Client, graph: &str) {
    db.graph_create(graph).expect("create graph");
    let counts = db
        .graph_add_many(
            graph,
            &[
                GraphNodeInput::new("a", &["P"])
                    .properties(object(json!({"name": "ada", "age": 30}))),
                GraphNodeInput::new("b", &["P"]).properties(object(json!({"name": "bob"}))),
                GraphNodeInput::new("c", &["P"]),
                GraphNodeInput::new("d", &[]),
            ],
            &[
                GraphEdgeInput::new("e1", "a", "b", "K").properties(object(json!({"w": 2}))),
                GraphEdgeInput::new("e2", "b", "c", "K"),
                GraphEdgeInput::new("e3", "c", "a", "K"),
                GraphEdgeInput::new("e4", "a", "c", "K"),
            ],
        )
        .expect("add many");
    assert_eq!((counts.nodes, counts.edges), (4, 4));
}

#[test]
fn graphs_bulk_write_update_copy_and_remove() {
    live(|db| {
        let graph = unique("rs_graph");
        seed_graph(db, &graph);
        let dangling =
            db.graph_add_many(&graph, &[], &[GraphEdgeInput::new("x", "a", "nobody", "K")]);
        assert!(dangling.is_err());

        let node = db
            .graph_update_node(
                &graph,
                "a",
                &NodeUpdate {
                    properties: Some(json!({"age": null, "city": "x"})),
                    add_labels: vec!["Q".into()],
                    remove_labels: vec!["P".into()],
                },
            )
            .expect("update node");
        assert_eq!(node.labels, vec!["Q"]);
        assert!(!node.properties.contains_key("age"));
        assert_eq!(node.properties["city"], "x");
        assert!(db
            .graph_update_node(&graph, "nobody", &NodeUpdate::default())
            .is_err());

        let edge = db
            .graph_update_edge(&graph, "e1", Some(&json!({"w": 3})), Some("KNOWS"))
            .expect("update edge");
        assert_eq!(
            (edge.label.as_str(), edge.from.as_str(), edge.to.as_str()),
            ("KNOWS", "a", "b")
        );
        assert_eq!(edge.properties["w"], 3);
        db.graph_update_edge(&graph, "e1", None, Some("K"))
            .expect("rename back");

        let imported = db
            .graph_import_csv(
                &graph,
                Some("id,labels,name\nx1,P,xavier\nx2,P,yan\n"),
                Some("id,from,to,label\nxe1,x1,x2,K\n"),
            )
            .expect("import csv");
        assert_eq!((imported.nodes, imported.edges), (2, 1));

        let copy = unique("rs_graph_copy");
        let copied = db.graph_copy(&graph, &copy).expect("copy");
        assert_eq!((copied.nodes, copied.edges), (6, 5));
        assert!(db.graph_copy(&graph, &copy).is_err());

        let removed = db
            .graph_remove_many(&copy, &["a", "nobody"], &["e2"], true)
            .expect("remove many");
        assert_eq!(removed.nodes_removed, 1);
        assert_eq!(
            removed.edges_removed, 4,
            "e2 and the three edges touching a"
        );
        assert!(
            db.graph_get_node(&graph, "a").expect("get").is_some(),
            "the original is untouched"
        );

        db.graph_drop(&copy).expect("drop");
        db.graph_drop(&graph).expect("drop");
    });
}

#[test]
fn graphs_walk_and_run_their_algorithms() {
    live(|db| {
        let graph = unique("rs_algo");
        seed_graph(db, &graph);

        let dfs = db
            .graph_dfs(&graph, "a", &TraverseOptions::default())
            .expect("dfs");
        assert_eq!(dfs.count, 3);
        assert_eq!(dfs.nodes[0].id, "a");
        let bounded = db
            .graph_dfs(
                &graph,
                "a",
                &TraverseOptions {
                    direction: Some(GraphDirection::Both),
                    limit: Some(2),
                    ..TraverseOptions::default()
                },
            )
            .expect("dfs");
        assert!(bounded.truncated);

        let sub = db
            .graph_subgraph(&graph, &["a"], &TraverseOptions::default())
            .expect("subgraph");
        assert_eq!((sub.node_count, sub.edge_count), (3, 4));

        let paths = db
            .graph_k_shortest_paths(&graph, "a", "c", &KPathOptions::default())
            .expect("k shortest");
        assert_eq!(paths.count, 2);
        assert_eq!(paths.paths[0].edges, vec!["e4"]);
        assert_eq!(paths.paths[1].hops, 2);
        let weighted = db
            .graph_k_shortest_paths(
                &graph,
                "a",
                "c",
                &KPathOptions {
                    k: Some(2),
                    weight_property: Some("w".into()),
                    ..KPathOptions::default()
                },
            )
            .expect("k shortest weighted");
        assert_eq!(weighted.paths[1].cost, 3.0);
        assert_eq!(
            db.graph_k_shortest_paths(&graph, "a", "d", &KPathOptions::default())
                .expect("none")
                .count,
            0
        );
        let all = db
            .graph_all_paths(&graph, "a", "c", &TraverseOptions::default())
            .expect("all paths");
        assert_eq!(all.count, 2);
        assert_eq!(all.complete, Some(true));

        let components = db
            .graph_connected_components(&graph, &ComponentOptions::default())
            .expect("components");
        assert_eq!((components.count, components.largest), (2, 3));
        assert_eq!(components.components[0].nodes, vec!["a", "b", "c"]);

        let cycle = db.graph_detect_cycle(&graph, None, None).expect("cycle");
        assert!(cycle.has_cycle);
        assert_eq!(cycle.cycle.expect("evidence").nodes.len(), 3);
        assert!(
            !db.graph_detect_cycle(&graph, Some(false), Some("NOPE"))
                .expect("cycle")
                .has_cycle
        );

        let stats = db.graph_stats(&graph).expect("stats");
        assert_eq!((stats.nodes, stats.edges, stats.isolated_nodes), (4, 4, 1));
        assert_eq!(stats.node_labels["P"], 3);
        assert_eq!(stats.edge_types["K"], 4);
        let schema = db.graph_schema(&graph).expect("schema");
        assert_eq!(schema.edge_types[0].edge_type, "K");
        assert_eq!(schema.unlabeled_nodes.count, 1);
        let check = db.graph_check_consistency(&graph).expect("consistency");
        assert!(check.consistent);
        assert_eq!((check.nodes, check.edges), (4, 4));

        for measure in [
            CentralityMeasure::Betweenness,
            CentralityMeasure::Closeness,
            CentralityMeasure::Eigenvector,
        ] {
            let scores = db
                .graph_centrality(&graph, measure, &CentralityOptions::default())
                .expect("centrality");
            assert_eq!(scores.measure, measure);
            assert_eq!(scores.results.len(), 4);
        }
        let communities = db
            .graph_communities(&graph, &CommunityOptions::default())
            .expect("communities");
        assert_eq!(communities.algorithm, "louvain");
        assert_eq!(communities.count, 2);
        let triangles = db
            .graph_triangle_count(&graph, None, None)
            .expect("triangles");
        assert_eq!(triangles.triangles, 1);
        let similar = db
            .graph_node_similarity(&graph, "a", &SimilarityOptions::default())
            .expect("similarity");
        assert_eq!(similar.len(), 2);
        let ranks = db
            .graph_page_rank(
                &graph,
                &PageRankOptions {
                    limit: Some(2),
                    ..PageRankOptions::default()
                },
            )
            .expect("page rank");
        assert_eq!(ranks.ranks.len(), 2);
        assert_eq!(ranks.count, 4);
        assert!(ranks.truncated);

        db.graph_drop(&graph).expect("drop");
    });
}

#[test]
fn graphs_keep_schema_objects_and_queue_transactions() {
    live(|db| {
        let graph = unique("rs_schema");
        seed_graph(db, &graph);

        db.graph_create_node_index(&graph, "by_name", Some("P"), &["name"])
            .expect("index");
        db.graph_create_node_index(&graph, "by_nc", None, &["name", "city"])
            .expect("composite");
        db.graph_create_constraint(
            &graph,
            "uniq_name",
            "P",
            "name",
            ConstraintKind::Unique,
            ConstraintTarget::Node,
        )
        .expect("constraint");
        let broken = db.graph_create_constraint(
            &graph,
            "req_w",
            "K",
            "w",
            ConstraintKind::Required,
            ConstraintTarget::Edge,
        );
        assert!(broken.is_err(), "a stored edge already breaks it");

        let objects = db.graph_list_schema_objects(&graph).expect("list");
        assert_eq!(objects.indexes.len(), 2);
        assert_eq!(objects.constraints.len(), 1);
        assert_eq!(objects.constraints[0].kind, ConstraintKind::Unique);
        assert_eq!(objects.constraints[0].target, ConstraintTarget::Node);

        let found = db
            .graph_find_nodes(
                &graph,
                Some("P"),
                Some(&object(json!({"name": "ada"}))),
                None,
            )
            .expect("find");
        assert_eq!(found.count, 1);
        assert_eq!(found.nodes[0].id, "a");
        assert!(found.plan.contains("by_name"), "{}", found.plan);
        assert_eq!(
            db.graph_find_nodes(&graph, None, None, Some(2))
                .expect("find")
                .count,
            2
        );
        assert!(
            db.graph_add_node(&graph, "dup", &["P"], Some(&object(json!({"name": "ada"}))))
                .is_err(),
            "the constraint is enforced"
        );

        db.graph_drop_schema_object(&graph, "by_nc")
            .expect("drop index");
        assert!(db.graph_drop_schema_object(&graph, "by_nc").is_err());
        assert_eq!(
            db.graph_list_schema_objects(&graph)
                .expect("list")
                .indexes
                .len(),
            1
        );

        let written = db
            .graph_execute(&graph, "CREATE (n:P {name: 'zed'})")
            .expect("execute");
        assert_eq!((written.nodes_created, written.statements), (1, 1));
        let by_name = |db: &mut Client, name: &str| {
            db.graph_find_nodes(&graph, None, Some(&object(json!({"name": name}))), None)
                .expect("find")
                .count
        };
        assert_eq!(by_name(db, "zed"), 1);

        let txn = db.graph_begin_transaction(&graph).expect("begin");
        db.set_request_txn(Some(&txn));
        let queued = db
            .graph_execute(&graph, "CREATE (n:P {name: 'tx1'})")
            .expect("queue");
        assert_eq!(queued.queued, Some(1));
        assert_eq!(queued.nodes_created, 0, "nothing is applied yet");
        db.set_request_txn(None);
        assert_eq!(by_name(db, "tx1"), 0);
        let commit = db.graph_commit_transaction(&graph, &txn).expect("commit");
        assert_eq!((commit.committed, commit.nodes_created), (Some(1), 1));
        assert_eq!(by_name(db, "tx1"), 1);

        let txn = db.graph_begin_transaction(&graph).expect("begin");
        db.set_request_txn(Some(&txn));
        db.graph_execute(&graph, "CREATE (n:P {name: 'tx2'})")
            .expect("queue");
        assert_eq!(
            db.graph_rollback_transaction(&graph, &txn)
                .expect("rollback"),
            1
        );
        assert_eq!(db.request_txn(), None);
        assert_eq!(by_name(db, "tx2"), 0);
        assert!(
            db.graph_commit_transaction(&graph, &txn).is_err(),
            "it is over"
        );

        db.graph_drop(&graph).expect("drop");
    });
}
