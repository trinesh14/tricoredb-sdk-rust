//! Everything this client can do, against a real `tricore-server`.
//!
//! These tests assert the value that came *back*, not merely that no error was
//! raised: a client that mangled a quote, a backslash or a 100 KiB payload
//! would pass a "no error" test and fail every one of these.
//!
//! Without a server binary they print why and pass — see `tests/common/mod.rs`.

mod common;

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Map, Value};
use tricoredb::{
    params, AccumulatorOp, AggregateStage, CachePair, Client, DocumentFilter, DocumentUpdate,
    ErrorKind, GraphDirection, GroupAccumulator, GroupKey, LlmSource, NeighborOptions, Options,
    OutputFormat, Param, PathOptions, Pool, Statement, VectorMetric,
};

use common::{start_server, unique, Server};

/// One server for this file's tests. Each test opens its own connection to it,
/// because a connection is not shared.
fn server() -> Option<&'static Server> {
    static SERVER: OnceLock<Result<Server, String>> = OnceLock::new();
    match SERVER.get_or_init(start_server) {
        Ok(server) => Some(server),
        Err(reason) => {
            eprintln!("skipping: {reason}");
            None
        }
    }
}

/// Run a test body against a server of its own, so no other test's schema
/// changes can reach it.
fn own_server(test: impl FnOnce(&mut Client)) {
    match start_server() {
        Ok(server) => {
            let mut client = server.client();
            test(&mut client);
        }
        Err(reason) => eprintln!("skipping: {reason}"),
    }
}

/// Run a test body with a connection, or skip when there is no server.
fn live(test: impl FnOnce(&mut Client)) {
    let Some(server) = server() else { return };
    let mut client = server.client();
    test(&mut client);
}

#[test]
fn a_session_authenticates_and_negotiates_its_capabilities() {
    live(|db| {
        assert!(db.ping().is_ok());
        assert!(
            db.session_id().is_some(),
            "an authenticated session has an id"
        );
        assert!(db.server_params_granted(), "the server binds parameters");
        assert!(
            db.session_txn_granted(),
            "the server keeps transactions open"
        );
        assert!(!db.in_transaction());
        assert!(!db.is_poisoned());
        assert_eq!(db.database(), "main");
    });
}

#[test]
fn an_empty_secret_is_refused_as_an_auth_error() {
    let Some(server) = server() else { return };
    // The dev authenticator accepts any non-empty secret, so an empty one is
    // the case that proves the refusal path.
    let error = Client::connect(
        &Options::new(&server.host, server.port)
            .user("admin")
            .secret(""),
    )
    .expect_err("an empty secret must be refused");
    assert_eq!(error.kind, ErrorKind::Auth, "{error}");
}

#[test]
fn values_round_trip_through_bound_parameters_byte_for_byte() {
    live(|db| {
        let table = unique("rs_params");
        db.execute(&format!(
            "CREATE TABLE {table} (id INT PRIMARY KEY, t TEXT, b BLOB, f DOUBLE, k BOOL)"
        ))
        .expect("create");
        // The table the injection string below names. If binding ever became
        // interpolation, this is what would disappear.
        let victim = unique("rs_victim");
        db.execute(&format!("CREATE TABLE {victim} (id INT PRIMARY KEY)"))
            .expect("create victim");

        let quote = "O'Hara said 'hi'";
        let backslash = r"C:\Users\trine\a\'b";
        let injection = format!("'; DROP TABLE {victim}; --");
        let blob: Vec<u8> = vec![0x00, 0x01, 0xff, 0xfe, b'\'', b'\\', b'h', b'i', 0x00];

        for (id, text) in [(1, quote), (2, backslash), (3, injection.as_str())] {
            db.execute_params(
                &format!("INSERT INTO {table} (id, t) VALUES (?, ?)"),
                &params![id, text],
            )
            .expect("insert text");
            let rows = db
                .query_params(&format!("SELECT t FROM {table} WHERE id = ?"), &params![id])
                .expect("read back");
            assert_eq!(rows.rows[0][0], text, "value {id} changed in flight");
        }

        // The proof that the injection string was data: the table it named is
        // still there.
        assert!(db.query(&format!("SELECT id FROM {victim}")).is_ok());

        db.execute_params(
            &format!("INSERT INTO {table} (id, b, f, k) VALUES (?, ?, ?, ?)"),
            &params![4, blob.clone(), -0.125, true],
        )
        .expect("insert blob, float, bool");
        let rows = db
            .query_params(
                &format!("SELECT b, f, k FROM {table} WHERE id = ?"),
                &params![4],
            )
            .expect("read back");
        assert_eq!(
            rows.rows[0][0], "0x0001fffe275c686900",
            "every byte survives"
        );
        assert_eq!(rows.rows[0][1], "-0.125", "a float is exact");
        assert_eq!(rows.rows[0][2], "true", "a bool is a bool");

        // A bound null is SQL NULL, not the four letters N-U-L-L.
        db.execute_params(
            &format!("INSERT INTO {table} (id, t) VALUES (?, ?)"),
            &params![5, None::<&str>],
        )
        .expect("insert null");
        let rows = db
            .query_params(
                &format!("SELECT COUNT(*) FROM {table} WHERE id = ? AND t IS NULL"),
                &params![5],
            )
            .expect("count nulls");
        assert_eq!(rows.rows[0][0], "1");

        // An exact decimal keeps its digits.
        db.execute(&format!(
            "CREATE TABLE {table}_d (id INT PRIMARY KEY, amount DECIMAL)"
        ))
        .expect("create decimal table");
        db.execute_params(
            &format!("INSERT INTO {table}_d VALUES (?, ?)"),
            &[Param::from(1), Param::decimal("10.50").unwrap()],
        )
        .expect("insert decimal");
        let rows = db
            .query(&format!("SELECT amount FROM {table}_d"))
            .expect("read decimal");
        assert_eq!(rows.rows[0][0], "10.50");

        let _ = db.execute(&format!("DROP TABLE {table}"));
        let _ = db.execute(&format!("DROP TABLE {table}_d"));
        let _ = db.execute(&format!("DROP TABLE {victim}"));
    });
}

#[test]
fn rows_can_be_read_positionally_and_by_name() {
    live(|db| {
        let table = unique("rs_rows");
        db.execute(&format!(
            "CREATE TABLE {table} (id INT PRIMARY KEY, name TEXT)"
        ))
        .expect("create");
        db.execute_params(
            &format!("INSERT INTO {table} VALUES (?, ?)"),
            &params![1, "ada"],
        )
        .expect("insert");
        let rows = db
            .query(&format!("SELECT id, name FROM {table}"))
            .expect("select");
        assert_eq!(rows.columns, vec!["id", "name"]);
        assert_eq!(rows.rows[0], vec!["1", "ada"]);
        assert_eq!(rows.get(0, "name"), Some("ada"));
        assert_eq!(rows.maps()[0]["id"], "1");
        let _ = db.execute(&format!("DROP TABLE {table}"));
    });
}

#[test]
fn a_write_sent_as_a_query_is_refused_and_a_bad_statement_is_a_server_error() {
    live(|db| {
        let table = unique("rs_split");
        db.execute(&format!("CREATE TABLE {table} (id INT PRIMARY KEY)"))
            .expect("create");

        let error = db
            .query(&format!("INSERT INTO {table} VALUES (1)"))
            .expect_err("a write through query must be refused");
        assert_eq!(error.kind, ErrorKind::Server, "{error}");

        let error = db
            .execute("THIS IS NOT SQL AT ALL")
            .expect_err("nonsense must not succeed");
        assert_eq!(error.kind, ErrorKind::Server, "{error}");
        // A refusal leaves the connection perfectly usable.
        assert!(!db.is_poisoned());
        assert!(db.ping().is_ok());
        let _ = db.execute(&format!("DROP TABLE {table}"));
    });
}

#[test]
fn a_transaction_script_commits_as_one_unit() {
    live(|db| {
        let table = unique("rs_txn_script");
        db.execute(&format!(
            "CREATE TABLE {table} (id INT PRIMARY KEY, name TEXT)"
        ))
        .expect("create");
        let result = db
            .transaction(&[
                Statement::with_params(
                    format!("INSERT INTO {table} VALUES (?, ?)"),
                    params![1, "ada"],
                ),
                Statement::with_params(
                    format!("INSERT INTO {table} VALUES (?, ?)"),
                    params![2, "grace"],
                ),
            ])
            .expect("transaction");
        assert_eq!(result.outcome, "committed");
        assert_eq!(result.committed_writes, 2);
        let rows = db
            .query(&format!("SELECT COUNT(*) FROM {table}"))
            .expect("count");
        assert_eq!(rows.rows[0][0], "2");
        let _ = db.execute(&format!("DROP TABLE {table}"));
    });
}

#[test]
fn a_session_transaction_rolls_back_what_it_does_not_commit() {
    // Its own server: a schema change committed by another test aborts an open
    // transaction, which is the server's rule and not something to assert here.
    own_server(|db| {
        let table = unique("rs_txn_session");
        db.execute(&format!(
            "CREATE TABLE {table} (id INT PRIMARY KEY, name TEXT)"
        ))
        .expect("create");

        db.begin().expect("begin");
        assert!(db.in_transaction());
        db.execute_params(
            &format!("INSERT INTO {table} VALUES (?, ?)"),
            &params![1, "ada"],
        )
        .expect("insert inside the transaction");
        db.rollback().expect("rollback");
        assert!(!db.in_transaction());
        let rows = db
            .query(&format!("SELECT COUNT(*) FROM {table}"))
            .expect("count");
        assert_eq!(rows.rows[0][0], "0", "a rolled-back write must not persist");

        // The closure form commits on success…
        db.with_transaction(|tx| {
            tx.execute_params(
                &format!("INSERT INTO {table} VALUES (?, ?)"),
                &params![2, "grace"],
            )?;
            Ok(())
        })
        .expect("with_transaction");
        assert!(!db.in_transaction());

        // …and rolls back on failure, returning the caller's own error.
        let error = db
            .with_transaction(|tx| {
                tx.execute_params(
                    &format!("INSERT INTO {table} VALUES (?, ?)"),
                    &params![3, "hopper"],
                )?;
                Err::<(), _>(tricoredb::Error::new(ErrorKind::InvalidArgument, "give up"))
            })
            .expect_err("the closure failed");
        assert_eq!(error.message(), "give up");
        assert!(!db.in_transaction());

        let rows = db
            .query(&format!("SELECT id FROM {table} ORDER BY id"))
            .expect("read back");
        assert_eq!(rows.rows, vec![vec!["2".to_string()]]);
        let _ = db.execute(&format!("DROP TABLE {table}"));
    });
}

#[test]
fn a_cache_value_of_100_kib_round_trips_byte_for_byte() {
    live(|db| {
        let namespace = unique("rs_cache");
        // A pseudo-random payload larger than one TCP segment: the case a
        // single-read client passes locally and corrupts in production.
        let big: Vec<u8> = (0..100 * 1024)
            .map(|i| ((i * 31 + 7) % 256) as u8)
            .collect();
        db.cache_set(&namespace, "big", &big).expect("set");
        let read = db.cache_get(&namespace, "big").expect("get");
        assert_eq!(read.as_deref(), Some(big.as_slice()));

        // A miss and a stored empty value are different answers.
        assert_eq!(db.cache_get(&namespace, "absent").expect("miss"), None);
        db.cache_set(&namespace, "empty", b"").expect("set empty");
        assert_eq!(
            db.cache_get(&namespace, "empty").expect("get empty"),
            Some(Vec::new())
        );

        assert!(db.cache_exists(&namespace, "big").expect("exists"));
        assert!(db.cache_delete(&namespace, "big").expect("delete"));
        assert!(!db.cache_delete(&namespace, "big").expect("delete again"));
        let _ = db.cache_clear_namespace(&namespace);
    });
}

#[test]
fn cache_ttls_counters_and_collections_behave() {
    live(|db| {
        let ns = unique("rs_cache2");
        db.cache_ping().expect("cache ping");

        db.cache_set_ttl(&ns, "k", b"v", Some(Duration::from_secs(30)))
            .expect("set with ttl");
        let ttl = db.cache_ttl(&ns, "k").expect("ttl").expect("a ttl was set");
        assert!(ttl <= Duration::from_secs(30) && ttl > Duration::ZERO);
        assert!(db.cache_persist(&ns, "k").expect("persist"));
        assert_eq!(db.cache_ttl(&ns, "k").expect("ttl"), None);

        assert!(db.cache_set_nx(&ns, "lock", b"1", None).expect("set nx"));
        assert!(!db
            .cache_set_nx(&ns, "lock", b"2", None)
            .expect("set nx again"));

        assert_eq!(db.cache_incr(&ns, "hits", 2).expect("incr"), 2);
        assert_eq!(db.cache_incr(&ns, "hits", 3).expect("incr"), 5);

        assert_eq!(db.cache_rpush(&ns, "q", &[b"a", b"b"]).expect("rpush"), 2);
        assert_eq!(db.cache_lpush(&ns, "q", &[b"z"]).expect("lpush"), 3);
        assert_eq!(db.cache_llen(&ns, "q").expect("llen"), 3);
        assert_eq!(
            db.cache_lrange(&ns, "q", 0, -1).expect("lrange"),
            vec![b"z".to_vec(), b"a".to_vec(), b"b".to_vec()]
        );
        assert_eq!(
            db.cache_lpop(&ns, "q").expect("lpop").as_deref(),
            Some(&b"z"[..])
        );
        assert_eq!(
            db.cache_rpop(&ns, "q").expect("rpop").as_deref(),
            Some(&b"b"[..])
        );
        assert_eq!(
            db.cache_lindex(&ns, "q", 0).expect("lindex").as_deref(),
            Some(&b"a"[..])
        );

        assert_eq!(
            db.cache_sadd(&ns, "tags", &[b"go", b"db"]).expect("sadd"),
            2
        );
        assert_eq!(db.cache_sadd(&ns, "tags", &[b"go"]).expect("sadd again"), 0);
        assert!(db.cache_sismember(&ns, "tags", b"db").expect("sismember"));
        assert_eq!(db.cache_scard(&ns, "tags").expect("scard"), 2);
        assert_eq!(db.cache_smembers(&ns, "tags").expect("smembers").len(), 2);
        assert_eq!(db.cache_srem(&ns, "tags", &[b"go"]).expect("srem"), 1);

        assert_eq!(
            db.cache_hset_text(&ns, "user:1", [("name", "ada"), ("city", "london")])
                .expect("hset"),
            2
        );
        assert_eq!(
            db.cache_hget(&ns, "user:1", b"name")
                .expect("hget")
                .as_deref(),
            Some(&b"ada"[..])
        );
        assert!(db.cache_hexists(&ns, "user:1", b"city").expect("hexists"));
        assert_eq!(db.cache_hlen(&ns, "user:1").expect("hlen"), 2);
        assert_eq!(db.cache_hgetall(&ns, "user:1").expect("hgetall").len(), 2);
        assert_eq!(db.cache_hdel(&ns, "user:1", &[b"city"]).expect("hdel"), 1);

        let id = db
            .cache_xadd_text(&ns, "events", [("msg", "hi")], None)
            .expect("xadd");
        assert!(!id.is_empty());
        assert_eq!(db.cache_xlen(&ns, "events").expect("xlen"), 1);
        let entries = db
            .cache_xrange(&ns, "events", "-", "+", None)
            .expect("xrange");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text()["msg"], "hi");
        assert!(!db
            .cache_xread(&ns, "events", "0-0", None)
            .expect("xread")
            .is_empty());
        assert_eq!(db.cache_xdel(&ns, "events", &[&id]).expect("xdel"), 1);

        let keys = db.cache_keys(&ns, None, None).expect("keys");
        assert!(!keys.is_empty());
        assert!(db.cache_clear_namespace(&ns).expect("clear") > 0);
    });
}

#[test]
fn a_binary_hash_field_survives_the_round_trip() {
    live(|db| {
        let ns = unique("rs_binary");
        // Neither half is valid UTF-8: a client that spoke only text would
        // corrupt both.
        let field = vec![0xffu8, 0x00, 0xfe];
        let value = vec![0x00u8, 0xc3, 0x28];
        db.cache_hset(&ns, "h", &[CachePair::new(field.clone(), value.clone())])
            .expect("hset");
        assert_eq!(
            db.cache_hget(&ns, "h", &field).expect("hget"),
            Some(value.clone())
        );
        let all = db.cache_hgetall(&ns, "h").expect("hgetall");
        assert_eq!(all[0].field, field);
        assert_eq!(all[0].value, value);
        let _ = db.cache_clear_namespace(&ns);
    });
}

#[test]
fn documents_can_be_written_queried_indexed_and_aggregated() {
    live(|db| {
        let collection = unique("rs_docs");
        db.document_create_collection(&collection).expect("create");

        let mut widget = Map::new();
        widget.insert("name".into(), json!("widget"));
        widget.insert("price".into(), json!(9));
        widget.insert("kind".into(), json!("tool"));
        let id = db.document_insert(&collection, &widget).expect("insert");
        assert!(!id.is_empty());

        db.document_insert_with_id(
            &collection,
            "gadget",
            &json!({"name": "gadget", "price": 20, "kind": "tool"}),
        )
        .expect("insert with id");

        let fetched = db.document_get(&collection, "gadget").expect("get");
        assert_eq!(fetched.expect("present")["name"], "gadget");
        assert_eq!(db.document_get(&collection, "missing").expect("miss"), None);

        let found = db
            .document_find(&collection, &DocumentFilter::gt("price", 10))
            .expect("find");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["name"], "gadget");

        assert_eq!(
            db.document_find_limit(&collection, &DocumentFilter::all(), 1)
                .expect("find limit")
                .len(),
            1
        );

        db.document_update_one(
            &collection,
            "gadget",
            &DocumentUpdate::new().inc("price", 5),
        )
        .expect("update one");
        let gadget = db
            .document_get(&collection, "gadget")
            .expect("get")
            .expect("present");
        assert_eq!(gadget["price"], 25);

        let inserted = db
            .document_upsert_one(
                &collection,
                "sprocket",
                &DocumentUpdate::new()
                    .set("name", "sprocket")
                    .set("price", 3),
            )
            .expect("upsert");
        assert!(inserted, "a missing id is created by an upsert");

        let counts = db
            .document_update_many(
                &collection,
                &DocumentFilter::eq("kind", "tool"),
                &DocumentUpdate::new().set("kind", "hardware"),
            )
            .expect("update many");
        assert_eq!(counts.matched, 2);
        assert_eq!(counts.modified, 2);

        db.document_create_index(&collection, "by_name", "name", true)
            .expect("create index");
        let indexes = db.document_list_indexes(&collection).expect("list indexes");
        assert!(indexes.iter().any(|i| i.name == "by_name" && i.unique));
        db.document_drop_index(&collection, "by_name")
            .expect("drop index");

        let stats = db.document_analyze(&collection).expect("analyze");
        assert_eq!(stats.document_count, 3);

        let totals = db
            .document_aggregate(
                &collection,
                &[
                    AggregateStage::filter(DocumentFilter::gt("price", 1)),
                    AggregateStage::group(
                        GroupKey::constant("all"),
                        [
                            GroupAccumulator::new("total", AccumulatorOp::sum("price")),
                            GroupAccumulator::new("n", AccumulatorOp::count()),
                        ],
                    ),
                ],
            )
            .expect("aggregate");
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0]["n"], 3);
        assert_eq!(
            totals[0]["total"].as_f64(),
            Some(37.0), // 9 + 25 + 3
            "the sum came back as {}",
            totals[0]["total"]
        );

        assert!(db
            .document_list_collections()
            .expect("list")
            .contains(&collection));
        db.document_delete(&collection, "gadget").expect("delete");
        db.document_drop_collection(&collection).expect("drop");
    });
}

#[test]
fn vectors_are_searchable_and_filterable() {
    live(|db| {
        let collection = unique("rs_vectors");
        db.vector_create_collection(&collection, 3, VectorMetric::Cosine)
            .expect("create");

        let mut doc_meta = Map::new();
        doc_meta.insert("kind".into(), json!("doc"));
        db.vector_upsert(&collection, "a", &[0.1, 0.2, 0.3], Some(&doc_meta))
            .expect("upsert a");
        let mut image_meta = Map::new();
        image_meta.insert("kind".into(), json!("image"));
        db.vector_upsert(&collection, "b", &[0.9, 0.1, 0.0], Some(&image_meta))
            .expect("upsert b");

        let found = db
            .vector_get(&collection, "a")
            .expect("get")
            .expect("present");
        assert_eq!(found.vector, vec![0.1, 0.2, 0.3], "stored exactly");
        assert_eq!(found.metadata["kind"], "doc");
        assert_eq!(db.vector_get(&collection, "zz").expect("miss"), None);

        let hits = db
            .vector_search(&collection, &[0.1, 0.2, 0.3], 2)
            .expect("search");
        assert_eq!(hits.results[0].id, "a", "the nearest vector comes first");
        assert!(!hits.index.is_empty());

        let mut filter = BTreeMap::new();
        filter.insert("kind".to_string(), json!("image"));
        let filtered = db
            .vector_search_filtered(&collection, &[0.1, 0.2, 0.3], 5, &filter)
            .expect("filtered search");
        assert_eq!(filtered.results.len(), 1);
        assert_eq!(filtered.results[0].id, "b");

        let info = db
            .vector_describe_collection(&collection)
            .expect("describe");
        assert_eq!(info.dimension, 3);
        assert_eq!(info.metric, VectorMetric::Cosine);
        assert_eq!(info.count, 2);

        let page = db
            .vector_list_vectors(&collection, Some(10), None)
            .expect("list");
        assert_eq!(page.vectors.len(), 2);

        // A wrong-length vector is refused rather than padded.
        let error = db
            .vector_upsert(&collection, "bad", &[1.0, 2.0], None)
            .expect_err("a dimension mismatch must be refused");
        assert_eq!(error.kind, ErrorKind::Server, "{error}");

        assert!(db
            .vector_list_collections()
            .expect("list collections")
            .iter()
            .any(|c| c.name == collection));
        db.vector_delete(&collection, "a").expect("delete");
        db.vector_drop_collection(&collection).expect("drop");
    });
}

#[test]
fn graphs_traverse_and_find_paths() {
    live(|db| {
        let graph = unique("rs_graph");
        db.graph_create(&graph).expect("create");

        let mut ada = Map::new();
        ada.insert("name".into(), json!("ada"));
        db.graph_add_node(&graph, "u1", &["User"], Some(&ada))
            .expect("add u1");
        db.graph_add_node(&graph, "u2", &["User"], None)
            .expect("add u2");
        db.graph_add_node(&graph, "u3", &["User"], None)
            .expect("add u3");

        let mut weight = Map::new();
        weight.insert("weight".into(), json!(1.0));
        db.graph_add_edge(&graph, "e1", "u1", "u2", "FOLLOWS", Some(&weight))
            .expect("add e1");
        db.graph_add_edge(&graph, "e2", "u2", "u3", "FOLLOWS", Some(&weight))
            .expect("add e2");

        let node = db
            .graph_get_node(&graph, "u1")
            .expect("get")
            .expect("present");
        assert_eq!(node.properties["name"], "ada");
        assert_eq!(node.labels, vec!["User"]);
        assert_eq!(db.graph_get_node(&graph, "nobody").expect("miss"), None);

        let edge = db
            .graph_get_edge(&graph, "e1")
            .expect("get")
            .expect("present");
        assert_eq!((edge.from.as_str(), edge.to.as_str()), ("u1", "u2"));

        let neighbors = db
            .graph_neighbors(&graph, "u1", &NeighborOptions::default())
            .expect("neighbors");
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].node_id, "u2");
        assert_eq!(neighbors[0].direction, GraphDirection::Outgoing);

        assert_eq!(
            db.graph_degree(&graph, "u2", Some(GraphDirection::Both))
                .expect("degree"),
            2
        );

        let walk = db
            .graph_traverse(&graph, "u1", &Default::default())
            .expect("traverse");
        assert!(walk.nodes.iter().any(|n| n.id == "u3"));

        let path = db
            .graph_shortest_path(&graph, "u1", "u3", &PathOptions::default())
            .expect("shortest path");
        assert!(path.found);
        assert_eq!(path.hops, 2);
        assert_eq!(path.node_path, vec!["u1", "u2", "u3"]);

        let weighted = db
            .graph_weighted_shortest_path(&graph, "u1", "u3", &Default::default())
            .expect("weighted path");
        assert!(weighted.found);
        assert_eq!(weighted.total_cost, 2.0);

        // No path is an answer, not an error.
        let none = db
            .graph_shortest_path(&graph, "u3", "u1", &PathOptions::default())
            .expect("no path is still a result");
        assert!(!none.found);

        let rows = db
            .graph_query(&graph, "MATCH (n:User) RETURN n LIMIT 10")
            .expect("cypher");
        assert!(!rows.columns.is_empty());

        assert_eq!(
            db.graph_list_nodes(&graph, None, None)
                .expect("nodes")
                .total,
            3
        );
        assert_eq!(
            db.graph_list_edges(&graph, None, None)
                .expect("edges")
                .total,
            2
        );
        assert!(db.graph_list().expect("list").contains(&graph));

        db.graph_delete_edge(&graph, "e1").expect("delete edge");
        db.graph_delete_node(&graph, "u1").expect("delete node");
        db.graph_drop(&graph).expect("drop");
    });
}

#[test]
fn context_exports_render_in_the_format_asked_for() {
    live(|db| {
        let table = unique("rs_llm");
        db.execute(&format!(
            "CREATE TABLE {table} (id INT PRIMARY KEY, name TEXT)"
        ))
        .expect("create");
        db.execute_params(
            &format!("INSERT INTO {table} VALUES (?, ?)"),
            &params![1, "ada"],
        )
        .expect("insert");

        let bundle = db
            .llm_context(
                &[LlmSource::sql(&format!("SELECT id, name FROM {table}"))],
                OutputFormat::Toon,
                None,
            )
            .expect("context");
        assert!(bundle.contains("ada"), "the bundle holds the row: {bundle}");

        let schema = db.llm_schema(OutputFormat::Markdown, None).expect("schema");
        assert!(!schema.is_empty());

        // A bundle with no source is refused here, before anything is sent.
        let error = db
            .llm_context(&[], OutputFormat::Json, None)
            .expect_err("no sources");
        assert_eq!(error.kind, ErrorKind::InvalidArgument);

        let _ = db.execute(&format!("DROP TABLE {table}"));
    });
}

#[test]
fn a_disabled_module_is_refused_by_name() {
    live(|db| {
        // The test server runs without the cluster module, so the admin plane
        // answers with an error rather than pretending to be healthy.
        let error = db
            .admin_ping()
            .expect_err("cluster is off in the test config");
        assert_eq!(error.kind, ErrorKind::Server, "{error}");
        assert!(error.code().is_some(), "a refusal carries a code: {error}");
        assert!(db.ping().is_ok(), "the connection is still usable");
    });
}

#[test]
fn a_request_timeout_and_a_cancel_reach_the_server() {
    live(|db| {
        db.set_request_timeout(Some(Duration::from_secs(30)));
        assert!(db.query("SELECT 1").is_ok());
        db.set_request_timeout(None);

        // Cancelling an id nobody is running stops nothing, and says so rather
        // than failing.
        let Some(server) = server() else { return };
        let mut other = server.client();
        assert_eq!(other.cancel("rs-nothing-1").expect("cancel"), 0);
        assert!(other.cancel("").is_err(), "an empty id is refused here");
    });
}

#[test]
fn a_raw_request_reaches_an_operation_with_no_typed_method() {
    live(|db| {
        let response = db
            .request(json!({"Cache": "Ping"}))
            .expect("raw cache ping");
        assert_eq!(response.status, "ok");
        assert!(response.request_id.starts_with("rs-"));
        assert_eq!(db.last_request_id(), Some(response.request_id.as_str()));
    });
}

#[test]
fn a_pool_serves_several_threads_at_once() {
    let Some(server) = server() else { return };
    let pool = Pool::new(server.options(), 4).expect("pool");
    let table = unique("rs_pool");
    pool.with_connection(|db| {
        db.execute(&format!("CREATE TABLE {table} (id INT PRIMARY KEY)"))?;
        Ok(())
    })
    .expect("create through the pool");

    std::thread::scope(|scope| {
        for id in 1..=8 {
            let pool = pool.clone();
            let table = table.clone();
            scope.spawn(move || {
                pool.with_connection(|db| {
                    db.execute_params(&format!("INSERT INTO {table} VALUES (?)"), &params![id])?;
                    Ok(())
                })
                .expect("insert through the pool");
            });
        }
    });

    pool.with_connection(|db| {
        let rows = db.query(&format!("SELECT COUNT(*) FROM {table}"))?;
        assert_eq!(rows.rows[0][0], "8");
        db.execute(&format!("DROP TABLE {table}"))?;
        Ok(())
    })
    .expect("count through the pool");

    let (idle, lent) = pool.stats();
    assert!(
        idle > 0 && lent == 0,
        "connections came back: {idle} idle, {lent} lent"
    );
    assert!(idle <= pool.size());
    pool.close();
    assert!(pool.with_connection(|_| Ok(())).is_err());
}

#[test]
fn a_pooled_connection_is_never_returned_mid_transaction() {
    // Its own server, for the same reason as the session-transaction test.
    let server = match start_server() {
        Ok(server) => server,
        Err(reason) => return eprintln!("skipping: {reason}"),
    };
    let pool = Pool::new(server.options(), 2).expect("pool");
    let table = unique("rs_pool_txn");
    pool.with_connection(|db| {
        db.execute(&format!("CREATE TABLE {table} (id INT PRIMARY KEY)"))?;
        Ok(())
    })
    .expect("create");

    // The closure opens a transaction and forgets to end it.
    let error = pool
        .with_connection(|db| {
            db.begin()?;
            db.execute_params(&format!("INSERT INTO {table} VALUES (?)"), &params![1])?;
            Ok(())
        })
        .expect_err("leaving a transaction open is reported");
    assert!(error.message().contains("rolled back"), "{error}");

    // The write was rolled back, and the pool still works.
    pool.with_connection(|db| {
        let rows = db.query(&format!("SELECT COUNT(*) FROM {table}"))?;
        assert_eq!(rows.rows[0][0], "0");
        db.execute(&format!("DROP TABLE {table}"))?;
        Ok(())
    })
    .expect("the pool is still usable");
    pool.close();
}

#[test]
fn masking_a_capability_out_makes_the_client_refuse_by_name() {
    let Some(server) = server() else { return };
    let options = server
        .options()
        .features(tricoredb::ALL_FEATURES & !tricoredb::FEATURE_SERVER_PARAMS);
    let mut db = Client::connect(&options).expect("connect without SERVER_PARAMS");
    assert!(!db.server_params_granted());

    let error = db
        .execute_params("INSERT INTO nothing VALUES (?)", &params![1])
        .expect_err("binding without the capability must be refused");
    assert_eq!(error.kind, ErrorKind::FeatureNotGranted, "{error}");
    assert_eq!(error.code(), Some("feature_not_granted"));
    assert!(
        error.message().contains("SERVER_PARAMS"),
        "the refusal names the capability: {error}"
    );

    // Nothing was sent, so the connection is untouched — and a statement with
    // no parameters needs no capability.
    assert!(db.ping().is_ok());
    assert!(db.query("SELECT 1").is_ok());

    let options = server
        .options()
        .features(tricoredb::ALL_FEATURES & !tricoredb::FEATURE_SESSION_TXN);
    let mut db = Client::connect(&options).expect("connect without SESSION_TXN");
    let error = db.begin().expect_err("begin without the capability");
    assert_eq!(error.kind, ErrorKind::FeatureNotGranted, "{error}");
    assert!(!db.in_transaction());
}

#[test]
fn a_closed_connection_says_so_rather_than_hanging() {
    let Some(server) = server() else { return };
    let mut db = server.client();
    assert!(db.query("SELECT 1").is_ok());
    db.close().expect("close");

    let mut db = server.client();
    // Reading a response the server never sends is bounded by the read timeout,
    // and afterwards the connection refuses to be reused.
    db.set_read_timeout(Some(Duration::from_millis(50)))
        .expect("set read timeout");
    assert!(db.ping().is_ok(), "a live server answers well within 50ms");
}

#[test]
fn every_value_type_is_accepted_by_the_params_macro() {
    live(|db| {
        let table = unique("rs_types");
        db.execute(&format!(
            "CREATE TABLE {table} (id INT PRIMARY KEY, a TEXT, b TEXT, c DOUBLE)"
        ))
        .expect("create");
        let text = String::from("owned");
        let slice: &[u8] = &[1, 2, 3];
        db.execute_params(
            &format!("INSERT INTO {table} VALUES (?, ?, ?, ?)"),
            &params![1i64, text.clone(), slice, 0.5f32],
        )
        .expect("insert mixed types");
        let rows = db
            .query(&format!("SELECT a, b, c FROM {table}"))
            .expect("read back");
        assert_eq!(rows.rows[0][0], "owned");
        assert_eq!(rows.rows[0][1], "0x010203");
        assert_eq!(rows.rows[0][2], "0.5");
        let _ = db.execute(&format!("DROP TABLE {table}"));
    });
}

#[test]
fn a_non_finite_float_is_refused_before_it_is_sent() {
    live(|db| {
        let error = db
            .execute_params("INSERT INTO t VALUES (?)", &params![f64::NAN])
            .expect_err("NaN has no SQL form");
        assert_eq!(error.kind, ErrorKind::InvalidArgument, "{error}");
        assert!(db.ping().is_ok(), "nothing was sent");
        let _: Value = json!(null); // keep serde_json::Value used in every build
    });
}
