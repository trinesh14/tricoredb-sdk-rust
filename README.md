# tricoredb

Official Rust client for [TriCoreDB](https://hub.docker.com/r/trinesh14/tricoredb):
SQL, documents, vectors, graphs and cache over one native connection.

[![crates.io](https://img.shields.io/crates/v/tricoredb?cacheSeconds=3600)](https://crates.io/crates/tricoredb)
[![docs.rs](https://img.shields.io/docsrs/tricoredb?cacheSeconds=3600)](https://docs.rs/tricoredb)
[![license](https://img.shields.io/crates/l/tricoredb?cacheSeconds=86400)](LICENSE)

- **Blocking, no async runtime.** Calls block; a `Pool` serves concurrent work.
- **Server-side parameters.** Values never become part of the SQL text.
- **Typed errors** you branch on by kind and code, never by message.
- **Transactions, connection pooling, TLS and mutual TLS.**
- **Four dependencies**, two of them optional: `serde`, `serde_json`, and
  `rustls` only when TLS is on.

## Contents

- [Requirements](#requirements)
- [Installation](#installation)
- [Running a server](#running-a-server)
- [Quick start](#quick-start)
- [Connecting](#connecting)
- [SQL](#sql)
- [Transactions](#transactions)
- [Connection pool](#connection-pool)
- [Cache](#cache)
- [Documents](#documents)
- [Vectors](#vectors)
- [Graphs](#graphs)
- [LLM context](#llm-context)
- [Admin](#admin)
- [Errors](#errors)
- [TLS](#tls)
- [Testing](#testing)

## Requirements

- Rust **1.85** or later, which is what `rustls` and its dependencies need.
- A TriCoreDB server speaking protocol 1.0 (`tricore-server` 0.1.0-rc.1 or
  later). See [Running a server](#running-a-server).

## Installation

```bash
cargo add tricoredb
```

Or in `Cargo.toml`:

```toml
[dependencies]
tricoredb = "0.1"
```

TLS is on by default. For a build with no cryptography dependencies at all:

```toml
[dependencies]
tricoredb = { version = "0.1", default-features = false }
```

## Running a server

The quickest way is the official Docker image,
[`trinesh14/tricoredb`](https://hub.docker.com/r/trinesh14/tricoredb).

**Local development** (no TLS and no encryption, for this machine only). Set
`TRICORE_ADMIN_PASSWORD` in your shell first. Then create the admin and start
the server:

```bash
docker run --rm -v tricoredb-dev:/var/lib/tricoredb -e TRICORE_ADMIN_PASSWORD --entrypoint /usr/local/bin/tricore trinesh14/tricoredb:0.1.0-rc.1-r2 auth init-admin --user admin --password-env TRICORE_ADMIN_PASSWORD --data-dir /var/lib/tricoredb/data
docker run -d --name tricoredb-dev -p 127.0.0.1:8427:8427 -e TRICORE_TLS=off -e TRICORE_ENCRYPTION=off -e TRICORE_MODULES=all -v tricoredb-dev:/var/lib/tricoredb trinesh14/tricoredb:0.1.0-rc.1-r2
```

**Anything else:** by default the image runs with **TLS on** and an
**encrypted data volume**. Follow the quick start on the
[Docker Hub page](https://hub.docker.com/r/trinesh14/tricoredb) to create the
certificate and key, then connect with [TLS](#tls).

`TRICORE_MODULES=all` enables every data model. The image's default is `sql`,
`document` and `cache`. A call to a disabled model fails with an error whose
`code()` is `engine.disabled`.

## Quick start

```rust,no_run
use tricoredb::{params, Client, Options};

fn main() -> tricoredb::Result<()> {
    let mut db = Client::connect(&Options::new("127.0.0.1", 8427).user("admin").secret("your-password"))?;

    db.execute("CREATE TABLE IF NOT EXISTS users (id INT PRIMARY KEY, name TEXT)")?;
    db.execute_params("INSERT INTO users VALUES (?, ?)", &params![1, "O'Hara"])?;

    let rows = db.query_params("SELECT name FROM users WHERE id = ?", &params![1])?;
    println!("{:?}", rows.get(0, "name")); // Some("O'Hara")

    db.cache_set("sessions", "u1", b"token")?;
    if let Some(token) = db.cache_get("sessions", "u1")? {
        println!("{} bytes", token.len());
    }
    Ok(())
}
```

## Connecting

`Client::connect` opens one authenticated connection. Dropping it ends the
session; `close()` says goodbye first and reports whether the socket closed
cleanly.

| `Options` field | Default | Meaning |
| --- | --- | --- |
| `host` | `"127.0.0.1"` | Server host |
| `port` | `8427` (`tricoredb::DEFAULT_PORT`) | Server port |
| `user` | `None` | Principal to authenticate as |
| `secret` | `None` | Password or token |
| `database` | `"main"` | Database named in every request |
| `client_name` | `"tricoredb-rust/<version>"` | Name reported in the handshake |
| `connect_timeout` | `None` | Bound on the connect and TLS handshake |
| `read_timeout` | `None` | Bound on each wait for a reply |
| `tls` | `None` (plain TCP) | See [TLS](#tls) |
| `features` | all | Capability bitmap announced in the handshake |

Each field has a builder method, so options read as one expression:

```rust
use std::time::Duration;
use tricoredb::Options;

let options = Options::new("db.internal", 8427)
    .user("admin")
    .secret("your-password")
    .database("reporting")
    .connect_timeout(Duration::from_secs(5));
```

**One connection runs one request at a time.** Every method takes `&mut self`,
so sharing a `Client` between threads does not compile — the frame interleaving
that would corrupt a connection is ruled out rather than documented. Use a
[pool](#connection-pool) for concurrency.

## SQL

`query` runs only `SELECT`. `execute` runs everything else. The server enforces
the split: a write sent through `query` is refused.

```rust,no_run
# fn main() -> tricoredb::Result<()> {
# let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
use tricoredb::{params, Param};

db.execute("INSERT INTO users VALUES (2, 'ada')")?;
db.execute_params(
    "INSERT INTO accounts VALUES (?, ?)",
    &params![1, Param::decimal("10.50").unwrap()],
)?;

let rows = db.query("SELECT id, name FROM users")?;
for row in &rows.rows {
    println!("{row:?}");
}
println!("{:?}", rows.get(0, "name"));
# Ok(()) }
```

`execute_params` and `query_params` bind `?` placeholders **on the server**. The
values travel next to the statement, so a value can never be read as SQL syntax,
however it is spelled. The `params!` macro builds the list from mixed types:

| Rust value | Sent as |
| --- | --- |
| `None` | SQL `NULL` |
| `bool` | a boolean |
| any integer, up to `i128` / `u128` | an exact number, never through `f64` |
| `f32` / `f64` | a number; `NaN` and infinities are refused |
| `Param::decimal("10.50")` | exact digits, no exponent — for `DECIMAL` |
| `Vec<u8>` / `&[u8]` | `0x`-prefixed hex, for `BLOB` |
| `&str` / `String` | text |

Binding needs the `SERVER_PARAMS` capability, agreed in the handshake
(`db.server_params_granted()`). If the server did not grant it, these calls fail
with `ErrorKind::FeatureNotGranted` **before anything is sent**. They never fall
back to pasting values into the statement text: escaping and binding are not the
same guarantee.

## Transactions

Two kinds, and they are not interchangeable.

`transaction` sends a whole `BEGIN … COMMIT` script in **one request**. It works
on every node, and is right when every statement is known up front:

```rust,no_run
# fn main() -> tricoredb::Result<()> {
# let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
use tricoredb::{params, Statement};

let result = db.transaction(&[
    Statement::with_params("UPDATE accounts SET balance = balance - ? WHERE id = ?", params![10, 1]),
    Statement::with_params("UPDATE accounts SET balance = balance + ? WHERE id = ?", params![10, 2]),
])?;
assert_eq!(result.outcome, "committed");
# Ok(()) }
```

`begin`, `commit` and `rollback` keep a transaction open **across requests on
this connection**, so a later statement can depend on what an earlier one read.
`with_transaction` commits when the closure returns `Ok` and rolls back on
`Err`:

```rust,no_run
# fn main() -> tricoredb::Result<()> {
# let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
use tricoredb::params;

db.with_transaction(|tx| {
    tx.execute_params("UPDATE accounts SET balance = balance - ? WHERE id = ?", &params![10, 1])?;
    tx.execute_params("UPDATE accounts SET balance = balance + ? WHERE id = ?", &params![10, 2])?;
    Ok(())
})?;
# Ok(()) }
```

Session transactions need the `SESSION_TXN` capability
(`db.session_txn_granted()`). Without it, `begin` fails by name rather than
silently running each statement on its own. The transaction belongs to this
connection: another connection cannot commit it, and a dropped socket rolls it
back.

## Connection pool

A `Pool` is built from the same `Options`, so every pooled connection uses the
same TLS settings. It is `Clone` and `Send`, so threads share one pool.

```rust,no_run
# fn main() -> tricoredb::Result<()> {
use tricoredb::{params, Options, Pool};

let pool = Pool::new(Options::new("db.internal", 8427).user("admin").secret("pw"), 8)?;

std::thread::scope(|scope| {
    for id in 1..=8 {
        let pool = pool.clone();
        scope.spawn(move || {
            pool.with_connection(|db| {
                db.execute_params("INSERT INTO users VALUES (?, ?)", &params![id, "grace"])?;
                Ok(())
            })
        });
    }
});

pool.close();
# Ok(()) }
```

`with_connection` lends a connection for the duration of a closure rather than
handing back a guard, because a guard is something a caller can hold across
threads. A connection is never returned to the pool with a transaction still
open: it is rolled back first, and retired if that rollback fails.

## Cache

Values are bytes. `None` is a miss, which is how a miss is told apart from a
stored empty value.

```rust,no_run
# fn main() -> tricoredb::Result<()> {
# let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
use std::time::Duration;

db.cache_set("sessions", "u1", b"token")?;
db.cache_set_ttl("sessions", "u2", b"token", Some(Duration::from_secs(30)))?;
let value = db.cache_get("sessions", "u1")?;

db.cache_incr("counters", "hits", 1)?;
db.cache_rpush("queue", "jobs", &[b"a", b"b"])?;
db.cache_sadd("tags", "post:1", &[b"rust", b"db"])?;
db.cache_hset_text("user:1", "profile", [("name", "ada")])?;
let id = db.cache_xadd_text("events", "log", [("msg", "hi")], None)?;
# let _ = (value, id); Ok(()) }
```

| Family | Methods |
| --- | --- |
| Keys | `cache_get`, `cache_set`, `cache_set_ttl`, `cache_set_nx`, `cache_delete`, `cache_exists`, `cache_ttl`, `cache_expire`, `cache_persist`, `cache_incr`, `cache_keys`, `cache_clear_namespace`, `cache_ping` |
| Lists | `cache_lpush`, `cache_rpush`, `cache_lpop`, `cache_rpop`, `cache_lrange`, `cache_llen`, `cache_lindex` |
| Sets | `cache_sadd`, `cache_srem`, `cache_sismember`, `cache_scard`, `cache_smembers` |
| Hashes | `cache_hset`, `cache_hset_text`, `cache_hget`, `cache_hdel`, `cache_hgetall`, `cache_hexists`, `cache_hlen` |
| Streams | `cache_xadd`, `cache_xadd_text`, `cache_xlen`, `cache_xrange`, `cache_xread`, `cache_xdel`, `cache_xtrim` |

## Documents

Filters and pipeline stages are built with constructors, so you never write the
request JSON by hand.

```rust,no_run
# fn main() -> tricoredb::Result<()> {
# let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
use serde_json::json;
use tricoredb::{DocumentFilter, DocumentUpdate};

db.document_create_collection("products")?;
let id = db.document_insert("products", &json!({"name": "widget", "price": 9}))?;
let found = db.document_find("products", &DocumentFilter::gt("price", 5))?;
db.document_update_one("products", &id, &DocumentUpdate::new().inc("price", 1))?;
# let _ = found; Ok(()) }
```

Aggregation uses the same shape:

```rust,no_run
# fn main() -> tricoredb::Result<()> {
# let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
use tricoredb::{AccumulatorOp, AggregateStage, DocumentFilter, GroupAccumulator, GroupKey};

let totals = db.document_aggregate("orders", &[
    AggregateStage::filter(DocumentFilter::eq("status", "paid")),
    AggregateStage::group(
        GroupKey::field("customer"),
        [GroupAccumulator::new("total", AccumulatorOp::sum("amount"))],
    ),
])?;
# let _ = totals; Ok(()) }
```

Also available: `document_insert_with_id`, `document_get`,
`document_find_limit`, `document_update`, `document_upsert_one`,
`document_update_many`, `document_delete`, `document_list_collections`,
`document_drop_collection`, `document_create_index`, `document_drop_index`,
`document_list_indexes` and `document_analyze`.

## Vectors

Fixed-dimension collections with cosine, dot or L2 distance, metadata filters
and optional `int8` quantization.

```rust,no_run
# fn main() -> tricoredb::Result<()> {
# let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
use tricoredb::VectorMetric;

db.vector_create_collection("embeddings", 3, VectorMetric::Cosine)?;
db.vector_upsert("embeddings", "a", &[0.1, 0.2, 0.3], None)?;
let hits = db.vector_search("embeddings", &[0.1, 0.2, 0.3], 5)?;
for hit in &hits.results {
    println!("{} {}", hit.id, hit.score);
}
# Ok(()) }
```

The score is a **similarity**: higher is closer under every metric, and results
come back best first. L2 is the case worth knowing — the server negates the
squared distance, so an L2 score is `<= 0` and `-0.02` is nearer than `-196.0`.

Also available: `vector_create_collection_quantized`, `vector_search_filtered`,
`vector_get`, `vector_delete`, `vector_list_collections`,
`vector_describe_collection`, `vector_list_vectors` and
`vector_drop_collection`.

## Graphs

Labelled nodes, typed edges, traversal, two shortest-path algorithms and a
read-only Cypher subset.

```rust,no_run
# fn main() -> tricoredb::Result<()> {
# let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
use tricoredb::{NeighborOptions, PathOptions};

db.graph_create("social")?;
db.graph_add_node("social", "u1", &["User"], None)?;
db.graph_add_node("social", "u2", &["User"], None)?;
db.graph_add_edge("social", "e1", "u1", "u2", "FOLLOWS", None)?;

let neighbors = db.graph_neighbors("social", "u1", &NeighborOptions::default())?;
let path = db.graph_shortest_path("social", "u1", "u2", &PathOptions::default())?;
println!("{} hops: {:?}", path.hops, path.node_path);
# let _ = neighbors; Ok(()) }
```

"No path" comes back as `found == false`, not as an error, and `message` says
whether the search ran out of graph or stopped at a bound.

Also available: `graph_get_node`, `graph_get_edge`, `graph_delete_node`,
`graph_delete_edge`, `graph_list`, `graph_drop`, `graph_traverse`,
`graph_weighted_shortest_path`, `graph_degree`, `graph_list_nodes`,
`graph_list_edges` and `graph_query`.

## LLM context

Read-only export of query results and schema, in TOON, JSON, Markdown or the
native format.

```rust,no_run
# fn main() -> tricoredb::Result<()> {
# let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
use tricoredb::{DocumentFilter, LlmSource, OutputFormat};

let bundle = db.llm_context(
    &[
        LlmSource::sql("SELECT id, name FROM users"),
        LlmSource::document_find("products", &DocumentFilter::all(), None),
    ],
    OutputFormat::Toon,
    None,
)?;
let schema = db.llm_schema(OutputFormat::Markdown, None)?;
# let _ = (bundle, schema); Ok(()) }
```

Sensitive fields are redacted by default — `LlmOptions::default()` matches the
server's own defaults, so build from it rather than from a zeroed struct.

## Admin

```rust,no_run
# fn main() -> tricoredb::Result<()> {
# let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
db.admin_ping()?;
let status = db.admin_status()?;
# let _ = status; Ok(()) }
```

Admin calls need the cluster module enabled on the server, even on a single
node. `db.ping()` checks the connection itself and reaches no module.

## Errors

Every fallible call returns `tricoredb::Error`. Branch on `kind()` and `code()`,
never on the message text:

```rust,no_run
# fn main() -> tricoredb::Result<()> {
# let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
use tricoredb::ErrorKind;

match db.query("SELECT * FROM missing") {
    Ok(rows) => println!("{} rows", rows.len()),
    Err(e) => match e.kind() {
        ErrorKind::InvalidArgument => {}     // refused here; nothing was sent
        ErrorKind::FeatureNotGranted => {}   // the server lacks a capability this call needs
        ErrorKind::Auth => {}                // the credentials were refused
        ErrorKind::Server => {}              // the request arrived; the operation failed
        ErrorKind::Timeout => {}             // no reply within the read timeout
        _ => println!("{e}"),
    },
}
# Ok(()) }
```

`is_connection_fatal()` says whether the connection can still be used: a server
refusal leaves it perfectly usable, while a timeout or a protocol failure does
not. A connection that failed that way is dropped, and every later call on it
reports the original failure instead of reading a stale reply as the next
answer.

**Leader redirects.** In a cluster, a write that reaches a follower fails with
`code() == "not_leader"`, which `is_redirect()` tests. When the leader is known,
`leader_hint()` holds its `host:port`; an empty hint means the leader is not
known yet, so wait and retry. This client does not follow redirects for you —
where to resend a write is your application's decision.

## TLS

TLS is off unless `Options::tls` is set. Without it the secret crosses the wire
in the clear, so use TLS for anything but local development.

```rust,no_run
# fn main() -> tricoredb::Result<()> {
use tricoredb::{Client, Options, TlsOptions};

let mut db = Client::connect(
    &Options::new("db.internal", 8427)
        .user("admin")
        .secret("your-password")
        .tls(TlsOptions::new().ca_file("/etc/tricore/ca.pem").server_name("db.internal")),
)?;
# let _ = db.ping(); Ok(()) }
```

With TLS on, the server certificate and host name are always verified. If
`ca_file` is not set the trust store is **empty**: the client never falls back
to the operating system's roots, so a wrong path fails closed instead of
trusting an unrelated CA.

For mutual TLS, `TlsOptions::client_identity(cert, key)` presents a client
certificate. Errors name a certificate or key file's *path*, never its
contents.

| `TlsOptions` field | Default | Meaning |
| --- | --- | --- |
| `ca_file` | `None` (empty trust store) | PEM bundle used to verify the server |
| `server_name` | `"localhost"` | Expected name (SNI and certificate check) |
| `client_cert_file` | `None` | PEM client certificate chain, for mutual TLS |
| `client_key_file` | `None` | PEM private key for that certificate |
| `danger_accept_invalid_certs` | `false` | **Development only.** Skips all verification. |

## Testing

```bash
cargo test
```

The unit tests and the scripted-peer tests in `tests/protocol.rs` need no
server. `tests/live.rs` starts a real `tricore-server`: point
`TRICORE_SERVER_BIN` at the binary to run it. Without that, those tests print
why and pass.

## License

[Apache License 2.0](LICENSE)
