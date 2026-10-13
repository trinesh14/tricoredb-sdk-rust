# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0] - 2026-10-13

Every method added here needs `tricore-server` **0.1.0-rc.3** or later. An
older server does not know these operations and answers each with an error;
the methods that existed before are unchanged and work against it as before.

### Added

- Cache, 53 operations: many-key reads and writes (`cache_mget`, `cache_mset`,
  `cache_warm`), conditional and atomic writes (`cache_set_if`,
  `cache_compare_and_swap`, `cache_get_del`, `cache_get_set`, `cache_get_ex`,
  `cache_append`, `cache_expire_at`), stale-while-revalidate
  (`cache_set_fresh`, `cache_get_fresh`, `cache_set_missing`), sorted sets
  (`cache_zadd`, `cache_zrem`, `cache_zscore`, `cache_zcard`, `cache_zrank`,
  `cache_zrange`, `cache_zrange_by_score`, their `_with_scores` forms,
  `cache_zincr_by`), locks with fencing tokens (`cache_lock_acquire`,
  `cache_lock_renew`, `cache_lock_release`), bitmaps (`cache_set_bit`,
  `cache_get_bit`, `cache_bit_count`, `cache_bit_pos`, `cache_bit_op`),
  HyperLogLog (`cache_pf_add`, `cache_pf_count`, `cache_pf_merge`), more list,
  set and hash operations (`cache_lmove`, `cache_ltrim`, `cache_blpop`,
  `cache_brpop`, `cache_sunion`, `cache_sinter`, `cache_sdiff`,
  `cache_srand_member`, `cache_hincr_by`), pipelines, transactions and scripts
  (`cache_pipeline`, `cache_transaction`, `cache_script_load`,
  `cache_script_run`), publish and poll (`cache_publish`, `cache_poll`), and
  the node and database operations (`cache_stats`, `cache_hot_keys`,
  `cache_purge_expired`, `cache_flush_database`, `cache_set_namespace_limit`,
  `cache_get_namespace_limit`).
- Documents, 13 operations: `document_replace`, `document_insert_many`,
  `document_delete_many`, `document_bulk_write` with `BulkOp` for its items,
  `document_rename_collection`, `document_purge_expired`, `document_explain`,
  `document_rebuild_index`, `document_set_collection_options`,
  `document_get_collection_options`, and transactions spanning requests
  (`document_begin_transaction`, `document_commit_transaction`,
  `document_rollback_transaction`).
- Vectors, 21 operations: `vector_upsert_many`, `vector_upsert_text`,
  `vector_upsert_named`, `vector_add_named_vector`, `vector_delete_many`,
  `vector_delete_by_filter`, `vector_update_by_filter`, `vector_count`,
  `vector_search_batch`, `vector_search_multi`, `vector_search_by_examples`,
  `vector_embed`, `vector_set_alias`, `vector_list_aliases`,
  `vector_list_partitions`, `vector_rename_collection`,
  `vector_copy_collection`, `vector_set_collection_config`,
  `vector_rebuild_index`, `vector_index_stats` and `vector_measure_recall`.
- Graphs, 29 operations: `graph_add_many`, `graph_remove_many`,
  `graph_update_node`, `graph_update_edge`, `graph_copy`, `graph_import_csv`,
  `graph_dfs`, `graph_subgraph`, `graph_k_shortest_paths`, `graph_all_paths`,
  `graph_connected_components`, `graph_detect_cycle`, `graph_centrality`,
  `graph_communities`, `graph_triangle_count`, `graph_node_similarity`,
  `graph_page_rank`, `graph_stats`, `graph_schema`, `graph_check_consistency`,
  `graph_create_node_index`, `graph_create_constraint`,
  `graph_drop_schema_object`, `graph_list_schema_objects`, `graph_find_nodes`,
  `graph_execute`, and transactions (`graph_begin_transaction`,
  `graph_commit_transaction`, `graph_rollback_transaction`).
- `Client::set_request_txn` and `Client::request_txn`: the document or graph
  transaction named on every later request. Committing or rolling a
  transaction back through this crate clears it.

### Fixed

- A `BEGIN` sent inside an open transaction opens a nested block, and the
  `COMMIT` or `ROLLBACK` that closes it leaves the transaction open.
  `in_transaction` reported false after it, so a `Pool` could hand the
  connection on mid-transaction. It now follows the reply: one carrying
  `nested_depth` closed a nested block only. `TransactionResult` gains
  `nested_depth`, and a pool rolls back every block a closure left open.

### Changed

- `VectorHit` has two new public fields, `vector` and `reason`, so building one
  with a struct literal needs them. Both are `None` unless a search asked for
  vectors or an explanation.

### Notes

- `cache_hot_keys` returns the server's payload as JSON rather than a typed
  result. The server answers it only when `[cache] hot_keys` is above zero, and
  the server this release was tested against had it off, so its answer was
  never observed.
- Tests can now run against a server that is already running: set
  `TRICORE_TEST_ADDR` (and `TRICORE_TEST_USER`, `TRICORE_TEST_SECRET`) and
  `tests/live_rc3.rs` uses it instead of starting a binary.

## [0.2.0] - 2026-09-17

### Added

- Following the leader: a request refused with `not_leader` is sent again,
  to the leader the refusal names (over a new connection that replaces the
  current one) or, while no leader is known, to the same node after a pause.
  On by default; `Options::follow_leader(false)` restores the previous
  behaviour, and `leader_retries` (5) and `leader_retry_delay` (250 ms) bound
  it. Never inside a session transaction.
- `OUTCOME_UNKNOWN`: a request that may or may not have been applied. It is
  never sent again.
- `Client::address()`.

### Changed

- `Options` has three new public fields, so building it with a struct literal
  needs `..Options::default()`.

## [0.1.0] - 2026-09-16

First release.

### Added

- Blocking client for TriCoreDB's native `tricore` protocol, on Rust 1.85 and
  later. Dependencies: `serde` and `serde_json`, plus `rustls` only when the
  default `tls` feature is on.
- SQL: `query`, `execute`, and `query_params` / `execute_params` with `?`
  placeholders bound by the server. The `params!` macro builds the values;
  `Param::decimal` keeps exact digits and refuses an exponent.
- Transactions: one-request scripts (`transaction`) and session transactions
  (`begin`, `commit`, `rollback`, `with_transaction`).
- `Pool`: a bounded, thread-safe pool that lends a connection to a closure and
  never returns one with a transaction still open.
- Documents, vectors, graphs, cache (keys, lists, sets, hashes, streams), LLM
  context export and the admin reads.
- `Error` with a `kind`, the server's own `code`, and a leader hint;
  `is_redirect` and `is_connection_fatal` for the two questions callers
  actually ask.
- TLS and mutual TLS through `TlsOptions`.
- `Client::request` for an operation this crate has no typed method for.

### Security

- Operations that need a capability the server did not grant — server-side
  parameters, session transactions — fail with `ErrorKind::FeatureNotGranted`
  before anything is sent, instead of falling back to a weaker behaviour.
- With TLS and no CA file, the trust store is empty: the client never falls
  back to the operating system's root certificates.
- A frame's declared length is checked against the protocol's ceiling before a
  payload byte is read, so a wrong or hostile peer cannot make the client
  allocate what it claimed.
- A connection that timed out or failed mid-frame is dropped rather than
  reused: the late reply can never be read as the answer to the next request.
- Errors name a certificate or key file's path, never its contents.

[Unreleased]: https://github.com/trinesh14/tricoredb-sdk-rust/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/trinesh14/tricoredb-sdk-rust/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/trinesh14/tricoredb-sdk-rust/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/trinesh14/tricoredb-sdk-rust/releases/tag/v0.1.0
