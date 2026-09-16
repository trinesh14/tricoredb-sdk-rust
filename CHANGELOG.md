# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/trinesh14/tricoredb-sdk-rust/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/trinesh14/tricoredb-sdk-rust/releases/tag/v0.1.0
