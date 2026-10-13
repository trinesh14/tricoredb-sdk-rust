//! Vectors: fixed-dimension collections, similarity search and metadata
//! filters.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::client::Client;
use crate::error::{Error, Result};

/// How a collection measures closeness. Fixed when the collection is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VectorMetric {
    /// Cosine similarity: higher is closer.
    #[default]
    Cosine,
    /// Dot product: higher is closer.
    Dot,
    /// Squared Euclidean distance, **returned negated** so that higher is still
    /// closer. An l2 score is therefore `<= 0`, and `-0.02` is nearer than
    /// `-196.0`. See [`VectorHit::score`].
    L2,
}

/// How the search index stores vectors in memory.
///
/// This is an index-level choice only: the durable records always keep full
/// `f32` precision, so [`Client::vector_get`] returns byte-identical values
/// either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VectorQuantization {
    /// Full `f32` vectors in the index. The default.
    #[default]
    None,
    /// `int8`-quantized vectors in the index, roughly four times smaller, with
    /// candidates re-ranked against the durable `f32` vectors.
    Int8,
}

/// One stored vector and its metadata.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VectorItem {
    /// The vector's id.
    pub id: String,
    /// The vector itself.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub vector: Vec<f32>,
    /// Metadata stored alongside it.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub metadata: Map<String, Value>,
}

/// One search result.
///
/// The score is a similarity, never a distance: **higher is closer under every
/// metric**, and results come back best first. Sorting these ascending, or
/// reading the score as a distance, inverts an l2 ranking while a "is the
/// expected id in the top k" check still passes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VectorHit {
    /// The matching vector's id.
    pub id: String,
    /// Its similarity to the query vector.
    #[serde(default)]
    pub score: f32,
    /// Its metadata.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub metadata: Map<String, Value>,
    /// The vector itself, when the search asked for vectors.
    #[serde(default)]
    pub vector: Option<Vec<f32>>,
    /// Why it matched, when the search asked for an explanation.
    #[serde(default)]
    pub reason: Option<String>,
}

/// A search's hits, and which path answered it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VectorSearchResult {
    /// The hits, best first.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub results: Vec<VectorHit>,
    /// `"flat"` for the exact scan a small collection gets, `"hnsw"` or
    /// `"hnsw-int8"` for the approximate index.
    #[serde(default)]
    pub index: String,
}

/// One entry of [`Client::vector_list_collections`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VectorCollectionSummary {
    /// The collection's name.
    pub name: String,
    /// The length every vector in it must have.
    #[serde(default)]
    pub dimension: usize,
    /// The metric searches rank by.
    #[serde(default)]
    pub metric: VectorMetric,
}

/// A collection's metadata and its live vector count.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VectorCollectionInfo {
    /// The collection's name.
    pub collection: String,
    /// The length every vector in it must have.
    #[serde(default)]
    pub dimension: usize,
    /// The metric searches rank by.
    #[serde(default)]
    pub metric: VectorMetric,
    /// How many vectors are stored.
    #[serde(default)]
    pub count: i64,
    /// The index quantization the collection was created with.
    #[serde(default)]
    pub quantization: VectorQuantization,
}

/// One page of [`Client::vector_list_vectors`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VectorPage {
    /// The collection's name.
    pub collection: String,
    /// How many vectors are on this page.
    #[serde(default)]
    pub count: usize,
    /// This page's vectors.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub vectors: Vec<VectorItem>,
    /// Whether more vectors remain past this page.
    #[serde(default)]
    pub truncated: bool,
    /// The collection's full count.
    #[serde(default)]
    pub total: i64,
}

/// The measure a search scores by, when it should differ from the
/// collection's own metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreMetric {
    /// Cosine similarity.
    Cosine,
    /// Dot product.
    Dot,
    /// Squared Euclidean distance, negated.
    L2,
    /// Manhattan distance, negated.
    L1,
    /// Chebyshev distance, negated.
    Linf,
}

/// How a filtered search combines the filter with the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterStrategy {
    /// Let the server choose. The default.
    #[default]
    Auto,
    /// Walk the index and test each candidate.
    Graph,
    /// Scan the vectors the filter admits.
    Exact,
}

/// Which approximate index a search uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnIndex {
    /// The HNSW graph.
    Hnsw,
    /// An inverted file index.
    Ivf,
    /// An inverted file index over product-quantized vectors.
    IvfPq,
}

/// How [`Client::vector_search_multi`] merges its queries' rankings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FusionMethod {
    /// Reciprocal rank fusion. The default.
    #[default]
    Rrf,
    /// Each vector's best score across the queries.
    Max,
    /// Each vector's mean score across the queries.
    Mean,
}

/// A sparse vector: parallel lists of term indices and their weights.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SparseVector {
    /// The indices that carry a weight.
    #[serde(default)]
    pub indices: Vec<u32>,
    /// The weight at each index.
    #[serde(default)]
    pub values: Vec<f32>,
}

/// What a batch or example search should apply beyond its query vectors.
///
/// Every field left at its default is left off the wire, so the server's own
/// default applies.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VectorSearchOptions {
    /// Keep only vectors whose metadata matches this filter.
    pub filter: Option<Value>,
    /// Drop hits scoring below this.
    pub min_score: Option<f32>,
    /// Return each hit's vector with it.
    pub include_vectors: bool,
    /// Skip this many hits from the front.
    pub offset: Option<usize>,
    /// Return each hit's metadata. `None` leaves the server's default, which
    /// is to include it.
    pub include_metadata: Option<bool>,
    /// Drop hits farther than this distance.
    pub max_distance: Option<f32>,
    /// The size of the candidate list the index walk keeps.
    pub ef_search: Option<usize>,
    /// Scan every vector instead of using the index.
    pub exact: bool,
    /// Score by this measure instead of the collection's metric.
    pub score_metric: Option<ScoreMetric>,
    /// How to combine the filter with the index.
    pub filter_strategy: Option<FilterStrategy>,
    /// Search this named vector instead of the collection's own.
    pub using: Option<String>,
    /// Restrict the search to this tenant.
    pub tenant: Option<String>,
    /// Restrict the search to this namespace.
    pub namespace: Option<String>,
    /// A sparse query to combine with the dense one.
    pub sparse: Option<SparseVector>,
    /// The dense share of a hybrid score, from 0 to 1.
    pub hybrid_alpha: Option<f32>,
    /// Attach to each hit the reason it matched.
    pub explain: bool,
    /// An expression that rescores each hit.
    pub score_expr: Option<String>,
    /// The query text an explanation should refer to.
    pub explain_text: Option<String>,
    /// Which approximate index to use.
    pub ann: Option<AnnIndex>,
    /// How many inverted-file cells to probe.
    pub nprobe: Option<usize>,
}

impl VectorSearchOptions {
    fn write(&self, body: &mut Map<String, Value>) {
        fn put(body: &mut Map<String, Value>, key: &str, value: Option<impl Serialize>) {
            if let Some(value) = value {
                body.insert(key.to_string(), json!(value));
            }
        }
        put(body, "filter", self.filter.as_ref());
        put(body, "min_score", self.min_score);
        put(
            body,
            "include_vectors",
            self.include_vectors.then_some(true),
        );
        put(body, "offset", self.offset);
        put(body, "include_metadata", self.include_metadata);
        put(body, "max_distance", self.max_distance);
        put(body, "ef_search", self.ef_search);
        put(body, "exact", self.exact.then_some(true));
        put(body, "score_metric", self.score_metric);
        put(body, "filter_strategy", self.filter_strategy);
        put(body, "using", self.using.as_ref());
        put(body, "tenant", self.tenant.as_ref());
        put(body, "namespace", self.namespace.as_ref());
        put(body, "sparse", self.sparse.as_ref());
        put(body, "hybrid_alpha", self.hybrid_alpha);
        put(body, "explain", self.explain.then_some(true));
        put(body, "score_expr", self.score_expr.as_ref());
        put(body, "explain_text", self.explain_text.as_ref());
        put(body, "ann", self.ann);
        put(body, "nprobe", self.nprobe);
    }
}

/// One example for [`Client::vector_search_by_examples`]: a stored vector
/// named by id, or a vector given outright.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum VectorExample {
    /// The stored vector with this id.
    Id(String),
    /// This vector.
    Vector(Vec<f32>),
}

impl VectorExample {
    /// The stored vector with this id.
    pub fn id(id: &str) -> Self {
        VectorExample::Id(id.to_string())
    }
}

/// One vector of [`Client::vector_upsert_many`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VectorUpsert {
    /// The vector's id.
    pub id: String,
    /// The vector, at the collection's dimension.
    pub vector: Vec<f32>,
    /// Metadata stored alongside it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

impl VectorUpsert {
    /// A vector with no metadata.
    pub fn new(id: &str, vector: impl Into<Vec<f32>>) -> Self {
        Self {
            id: id.to_string(),
            vector: vector.into(),
            metadata: None,
        }
    }

    /// Attach metadata.
    pub fn metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// One text of [`Client::vector_upsert_text`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TextUpsert {
    /// The id the embedded vector is stored under.
    pub id: String,
    /// The text to embed. It is kept in the metadata under `text`.
    pub text: String,
    /// Metadata stored alongside it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

impl TextUpsert {
    /// A text with no metadata.
    pub fn new(id: &str, text: &str) -> Self {
        Self {
            id: id.to_string(),
            text: text.to_string(),
            metadata: None,
        }
    }

    /// Attach metadata.
    pub fn metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// What [`Client::vector_embed`] produced.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Embedding {
    /// The embedder's name. `hash-v1` is deterministic feature hashing:
    /// lexical, with no model behind it.
    #[serde(default)]
    pub embedder: String,
    /// The dimension asked for.
    #[serde(default)]
    pub dimension: usize,
    /// One dense vector per text, in the order given.
    #[serde(default)]
    pub vectors: Vec<Vec<f32>>,
    /// One sparse vector per text, in the order given.
    #[serde(default)]
    pub sparse: Vec<SparseVector>,
}

/// One alias and the collection it names.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VectorAlias {
    /// The alias.
    pub alias: String,
    /// The collection every operation naming the alias acts on.
    #[serde(default)]
    pub collection: String,
}

/// One partition of a collection.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VectorPartition {
    /// The partition's value of the field.
    pub name: String,
    /// How many vectors carry it.
    #[serde(default)]
    pub count: i64,
}

/// The partitions of a collection, as [`Client::vector_list_partitions`]
/// reports them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VectorPartitions {
    /// The metadata field the collection was partitioned by.
    #[serde(default)]
    pub field: String,
    /// Each distinct value of the field, with its vector count.
    #[serde(default)]
    pub partitions: Vec<VectorPartition>,
    /// How many vectors carry no value for the field.
    #[serde(default)]
    pub unpartitioned: i64,
}

/// What [`Client::vector_rebuild_index`] did.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VectorIndexRebuild {
    /// Whether the index was rebuilt.
    #[serde(default)]
    pub rebuilt: bool,
    /// How many vectors went into it.
    #[serde(default)]
    pub vectors: i64,
    /// How long the build took, in milliseconds.
    #[serde(default)]
    pub build_ms: i64,
    /// Whether the new index was persisted.
    #[serde(default)]
    pub persisted: bool,
    /// How many tombstones the rebuild cleared.
    #[serde(default)]
    pub tombstones_cleared: i64,
}

/// The shape of a collection's search index.
///
/// A collection small enough to be scanned has no graph, so everything past
/// `stored` is `None` for it and [`VectorIndexStats::index`] is `"flat"`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VectorIndexStats {
    /// The collection's name.
    #[serde(default)]
    pub collection: String,
    /// `"flat"` for an exact scan, `"hnsw"` for the graph.
    #[serde(default)]
    pub index: String,
    /// How many vectors are stored.
    #[serde(default)]
    pub stored: i64,
    /// Below this many vectors a search scans instead of using a graph.
    #[serde(default)]
    pub flat_scan_threshold: Option<i64>,
    /// How many layers the graph has.
    #[serde(default)]
    pub layers: Option<i64>,
    /// How many nodes the graph holds, tombstones included.
    #[serde(default)]
    pub nodes: Option<i64>,
    /// How many of them are live.
    #[serde(default)]
    pub live: Option<i64>,
    /// How many nodes a delete or re-upsert left behind.
    #[serde(default)]
    pub tombstones: Option<i64>,
    /// How many live nodes the entry point can reach.
    #[serde(default)]
    pub reachable: Option<i64>,
    /// How many it cannot.
    #[serde(default)]
    pub unreachable: Option<i64>,
    /// Whether the server recommends a rebuild.
    #[serde(default)]
    pub should_rebuild: Option<bool>,
    /// Whether the index is persisted.
    #[serde(default)]
    pub persisted: Option<bool>,
    /// The id of the entry point.
    #[serde(default)]
    pub entry: Option<String>,
    /// Live nodes on each layer, the bottom first.
    #[serde(default)]
    pub live_per_layer: Option<Vec<i64>>,
    /// The degree extremes and mean, as `{min, mean, max}`.
    #[serde(default)]
    pub degree: Option<Value>,
    /// The index's memory, as `{vector_bytes, overhead_bytes}`.
    #[serde(default)]
    pub memory: Option<Value>,
    /// The graph's parameters, as `{m, m0, ef_construction, ef_search}`.
    #[serde(default)]
    pub parameters: Option<Value>,
}

/// What [`Client::vector_measure_recall`] should sample. Every `None` leaves
/// the server's default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecallOptions {
    /// How many stored vectors to search with. The default is 100, capped at
    /// 1000.
    pub sample: Option<usize>,
    /// How long a result list to compare. The default is 10, capped at 100.
    pub top_k: Option<usize>,
    /// The candidate list size to measure at.
    pub ef_search: Option<usize>,
}

/// The recall [`Client::vector_measure_recall`] measured.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VectorRecall {
    /// The collection's name.
    #[serde(default)]
    pub collection: String,
    /// `"flat"` or `"hnsw"`.
    #[serde(default)]
    pub index: String,
    /// Whether anything was measured. A collection small enough to be scanned
    /// has nothing to measure, and its recall is 1 by construction.
    #[serde(default)]
    pub measured: bool,
    /// The mean overlap of the index's results with an exact scan's.
    #[serde(default)]
    pub recall: f64,
    /// The worst single query's overlap.
    #[serde(default)]
    pub min_recall: Option<f64>,
    /// How many queries were run.
    #[serde(default)]
    pub queries: Option<i64>,
    /// The result list length that was compared.
    #[serde(default)]
    pub top_k: Option<i64>,
    /// The candidate list size it was measured at.
    #[serde(default)]
    pub ef_search: Option<i64>,
    /// How many queries fell back to a scan.
    #[serde(default)]
    pub flat_fallbacks: Option<i64>,
    /// Why nothing was measured, when nothing was.
    #[serde(default)]
    pub reason: Option<String>,
}

/// One hit of a fused ranking.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FusedHit {
    /// The matching vector's id.
    pub id: String,
    /// Its fused score. Higher is better, but the scale is the fusion
    /// method's, not the collection metric's.
    #[serde(default)]
    pub score: f32,
    /// How many of the queries ranked it.
    #[serde(default)]
    pub matched: usize,
    /// Its metadata.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub metadata: Map<String, Value>,
}

/// The answer to [`Client::vector_search_multi`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FusedSearchResult {
    /// The fusion method that was applied.
    #[serde(default)]
    pub fusion: FusionMethod,
    /// How many query vectors were run.
    #[serde(default)]
    pub queries: usize,
    /// How deep each query ranked before fusing.
    #[serde(default)]
    pub depth: usize,
    /// The fused hits, best first.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub results: Vec<FusedHit>,
}

/// What [`Client::vector_set_collection_config`] should change. A `None`
/// leaves that setting as it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VectorConfigChange {
    /// Store every vector, and rank every query, at unit length. Can only
    /// change on an empty collection.
    pub normalize: Option<bool>,
    /// The HNSW graph's `m`. Takes effect at the next build, which the call
    /// forces.
    pub m: Option<usize>,
    /// The HNSW graph's `ef_construction`.
    pub ef_construction: Option<usize>,
    /// Require every vector to carry a string `_tenant`, and every search to
    /// name one.
    pub multi_tenant: Option<bool>,
}

/// One named vector declared on a collection.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NamedVector {
    /// Its name, which a search passes as `using`.
    pub name: String,
    /// Its dimension.
    #[serde(default)]
    pub dimension: usize,
    /// Its metric.
    #[serde(default)]
    pub metric: VectorMetric,
}

/// A collection's configuration after [`Client::vector_set_collection_config`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VectorConfig {
    /// The collection's name.
    #[serde(default)]
    pub collection: String,
    /// Whether vectors are stored at unit length.
    #[serde(default)]
    pub normalize: bool,
    /// The HNSW graph's `m`.
    #[serde(default)]
    pub m: usize,
    /// The HNSW graph's `ef_construction`.
    #[serde(default)]
    pub ef_construction: usize,
    /// Whether every vector must carry a `_tenant`.
    #[serde(default)]
    pub multi_tenant: bool,
    /// The named vectors declared on the collection.
    #[serde(default)]
    pub named: Vec<NamedVector>,
}

fn op(body: Value) -> Value {
    json!({"Vector": body})
}

impl Client {
    /// Create a collection. The dimension and metric are fixed for its
    /// lifetime.
    pub fn vector_create_collection(
        &mut self,
        collection: &str,
        dimension: usize,
        metric: VectorMetric,
    ) -> Result<()> {
        self.vector_create_collection_quantized(
            collection,
            dimension,
            metric,
            VectorQuantization::None,
        )
    }

    /// Create a collection with an explicit index quantization.
    pub fn vector_create_collection_quantized(
        &mut self,
        collection: &str,
        dimension: usize,
        metric: VectorMetric,
        quantization: VectorQuantization,
    ) -> Result<()> {
        self.send(op(json!({"CreateCollection": {
            "collection": collection,
            "dimension": dimension,
            "metric": metric,
            "quantization": quantization,
        }})))
        .map(|_| ())
    }

    /// Drop a collection and every vector in it. Dropping one that does not
    /// exist is an error.
    pub fn vector_drop_collection(&mut self, collection: &str) -> Result<()> {
        self.send(op(json!({"DropCollection": {"collection": collection}})))
            .map(|_| ())
    }

    /// Describe every vector collection in the database.
    pub fn vector_list_collections(&mut self) -> Result<Vec<VectorCollectionSummary>> {
        #[derive(Deserialize)]
        struct Details {
            #[serde(default)]
            details: Vec<VectorCollectionSummary>,
        }
        let response = self.send(op(Value::String("ListCollections".into())))?;
        let out: Details = response.decode("ListCollections")?;
        Ok(out.details)
    }

    /// Read one collection's metadata and live count.
    pub fn vector_describe_collection(&mut self, collection: &str) -> Result<VectorCollectionInfo> {
        let response = self.send(op(
            json!({"DescribeCollection": {"collection": collection}}),
        ))?;
        response.decode("DescribeCollection")
    }

    /// Store a vector under an id, replacing whatever was there.
    ///
    /// The vector's length must equal the collection's dimension; a mismatch is
    /// refused rather than padded or truncated.
    pub fn vector_upsert(
        &mut self,
        collection: &str,
        id: &str,
        vector: &[f32],
        metadata: Option<&Map<String, Value>>,
    ) -> Result<()> {
        self.send(op(json!({"Upsert": {
            "collection": collection,
            "id": id,
            "vector": vector,
            "metadata": metadata,
        }})))
        .map(|_| ())
    }

    /// Fetch one stored vector. `None` when there is no such id.
    pub fn vector_get(&mut self, collection: &str, id: &str) -> Result<Option<VectorItem>> {
        let response = self.send(op(json!({"Get": {"collection": collection, "id": id}})))?;
        let data = response.expect("Json", "Get")?;
        if data.is_null() {
            return Ok(None);
        }
        response.decode("Get").map(Some)
    }

    /// Delete one vector. Deleting an absent id is not an error; a missing
    /// collection is.
    pub fn vector_delete(&mut self, collection: &str, id: &str) -> Result<()> {
        self.send(op(json!({"Delete": {"collection": collection, "id": id}})))
            .map(|_| ())
    }

    /// The `top_k` nearest vectors to a query vector.
    ///
    /// `top_k` must be between 1 and 1000, and the query vector's length must
    /// equal the collection's dimension.
    pub fn vector_search(
        &mut self,
        collection: &str,
        vector: &[f32],
        top_k: usize,
    ) -> Result<VectorSearchResult> {
        self.vector_search_filtered(collection, vector, top_k, &BTreeMap::new())
    }

    /// A search restricted to vectors whose metadata matches every entry of
    /// `filter`.
    ///
    /// Exact equality on top-level fields only: the server implements no ranges
    /// and no nesting here. An empty filter means no filtering.
    pub fn vector_search_filtered(
        &mut self,
        collection: &str,
        vector: &[f32],
        top_k: usize,
        filter: &BTreeMap<String, Value>,
    ) -> Result<VectorSearchResult> {
        let filter = if filter.is_empty() {
            Value::Null
        } else {
            json!(filter)
        };
        let response = self.send(op(json!({"Search": {
            "collection": collection,
            "vector": vector,
            "top_k": top_k,
            "filter": filter,
        }})))?;
        response.decode("Search")
    }

    /// One page of a collection's vectors, ordered by id.
    ///
    /// `limit` of `None` asks for the server's default (500, capped at 1000);
    /// `offset` skips that many.
    pub fn vector_list_vectors(
        &mut self,
        collection: &str,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<VectorPage> {
        let mut body = Map::new();
        body.insert("collection".into(), json!(collection));
        if let Some(limit) = limit.filter(|l| *l > 0) {
            body.insert("limit".into(), json!(limit));
        }
        if let Some(offset) = offset.filter(|o| *o > 0) {
            body.insert("offset".into(), json!(offset));
        }
        let response = self.send(op(json!({"ListVectors": Value::Object(body)})))?;
        response.decode("ListVectors")
    }

    /// Embed texts with the server's built-in embedder and return the
    /// vectors. Nothing is stored.
    pub fn vector_embed(&mut self, texts: &[&str], dimension: usize) -> Result<Embedding> {
        let response = self.send(op(json!({"Embed": {
            "texts": texts,
            "dimension": dimension,
        }})))?;
        response.decode("Embed")
    }

    /// Embed each text at the collection's dimension and upsert the vectors as
    /// one atomic batch, returning how many were written.
    pub fn vector_upsert_text(&mut self, collection: &str, items: &[TextUpsert]) -> Result<i64> {
        let response = self.send(op(json!({"UpsertText": {
            "collection": collection,
            "items": items,
        }})))?;
        Ok(response.decode::<Upserted>("UpsertText")?.upserted)
    }

    /// Point an alias at a collection; `None` removes it. Every operation
    /// naming the alias acts on the collection, so a rebuilt collection can
    /// replace the old one by moving the alias.
    pub fn vector_set_alias(&mut self, alias: &str, collection: Option<&str>) -> Result<()> {
        self.send(op(json!({"SetAlias": {
            "alias": alias,
            "collection": collection,
        }})))
        .map(|_| ())
    }

    /// Every alias and the collection it names.
    pub fn vector_list_aliases(&mut self) -> Result<Vec<VectorAlias>> {
        #[derive(Deserialize)]
        struct Aliases {
            #[serde(default)]
            aliases: Vec<VectorAlias>,
        }
        let response = self.send(op(Value::String("ListAliases".into())))?;
        let out: Aliases = response.decode("ListAliases")?;
        Ok(out.aliases)
    }

    /// Declare a named vector on a collection: a second vector per item, with
    /// its own dimension and metric.
    pub fn vector_add_named_vector(
        &mut self,
        collection: &str,
        name: &str,
        dimension: usize,
        metric: VectorMetric,
    ) -> Result<()> {
        self.send(op(json!({"AddNamedVector": {
            "collection": collection,
            "name": name,
            "dimension": dimension,
            "metric": metric,
        }})))
        .map(|_| ())
    }

    /// Write an item's vectors by name, each with the same metadata. The name
    /// `default` is the collection's own vector.
    pub fn vector_upsert_named(
        &mut self,
        collection: &str,
        id: &str,
        vectors: &BTreeMap<String, Vec<f32>>,
        metadata: Option<&Map<String, Value>>,
    ) -> Result<()> {
        self.send(op(json!({"UpsertNamed": {
            "collection": collection,
            "id": id,
            "vectors": vectors,
            "metadata": metadata,
        }})))
        .map(|_| ())
    }

    /// Write many vectors as one atomic batch, returning how many were
    /// written. One bad item refuses the whole batch, and so does a repeated
    /// id.
    pub fn vector_upsert_many(&mut self, collection: &str, items: &[VectorUpsert]) -> Result<i64> {
        let response = self.send(op(json!({"UpsertMany": {
            "collection": collection,
            "items": items,
        }})))?;
        Ok(response.decode::<Upserted>("UpsertMany")?.upserted)
    }

    /// Delete many vectors by id as one atomic batch, returning how many of
    /// the ids were present.
    pub fn vector_delete_many(&mut self, collection: &str, ids: &[&str]) -> Result<i64> {
        let response = self.send(op(json!({"DeleteMany": {
            "collection": collection,
            "ids": ids,
        }})))?;
        Ok(response.decode::<DeletedCount>("DeleteMany")?.deleted)
    }

    /// Run several query vectors against one collection in one round trip.
    ///
    /// One result list per query, in the order asked, each what a single
    /// search with the same arguments returns. Not a snapshot.
    pub fn vector_search_batch(
        &mut self,
        collection: &str,
        vectors: &[Vec<f32>],
        top_k: usize,
        options: &VectorSearchOptions,
    ) -> Result<Vec<Vec<VectorHit>>> {
        #[derive(Deserialize)]
        struct Batch {
            #[serde(default)]
            results: Vec<Vec<VectorHit>>,
        }
        let mut body = Map::new();
        body.insert("collection".into(), json!(collection));
        body.insert("vectors".into(), json!(vectors));
        body.insert("top_k".into(), json!(top_k));
        options.write(&mut body);
        let response = self.send(op(json!({"SearchBatch": Value::Object(body)})))?;
        let out: Batch = response.decode("SearchBatch")?;
        Ok(out.results)
    }

    /// Search with several query vectors and return one fused ranking.
    pub fn vector_search_multi(
        &mut self,
        collection: &str,
        vectors: &[Vec<f32>],
        top_k: usize,
        fusion: FusionMethod,
        filter: Option<&Value>,
    ) -> Result<FusedSearchResult> {
        let mut body = Map::new();
        body.insert("collection".into(), json!(collection));
        body.insert("vectors".into(), json!(vectors));
        body.insert("top_k".into(), json!(top_k));
        body.insert("fusion".into(), json!(fusion));
        if let Some(filter) = filter {
            body.insert("filter".into(), filter.clone());
        }
        let response = self.send(op(json!({"SearchMulti": Value::Object(body)})))?;
        response.decode("SearchMulti")
    }

    /// Search with a query built from examples: the sum of the `positive`
    /// examples minus the sum of the `negative` ones.
    ///
    /// With `exclude_examples`, the examples named by id are left out of the
    /// results — which is the server's default.
    pub fn vector_search_by_examples(
        &mut self,
        collection: &str,
        positive: &[VectorExample],
        negative: &[VectorExample],
        top_k: usize,
        exclude_examples: bool,
        options: &VectorSearchOptions,
    ) -> Result<VectorSearchResult> {
        let mut body = Map::new();
        body.insert("collection".into(), json!(collection));
        body.insert("positive".into(), json!(positive));
        body.insert("negative".into(), json!(negative));
        body.insert("top_k".into(), json!(top_k));
        body.insert("exclude_examples".into(), json!(exclude_examples));
        options.write(&mut body);
        let response = self.send(op(json!({"SearchByExamples": Value::Object(body)})))?;
        response.decode("SearchByExamples")
    }

    /// Delete every vector whose metadata matches `filter`, as one atomic
    /// batch, returning how many went. An empty filter is refused by the
    /// server: emptying a collection is [`Client::vector_drop_collection`]'s
    /// job.
    pub fn vector_delete_by_filter(&mut self, collection: &str, filter: &Value) -> Result<i64> {
        let response = self.send(op(json!({"DeleteByFilter": {
            "collection": collection,
            "filter": filter,
        }})))?;
        Ok(response.decode::<DeletedCount>("DeleteByFilter")?.deleted)
    }

    /// Merge `set` into the metadata of every vector matching `filter`, as one
    /// atomic batch, returning how many changed. Vectors are never touched.
    pub fn vector_update_by_filter(
        &mut self,
        collection: &str,
        filter: &Value,
        set: &Map<String, Value>,
    ) -> Result<i64> {
        #[derive(Deserialize)]
        struct Updated {
            #[serde(default)]
            updated: i64,
        }
        let response = self.send(op(json!({"UpdateByFilter": {
            "collection": collection,
            "filter": filter,
            "set": set,
        }})))?;
        Ok(response.decode::<Updated>("UpdateByFilter")?.updated)
    }

    /// How many vectors a collection holds — or, with a filter, how many match
    /// it. A count, not an estimate.
    pub fn vector_count(&mut self, collection: &str, filter: Option<&Value>) -> Result<i64> {
        #[derive(Deserialize)]
        struct Count {
            #[serde(default)]
            count: i64,
        }
        let mut body = Map::new();
        body.insert("collection".into(), json!(collection));
        if let Some(filter) = filter {
            body.insert("filter".into(), filter.clone());
        }
        let response = self.send(op(json!({"Count": Value::Object(body)})))?;
        Ok(response.decode::<Count>("Count")?.count)
    }

    /// The partitions of a collection: each distinct value of `field` with its
    /// vector count. `field` is `_namespace` or `_tenant`; `None` leaves it to
    /// the server, which uses `_namespace`.
    pub fn vector_list_partitions(
        &mut self,
        collection: &str,
        field: Option<&str>,
    ) -> Result<VectorPartitions> {
        let mut body = Map::new();
        body.insert("collection".into(), json!(collection));
        if let Some(field) = field {
            body.insert("field".into(), json!(field));
        }
        let response = self.send(op(json!({"ListPartitions": Value::Object(body)})))?;
        response.decode("ListPartitions")
    }

    /// Rename a collection, moving every vector with it in one atomic batch,
    /// and return how many moved. Refused when `to` already exists.
    pub fn vector_rename_collection(&mut self, collection: &str, to: &str) -> Result<i64> {
        #[derive(Deserialize)]
        struct Moved {
            #[serde(default)]
            moved: i64,
        }
        let response = self.send(op(json!({"RenameCollection": {
            "collection": collection,
            "to": to,
        }})))?;
        Ok(response.decode::<Moved>("RenameCollection")?.moved)
    }

    /// Copy a collection — configuration and every vector — to a new name, and
    /// return how many vectors were copied. Refused when `to` exists.
    pub fn vector_copy_collection(&mut self, collection: &str, to: &str) -> Result<i64> {
        #[derive(Deserialize)]
        struct Copied {
            #[serde(default)]
            copied: i64,
        }
        let response = self.send(op(json!({"CopyCollection": {
            "collection": collection,
            "to": to,
        }})))?;
        Ok(response.decode::<Copied>("CopyCollection")?.copied)
    }

    /// Rebuild the collection's search index from its stored vectors and
    /// persist it, clearing every tombstone.
    pub fn vector_rebuild_index(&mut self, collection: &str) -> Result<VectorIndexRebuild> {
        let response = self.send(op(json!({"RebuildIndex": {"collection": collection}})))?;
        response.decode("RebuildIndex")
    }

    /// The shape of the collection's search index. Loads or builds the index
    /// as a search would.
    pub fn vector_index_stats(&mut self, collection: &str) -> Result<VectorIndexStats> {
        let response = self.send(op(json!({"IndexStats": {"collection": collection}})))?;
        response.decode("IndexStats")
    }

    /// Measure the index's recall: sampled stored vectors are each searched
    /// through the index and by an exact scan, and the overlap is reported.
    /// Costs one exact scan per sample.
    pub fn vector_measure_recall(
        &mut self,
        collection: &str,
        options: &RecallOptions,
    ) -> Result<VectorRecall> {
        let mut body = Map::new();
        body.insert("collection".into(), json!(collection));
        if let Some(sample) = options.sample {
            body.insert("sample".into(), json!(sample));
        }
        if let Some(top_k) = options.top_k {
            body.insert("top_k".into(), json!(top_k));
        }
        if let Some(ef_search) = options.ef_search {
            body.insert("ef_search".into(), json!(ef_search));
        }
        let response = self.send(op(json!({"MeasureRecall": Value::Object(body)})))?;
        response.decode("MeasureRecall")
    }

    /// Change a collection's configuration, and return it as it now stands.
    pub fn vector_set_collection_config(
        &mut self,
        collection: &str,
        change: &VectorConfigChange,
    ) -> Result<VectorConfig> {
        if *change == VectorConfigChange::default() {
            return Err(Error::invalid(
                "a configuration change must set at least one of `normalize`, `m`, `ef_construction` or `multi_tenant`",
            ));
        }
        let mut body = Map::new();
        body.insert("collection".into(), json!(collection));
        if let Some(normalize) = change.normalize {
            body.insert("normalize".into(), json!(normalize));
        }
        if let Some(m) = change.m {
            body.insert("m".into(), json!(m));
        }
        if let Some(ef_construction) = change.ef_construction {
            body.insert("ef_construction".into(), json!(ef_construction));
        }
        if let Some(multi_tenant) = change.multi_tenant {
            body.insert("multi_tenant".into(), json!(multi_tenant));
        }
        let response = self.send(op(json!({"SetCollectionConfig": Value::Object(body)})))?;
        response.decode("SetCollectionConfig")
    }
}

#[derive(Deserialize)]
struct Upserted {
    #[serde(default)]
    upserted: i64,
}

#[derive(Deserialize)]
struct DeletedCount {
    #[serde(default)]
    deleted: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_and_quantizations_use_the_servers_spelling() {
        assert_eq!(
            serde_json::to_value(VectorMetric::Cosine).unwrap(),
            "cosine"
        );
        assert_eq!(serde_json::to_value(VectorMetric::Dot).unwrap(), "dot");
        assert_eq!(serde_json::to_value(VectorMetric::L2).unwrap(), "l2");
        assert_eq!(
            serde_json::to_value(VectorQuantization::None).unwrap(),
            "none"
        );
        assert_eq!(
            serde_json::to_value(VectorQuantization::Int8).unwrap(),
            "int8"
        );
    }

    #[test]
    fn metrics_round_trip_from_the_wire() {
        let metric: VectorMetric = serde_json::from_str("\"l2\"").unwrap();
        assert_eq!(metric, VectorMetric::L2);
        assert_eq!(VectorMetric::default(), VectorMetric::Cosine);
        assert_eq!(VectorQuantization::default(), VectorQuantization::None);
    }

    #[test]
    fn a_hit_decodes_with_its_metadata() {
        let hit: VectorHit =
            serde_json::from_value(json!({"id": "a", "score": -0.02, "metadata": {"kind": "doc"}}))
                .unwrap();
        assert_eq!(hit.id, "a");
        assert_eq!(hit.metadata["kind"], "doc");
    }
}
