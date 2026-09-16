//! Vectors: fixed-dimension collections, similarity search and metadata
//! filters.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::client::Client;
use crate::error::Result;

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
