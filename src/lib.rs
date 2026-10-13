//! Blocking Rust client for [TriCoreDB](https://hub.docker.com/r/trinesh14/tricoredb):
//! SQL, documents, cache, vectors, graphs and LLM context over one connection.
//!
//! It speaks TriCoreDB's own `tricore` protocol directly over TCP, optionally
//! inside TLS. There is no async runtime: calls block, and a [`Pool`] is what
//! serves concurrent work.
//!
//! ```no_run
//! use tricoredb::{params, Client, Options};
//!
//! # fn main() -> tricoredb::Result<()> {
//! let mut db = Client::connect(&Options::new("127.0.0.1", 8427).user("admin").secret("pw"))?;
//!
//! db.execute("CREATE TABLE IF NOT EXISTS users (id INT PRIMARY KEY, name TEXT)")?;
//! db.execute_params("INSERT INTO users VALUES (?, ?)", &params![1, "O'Hara"])?;
//!
//! let rows = db.query_params("SELECT name FROM users WHERE id = ?", &params![1])?;
//! assert_eq!(rows.get(0, "name"), Some("O'Hara"));
//!
//! db.cache_set("sessions", "u1", b"token")?;
//! let token = db.cache_get("sessions", "u1")?; // None on a miss
//! # let _ = token;
//! # Ok(())
//! # }
//! ```
//!
//! # One connection, one request at a time
//!
//! The protocol is a single request/response stream, so every call takes
//! `&mut self`. Sharing a [`Client`] between threads does not compile, which is
//! the frame interleaving this design rules out rather than documents. Use a
//! [`Pool`] when you need several requests at once.
//!
//! # Values are bound by the server
//!
//! [`Client::execute_params`] and [`Client::query_params`] send the values
//! beside the statement; the server substitutes them at positions its grammar
//! has already fixed, so a value can never be read as SQL. When a server does
//! not offer that capability, those calls fail before sending anything instead
//! of quietly building the statement text here — see [`ErrorKind::FeatureNotGranted`].
//!
//! # Errors
//!
//! Every fallible call returns [`Error`]. Branch on [`Error::kind`] and
//! [`Error::code`], never on the message text. A write refused by a follower in
//! a cluster has [`Error::is_redirect`] true and, when the cluster knows one,
//! [`Error::leader_hint`]. By default the client follows it itself: see
//! [`Options::follow_leader`].
//!
//! # Features
//!
//! * `tls` (default): TLS and mutual TLS through [`TlsOptions`], built on
//!   rustls. Turn it off for a build with no cryptography dependencies.

#![forbid(unsafe_code)]
#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

mod cache;
mod client;
mod document;
mod error;
mod frame;
mod graph;
mod llm;
mod options;
mod params;
mod pool;
mod response;
mod serde_null;
mod sql;
#[cfg(feature = "tls")]
mod tls;
mod transport;
mod vector;
mod wire;

pub use crate::cache::{
    BitOpKind, CacheCommand, CacheEntry, CacheEvictionStats, CacheExpiry, CacheKeyInfo, CacheLock,
    CacheLockRenewal, CacheMemoryStats, CachePair, CacheReply, CacheStats, ChannelMessage,
    CompareAndSwap, ExpireAtOutcome, FreshRead, FreshState, ListEnd, NamespaceLimit,
    PolledMessages, ScoreBound, ScoredMember, SetCondition, StreamEntry, TrimCounts, WarmCounts,
    ZAddCounts,
};
pub use crate::client::Client;
pub use crate::document::{
    AccumulatorOp, AggregateStage, BulkOp, BulkWriteResult, CollectionOptions, Document,
    DocumentCommit, DocumentFilter, DocumentIndex, DocumentPlan, DocumentSchema, DocumentStats,
    DocumentUpdate, ExplainOptions, FieldRule, FieldType, GroupAccumulator, GroupKey, IndexRebuild,
    PurgeCounts, ReplaceOptions, ReplaceResult, ReturnDocument, SortKey, UpdateCounts,
};
pub use crate::error::{Error, ErrorKind, Result, NOT_LEADER, OUTCOME_UNKNOWN};
pub use crate::frame::{Frame, Tag, FRAME_VERSION, MAX_CONTROL_PAYLOAD, MAX_DATA_PAYLOAD};
pub use crate::graph::{
    CentralityMeasure, CentralityOptions, CommunityOptions, ComponentKind, ComponentOptions,
    ConstraintKind, ConstraintTarget, GraphCentrality, GraphCommunities, GraphComponents,
    GraphConsistency, GraphConstraint, GraphCounts, GraphCycle, GraphCycleCheck, GraphDirection,
    GraphEdge, GraphEdgeInput, GraphEdgePage, GraphEdgeTypeSchema, GraphFindings, GraphFoundNodes,
    GraphGroup, GraphIndex, GraphLabelSchema, GraphMirrorCheck, GraphNeighbor, GraphNode,
    GraphNodeInput, GraphNodePage, GraphNodeTriangles, GraphPageRank, GraphPath, GraphPathEntry,
    GraphPaths, GraphPropertySchema, GraphRemoved, GraphRows, GraphSchema, GraphSchemaObjects,
    GraphScore, GraphStats, GraphSubgraph, GraphTraversal, GraphTriangles, GraphVisit,
    GraphWriteSummary, KPathOptions, NeighborOptions, NodeUpdate, PageRankOptions, PathOptions,
    SimilarityMetric, SimilarityOptions, TraverseOptions, WeightedPathOptions,
};
pub use crate::llm::{LlmOptions, LlmSource, OutputFormat};
pub use crate::options::{
    Options, TlsOptions, ALL_FEATURES, DEFAULT_DATABASE, DEFAULT_PORT, FEATURE_CORRELATION_ID,
    FEATURE_SERVER_PARAMS, FEATURE_SESSION_TXN, VERSION,
};
pub use crate::params::Param;
pub use crate::pool::Pool;
pub use crate::response::Response;
pub use crate::sql::{Rows, Statement, TransactionResult};
pub use crate::vector::{
    AnnIndex, Embedding, FilterStrategy, FusedHit, FusedSearchResult, FusionMethod, NamedVector,
    RecallOptions, ScoreMetric, SparseVector, TextUpsert, VectorAlias, VectorCollectionInfo,
    VectorCollectionSummary, VectorConfig, VectorConfigChange, VectorExample, VectorHit,
    VectorIndexRebuild, VectorIndexStats, VectorItem, VectorMetric, VectorPage, VectorPartition,
    VectorPartitions, VectorQuantization, VectorRecall, VectorSearchOptions, VectorSearchResult,
    VectorUpsert,
};
