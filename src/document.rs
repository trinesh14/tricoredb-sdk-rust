//! Documents: collections, filters, updates, indexes and aggregation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::client::Client;
use crate::error::{Error, Result};
use crate::response::Response;

/// One JSON document as the server stores it. Every stored document carries an
/// `_id`, whether the caller chose it or the server did.
pub type Document = Map<String, Value>;

/// A query predicate.
///
/// Field paths use dot notation (`"address.city"`) into nested objects. A
/// document that lacks the path never matches — including for
/// [`DocumentFilter::ne`].
///
/// This is not MongoDB's query language: there is no `or`, no `not` and no
/// regular expression, because the server implements none of them.
///
/// ```
/// use tricoredb::DocumentFilter;
/// let cheap_widgets = DocumentFilter::and([
///     DocumentFilter::eq("kind", "widget"),
///     DocumentFilter::lt("price", 10),
/// ]);
/// # let _ = cheap_widgets;
/// ```
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct DocumentFilter(Value);

fn field_filter(variant: &str, field: &str, value: impl Into<Value>) -> DocumentFilter {
    DocumentFilter(json!({variant: {"field": field, "value": value.into()}}))
}

impl DocumentFilter {
    /// Match every document. Say this on purpose: there is no filter that means
    /// it by accident.
    pub fn all() -> Self {
        DocumentFilter(Value::String("All".into()))
    }

    /// The field equals this value.
    pub fn eq(field: &str, value: impl Into<Value>) -> Self {
        field_filter("Eq", field, value)
    }

    /// The field exists and does not equal this value.
    pub fn ne(field: &str, value: impl Into<Value>) -> Self {
        field_filter("Ne", field, value)
    }

    /// The field is greater than this value.
    pub fn gt(field: &str, value: impl Into<Value>) -> Self {
        field_filter("Gt", field, value)
    }

    /// The field is greater than or equal to this value.
    pub fn gte(field: &str, value: impl Into<Value>) -> Self {
        field_filter("Gte", field, value)
    }

    /// The field is less than this value.
    pub fn lt(field: &str, value: impl Into<Value>) -> Self {
        field_filter("Lt", field, value)
    }

    /// The field is less than or equal to this value.
    pub fn lte(field: &str, value: impl Into<Value>) -> Self {
        field_filter("Lte", field, value)
    }

    /// A string field holds this substring, or an array field holds this
    /// element.
    pub fn contains(field: &str, value: impl Into<Value>) -> Self {
        field_filter("Contains", field, value)
    }

    /// The field equals one of these values.
    pub fn is_in<V: Into<Value>>(field: &str, values: impl IntoIterator<Item = V>) -> Self {
        let values: Vec<Value> = values.into_iter().map(Into::into).collect();
        DocumentFilter(json!({"In": {"field": field, "values": values}}))
    }

    /// Every one of these filters holds. With none given it matches
    /// everything, which is what the server does with an empty `And`.
    pub fn and(filters: impl IntoIterator<Item = DocumentFilter>) -> Self {
        let filters: Vec<Value> = filters.into_iter().map(|f| f.0).collect();
        DocumentFilter(json!({"And": filters}))
    }
}

/// Field changes applied to one document: `set` overwrites the value at a dot
/// path, `inc` adds a number to it. Every `set` happens before every `inc`.
///
/// `inc` never converts: incrementing a field that holds a string, a boolean,
/// null, an array or an object is an error on the server. A missing field
/// increments from zero, and a negative delta is how you subtract.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DocumentUpdate {
    /// Dot path to its new value.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub set: Map<String, Value>,
    /// Dot path to a numeric delta.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub inc: Map<String, Value>,
}

impl DocumentUpdate {
    /// An update that changes nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Overwrite the value at a dot path.
    pub fn set(mut self, path: &str, value: impl Into<Value>) -> Self {
        self.set.insert(path.to_string(), value.into());
        self
    }

    /// Add a number to the value at a dot path.
    pub fn inc(mut self, path: &str, delta: impl Into<Value>) -> Self {
        self.inc.insert(path.to_string(), delta.into());
        self
    }

    fn is_empty(&self) -> bool {
        self.set.is_empty() && self.inc.is_empty()
    }
}

/// How [`AggregateStage::group`] derives a group key.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct GroupKey(Value);

impl GroupKey {
    /// Group by the value at a dot path. A document that lacks the path groups
    /// under null rather than being dropped.
    pub fn field(path: &str) -> Self {
        GroupKey(json!({"Field": path}))
    }

    /// Put the whole collection in one group under this constant — how a
    /// collection-wide total is expressed.
    pub fn constant(value: impl Into<Value>) -> Self {
        GroupKey(json!({"Constant": value.into()}))
    }
}

/// One reduction inside a group stage.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct AccumulatorOp(Value);

impl AccumulatorOp {
    /// Total the numeric values at a field. Documents where it is missing or
    /// not a number are ignored, so an absent field never contributes a zero.
    pub fn sum(field: &str) -> Self {
        AccumulatorOp(json!({"Sum": field}))
    }

    /// Average the numeric values at a field.
    pub fn avg(field: &str) -> Self {
        AccumulatorOp(json!({"Avg": field}))
    }

    /// The smallest value at a field.
    pub fn min(field: &str) -> Self {
        AccumulatorOp(json!({"Min": field}))
    }

    /// The largest value at a field.
    pub fn max(field: &str) -> Self {
        AccumulatorOp(json!({"Max": field}))
    }

    /// Count documents. It takes no field because it counts documents, not
    /// values.
    pub fn count() -> Self {
        AccumulatorOp(Value::String("Count".into()))
    }
}

/// An accumulator and the field it writes into the grouped document.
///
/// `output` must not be `_id` — that holds the group key — and must not repeat
/// within one group stage.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroupAccumulator {
    /// The field the result is written to.
    pub output: String,
    /// The reduction to apply.
    pub op: AccumulatorOp,
}

impl GroupAccumulator {
    /// An accumulator writing into `output`.
    pub fn new(output: &str, op: AccumulatorOp) -> Self {
        Self {
            output: output.to_string(),
            op,
        }
    }
}

/// One sort key. After a group stage the addressable fields are `_id` and the
/// accumulator outputs, not the original document's fields.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SortKey {
    /// The field to sort on.
    pub field: String,
    /// Largest first when true.
    pub descending: bool,
}

impl SortKey {
    /// Smallest first.
    pub fn ascending(field: &str) -> Self {
        Self {
            field: field.to_string(),
            descending: false,
        }
    }

    /// Largest first.
    pub fn descending(field: &str) -> Self {
        Self {
            field: field.to_string(),
            descending: true,
        }
    }
}

/// One stage of an aggregation pipeline.
///
/// Stages apply in the order given, and the order is meaning rather than
/// style: a match before a group filters documents, after it filters groups.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct AggregateStage(Value);

impl AggregateStage {
    /// Filter with the same matcher [`Client::document_find`] uses.
    pub fn filter(filter: DocumentFilter) -> Self {
        AggregateStage(json!({"Match": filter}))
    }

    /// Group by a key and apply accumulators.
    pub fn group(by: GroupKey, accumulators: impl IntoIterator<Item = GroupAccumulator>) -> Self {
        let accumulators: Vec<GroupAccumulator> = accumulators.into_iter().collect();
        AggregateStage(json!({"Group": {"by": by, "accumulators": accumulators}}))
    }

    /// Order the documents.
    pub fn sort(keys: impl IntoIterator<Item = SortKey>) -> Self {
        let keys: Vec<SortKey> = keys.into_iter().collect();
        AggregateStage(json!({"Sort": keys}))
    }

    /// Drop the first `n` documents.
    pub fn skip(n: usize) -> Self {
        AggregateStage(json!({"Skip": n}))
    }

    /// Keep at most `n` documents.
    pub fn limit(n: usize) -> Self {
        AggregateStage(json!({"Limit": n}))
    }

    /// Keep (`include` true) or drop (`include` false) these top-level fields.
    /// Nested projection is refused by the server.
    pub fn project(fields: impl IntoIterator<Item = impl Into<String>>, include: bool) -> Self {
        let fields: Vec<String> = fields.into_iter().map(Into::into).collect();
        AggregateStage(json!({"Project": {"fields": fields, "include": include}}))
    }

    /// Replace the documents with one that holds the input count under `field`.
    pub fn count(field: &str) -> Self {
        AggregateStage(json!({"Count": {"field": field}}))
    }
}

/// One secondary index on a collection.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DocumentIndex {
    /// The index's name.
    #[serde(rename = "index_name")]
    pub name: String,
    /// The field it indexes.
    #[serde(default)]
    pub field: String,
    /// Whether it enforces uniqueness.
    #[serde(default)]
    pub unique: bool,
}

/// What [`Client::document_analyze`] measured.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DocumentStats {
    /// The collection that was analyzed.
    #[serde(rename = "analyzed")]
    pub collection: String,
    /// How many documents it holds.
    #[serde(default)]
    pub document_count: i64,
    /// How many fields carry a secondary index.
    #[serde(default)]
    pub indexed_fields: i64,
}

/// What [`Client::document_update_many`] changed.
///
/// `matched` counts the documents the filter selected; `modified` counts those
/// whose contents actually changed, so rewriting a document to the value it
/// already held is matched but not modified.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct UpdateCounts {
    /// How many documents the filter selected.
    #[serde(default)]
    pub matched: i64,
    /// How many of those actually changed.
    #[serde(default)]
    pub modified: i64,
}

/// Whether a by-id write hands back the document it touched, and which
/// version of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub enum ReturnDocument {
    /// Return no document. The default.
    #[default]
    None,
    /// The document as it was before the write. Absent when there was none —
    /// an upsert that inserted.
    Before,
    /// The document as it is after the write.
    After,
}

/// How [`Client::document_replace`] behaves. The default refuses a missing
/// document, returns no document and checks no version.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplaceOptions {
    /// Create the document when it does not exist, rather than refuse.
    pub upsert: bool,
    /// Which version of the document to hand back.
    pub return_document: ReturnDocument,
    /// Apply only if the document's `_version` is still this. Needs a
    /// versioned collection; `0` means "must not exist yet".
    pub expected_version: Option<u64>,
}

/// What [`Client::document_replace`] did.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ReplaceResult {
    /// The document's id.
    #[serde(default)]
    pub id: String,
    /// Whether a document now holds the new body.
    #[serde(default)]
    pub replaced: bool,
    /// Whether the document was created by an upsert.
    #[serde(default)]
    pub inserted: bool,
    /// The document asked for with [`ReplaceOptions::return_document`], when
    /// there is one.
    #[serde(default)]
    pub document: Option<Document>,
}

/// What [`Client::document_purge_expired`] removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct PurgeCounts {
    /// How many expired documents were deleted.
    #[serde(default)]
    pub purged: i64,
    /// Whether expired documents remain past this call's batch.
    #[serde(default)]
    pub more: bool,
}

/// What [`Client::document_explain`] should run. The default explains an
/// unbounded, unsorted find.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExplainOptions {
    /// Cap the documents returned.
    pub limit: Option<usize>,
    /// Order the documents.
    pub sort: Vec<SortKey>,
    /// Drop this many documents from the front.
    pub skip: Option<usize>,
}

/// How a find ran, as [`Client::document_explain`] reports it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DocumentPlan {
    /// The plan's name: `FindScan`, or an index lookup.
    #[serde(default)]
    pub plan: String,
    /// The index that answered, when one did.
    #[serde(default)]
    pub index: Option<String>,
    /// Whether an index answered.
    #[serde(default)]
    pub index_used: bool,
    /// Why the collection was scanned, when it was.
    #[serde(default)]
    pub scan_reason: Option<String>,
    /// Where a scan read from.
    #[serde(default)]
    pub scan_source: Option<String>,
    /// The planner's estimate of the matching documents.
    #[serde(default)]
    pub estimated_rows: f64,
    /// The planner's cost for the plan it chose.
    #[serde(default)]
    pub estimated_cost: f64,
    /// How many documents were read.
    #[serde(default)]
    pub rows_examined: i64,
    /// How many documents the find returned.
    #[serde(default)]
    pub rows_returned: i64,
    /// How long it took, in microseconds.
    #[serde(default)]
    pub elapsed_us: i64,
}

/// What [`Client::document_rebuild_index`] rebuilt.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IndexRebuild {
    /// The index that was rebuilt.
    #[serde(rename = "rebuilt")]
    pub index_name: String,
    /// How many documents were read.
    #[serde(default)]
    pub documents: i64,
    /// How many index entries were written.
    #[serde(default)]
    pub entries: i64,
}

/// What [`Client::document_commit_transaction`] landed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct DocumentCommit {
    /// Whether the transaction committed.
    #[serde(default)]
    pub committed: bool,
    /// How many writes landed with it.
    #[serde(default)]
    pub writes: i64,
}

/// A JSON type a [`FieldRule`] can demand. `Integer` is a number with no
/// fractional part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    /// A string.
    String,
    /// Any number.
    Number,
    /// A number with no fractional part.
    Integer,
    /// `true` or `false`.
    Boolean,
    /// An object.
    Object,
    /// An array.
    Array,
    /// `null`.
    Null,
}

/// What one field may hold. Every rule given must pass.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldRule {
    /// The JSON type the value must have.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub field_type: Option<FieldType>,
    /// The value must equal one of these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub one_of: Vec<Value>,
    /// The smallest number allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    /// The largest number allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
    /// The fewest characters of a string, or elements of an array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,
    /// The most characters of a string, or elements of an array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,
    /// A regular expression a string must contain a match for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

impl FieldRule {
    /// A rule demanding one JSON type.
    pub fn of_type(field_type: FieldType) -> Self {
        Self {
            field_type: Some(field_type),
            ..Self::default()
        }
    }
}

/// A document shape: required paths, per-path rules, and whether top-level
/// fields the schema does not name are allowed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentSchema {
    /// Paths every document must have. A stored `null` counts as present.
    #[serde(default)]
    pub required: Vec<String>,
    /// Path to rule, checked when the path is present.
    #[serde(default)]
    pub fields: BTreeMap<String, FieldRule>,
    /// Whether a top-level field the schema does not name is allowed. `_id`
    /// and `_version` always are.
    #[serde(default = "yes")]
    pub additional_fields: bool,
}

impl Default for DocumentSchema {
    fn default() -> Self {
        Self {
            required: Vec::new(),
            fields: BTreeMap::new(),
            additional_fields: true,
        }
    }
}

fn yes() -> bool {
    true
}

/// Rules a collection applies to every document written into it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CollectionOptions {
    /// Every document written must satisfy this; a violation refuses the
    /// write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<DocumentSchema>,
    /// Path to the value a newly created document is given when it lacks the
    /// path.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub defaults: Map<String, Value>,
    /// Keep a `_version` counter on every document, which is what lets a write
    /// name an expected version.
    #[serde(default)]
    pub versioned: bool,
}

/// One operation of [`Client::document_bulk_write`].
///
/// ```
/// use serde_json::json;
/// use tricoredb::{BulkOp, DocumentUpdate};
/// let ops = [
///     BulkOp::insert_with_id("a", json!({"n": 1})),
///     BulkOp::update_one("a", DocumentUpdate::new().inc("n", 1)),
///     BulkOp::delete("b"),
/// ];
/// # let _ = ops;
/// ```
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum BulkOp {
    /// Insert a document, under `id` when one is given.
    Insert {
        /// The id to insert under. `None` uses the document's `_id`, or
        /// generates one.
        id: Option<String>,
        /// The document, which must be a JSON object.
        document: Value,
    },
    /// Apply an update to one document by id.
    UpdateOne {
        /// The document's id.
        id: String,
        /// The changes to make.
        update: DocumentUpdate,
        /// Create the document from the update when it does not exist.
        upsert: bool,
        /// Apply only if the document's `_version` is still this.
        expected_version: Option<u64>,
    },
    /// Swap one document's whole body.
    Replace {
        /// The document's id.
        id: String,
        /// The new body, which must be a JSON object.
        document: Value,
        /// Create the document when it does not exist.
        upsert: bool,
        /// Apply only if the document's `_version` is still this.
        expected_version: Option<u64>,
    },
    /// Delete one document by id.
    Delete {
        /// The document's id.
        id: String,
        /// Apply only if the document's `_version` is still this.
        expected_version: Option<u64>,
    },
}

impl BulkOp {
    /// Insert a document under its own `_id`, or a generated one.
    pub fn insert(document: impl Into<Value>) -> Self {
        BulkOp::Insert {
            id: None,
            document: document.into(),
        }
    }

    /// Insert a document under an id you choose.
    pub fn insert_with_id(id: &str, document: impl Into<Value>) -> Self {
        BulkOp::Insert {
            id: Some(id.to_string()),
            document: document.into(),
        }
    }

    /// Apply an update to one document by id.
    pub fn update_one(id: &str, update: DocumentUpdate) -> Self {
        BulkOp::UpdateOne {
            id: id.to_string(),
            update,
            upsert: false,
            expected_version: None,
        }
    }

    /// Swap one document's whole body.
    pub fn replace(id: &str, document: impl Into<Value>) -> Self {
        BulkOp::Replace {
            id: id.to_string(),
            document: document.into(),
            upsert: false,
            expected_version: None,
        }
    }

    /// Delete one document by id.
    pub fn delete(id: &str) -> Self {
        BulkOp::Delete {
            id: id.to_string(),
            expected_version: None,
        }
    }

    /// Create the document when it does not exist. Changes nothing on an
    /// insert or a delete, which have no such choice.
    pub fn upsert(mut self) -> Self {
        if let BulkOp::UpdateOne { upsert, .. } | BulkOp::Replace { upsert, .. } = &mut self {
            *upsert = true;
        }
        self
    }

    /// Apply only if the document's `_version` is still `version`, compared
    /// with the version it had before the batch. Changes nothing on an insert.
    pub fn expected_version(mut self, version: u64) -> Self {
        match &mut self {
            BulkOp::UpdateOne {
                expected_version, ..
            }
            | BulkOp::Replace {
                expected_version, ..
            }
            | BulkOp::Delete {
                expected_version, ..
            } => *expected_version = Some(version),
            BulkOp::Insert { .. } => {}
        }
        self
    }
}

/// What [`Client::document_bulk_write`] applied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct BulkWriteResult {
    /// Documents inserted.
    #[serde(default)]
    pub inserted: i64,
    /// Documents updated.
    #[serde(default)]
    pub updated: i64,
    /// Documents created by an upsert.
    #[serde(default)]
    pub upserted: i64,
    /// Documents replaced.
    #[serde(default)]
    pub replaced: i64,
    /// Documents deleted.
    #[serde(default)]
    pub deleted: i64,
    /// The ids of the inserted documents, in the order their operations were
    /// given.
    #[serde(default)]
    pub inserted_ids: Vec<String>,
}

fn op(body: Value) -> Value {
    json!({"Document": body})
}

impl Client {
    /// Create a collection.
    pub fn document_create_collection(&mut self, collection: &str) -> Result<()> {
        self.send(op(json!({"CreateCollection": {"collection": collection}})))
            .map(|_| ())
    }

    /// Drop a collection with its documents and indexes. Dropping one that does
    /// not exist is an error.
    pub fn document_drop_collection(&mut self, collection: &str) -> Result<()> {
        self.send(op(json!({"DropCollection": {"collection": collection}})))
            .map(|_| ())
    }

    /// Name every collection in the database.
    pub fn document_list_collections(&mut self) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Collections {
            #[serde(default)]
            collections: Vec<String>,
        }
        let response = self.send(op(Value::String("ListCollections".into())))?;
        let out: Collections = response.decode("ListCollections")?;
        Ok(out.collections)
    }

    /// Insert a document under an id the server generates, and return that id.
    pub fn document_insert(
        &mut self,
        collection: &str,
        document: &impl Serialize,
    ) -> Result<String> {
        self.document_insert_inner(collection, Value::Null, document)
    }

    /// Insert a document under an id you choose. Inserting over an existing id
    /// is an error rather than an overwrite — see [`Client::document_upsert_one`].
    pub fn document_insert_with_id(
        &mut self,
        collection: &str,
        id: &str,
        document: &impl Serialize,
    ) -> Result<String> {
        self.document_insert_inner(collection, json!(id), document)
    }

    fn document_insert_inner(
        &mut self,
        collection: &str,
        id: Value,
        document: &impl Serialize,
    ) -> Result<String> {
        #[derive(Deserialize)]
        struct Inserted {
            #[serde(default)]
            id: String,
        }
        let document = serde_json::to_value(document)
            .map_err(|e| Error::invalid(format!("the document cannot be encoded as JSON: {e}")))?;
        if !document.is_object() {
            return Err(Error::invalid(
                "a document must encode to a JSON object, not a scalar or an array",
            ));
        }
        let response = self.send(op(json!({"Insert": {
            "collection": collection,
            "id": id,
            "document": document,
        }})))?;
        let out: Inserted = response.decode("Insert")?;
        if out.id.is_empty() {
            return Err(Error::protocol("Insert answered with no document id"));
        }
        Ok(out.id)
    }

    /// Fetch one document by id. `None` when there is no such document, which
    /// is how an absent document is told apart from a stored empty one.
    pub fn document_get(&mut self, collection: &str, id: &str) -> Result<Option<Document>> {
        let response = self.send(op(json!({"Get": {"collection": collection, "id": id}})))?;
        Ok(documents_of(&response, "Get")?.into_iter().next())
    }

    /// Every document matching a filter.
    pub fn document_find(
        &mut self,
        collection: &str,
        filter: &DocumentFilter,
    ) -> Result<Vec<Document>> {
        self.document_find_inner(collection, filter, Value::Null)
    }

    /// At most `limit` documents matching a filter.
    pub fn document_find_limit(
        &mut self,
        collection: &str,
        filter: &DocumentFilter,
        limit: usize,
    ) -> Result<Vec<Document>> {
        let limit = if limit == 0 {
            Value::Null
        } else {
            json!(limit)
        };
        self.document_find_inner(collection, filter, limit)
    }

    fn document_find_inner(
        &mut self,
        collection: &str,
        filter: &DocumentFilter,
        limit: Value,
    ) -> Result<Vec<Document>> {
        let response = self.send(op(json!({"Find": {
            "collection": collection,
            "filter": filter,
            "limit": limit,
        }})))?;
        documents_of(&response, "Find")
    }

    /// Set fields on an existing document. A missing id is an error: this is
    /// not an upsert, and `_id` cannot be set.
    pub fn document_update(
        &mut self,
        collection: &str,
        id: &str,
        set: &Map<String, Value>,
    ) -> Result<()> {
        self.send(op(json!({"Update": {
            "collection": collection,
            "id": id,
            "set": set,
        }})))
        .map(|_| ())
    }

    /// Apply an update to one document by id. A missing id is an error; use
    /// [`Client::document_upsert_one`] to create instead.
    pub fn document_update_one(
        &mut self,
        collection: &str,
        id: &str,
        update: &DocumentUpdate,
    ) -> Result<()> {
        self.update_one(collection, id, update, false).map(|_| ())
    }

    /// Apply an update to one document by id, creating it from the update when
    /// it does not exist. Returns whether it was created.
    pub fn document_upsert_one(
        &mut self,
        collection: &str,
        id: &str,
        update: &DocumentUpdate,
    ) -> Result<bool> {
        let out = self.update_one(collection, id, update, true)?;
        Ok(out.inserted)
    }

    fn update_one(
        &mut self,
        collection: &str,
        id: &str,
        update: &DocumentUpdate,
        upsert: bool,
    ) -> Result<UpdateOne> {
        if update.is_empty() {
            return Err(Error::invalid(
                "an update must set or increment at least one field",
            ));
        }
        let response = self.send(op(json!({"UpdateOne": {
            "collection": collection,
            "id": id,
            "update": update,
            "upsert": upsert,
        }})))?;
        response.decode("UpdateOne")
    }

    /// Apply an update to every document matching a filter. Never an upsert: a
    /// filter that matches nothing changes nothing, and that is not an error.
    pub fn document_update_many(
        &mut self,
        collection: &str,
        filter: &DocumentFilter,
        update: &DocumentUpdate,
    ) -> Result<UpdateCounts> {
        if update.is_empty() {
            return Err(Error::invalid(
                "an update must set or increment at least one field",
            ));
        }
        let response = self.send(op(json!({"UpdateMany": {
            "collection": collection,
            "filter": filter,
            "update": update,
        }})))?;
        response.decode("UpdateMany")
    }

    /// Delete one document by id. Deleting an absent document is not an error.
    pub fn document_delete(&mut self, collection: &str, id: &str) -> Result<()> {
        self.send(op(json!({"Delete": {"collection": collection, "id": id}})))
            .map(|_| ())
    }

    /// Build a secondary index on a top-level field. A unique index over a
    /// collection that already holds duplicates is refused before anything is
    /// written.
    pub fn document_create_index(
        &mut self,
        collection: &str,
        index_name: &str,
        field: &str,
        unique: bool,
    ) -> Result<()> {
        self.send(op(json!({"CreateIndex": {
            "collection": collection,
            "index_name": index_name,
            "field": field,
            "unique": unique,
        }})))
        .map(|_| ())
    }

    /// Remove a named index.
    pub fn document_drop_index(&mut self, collection: &str, index_name: &str) -> Result<()> {
        self.send(op(json!({"DropIndex": {
            "collection": collection,
            "index_name": index_name,
        }})))
        .map(|_| ())
    }

    /// List a collection's indexes.
    pub fn document_list_indexes(&mut self, collection: &str) -> Result<Vec<DocumentIndex>> {
        #[derive(Deserialize)]
        struct Indexes {
            #[serde(default)]
            indexes: Vec<DocumentIndex>,
        }
        let response = self.send(op(json!({"ListIndexes": {"collection": collection}})))?;
        let out: Indexes = response.decode("ListIndexes")?;
        Ok(out.indexes)
    }

    /// Collect the statistics the planner uses to choose between an index
    /// lookup and a scan.
    pub fn document_analyze(&mut self, collection: &str) -> Result<DocumentStats> {
        let response = self.send(op(json!({"Analyze": {"collection": collection}})))?;
        response.decode("Analyze")
    }

    /// Run an aggregation pipeline. An empty pipeline returns the collection
    /// unchanged.
    ///
    /// ```no_run
    /// # fn main() -> tricoredb::Result<()> {
    /// # let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
    /// use tricoredb::{AccumulatorOp, AggregateStage, DocumentFilter, GroupAccumulator, GroupKey};
    /// let totals = db.document_aggregate("orders", &[
    ///     AggregateStage::filter(DocumentFilter::eq("status", "paid")),
    ///     AggregateStage::group(
    ///         GroupKey::field("customer"),
    ///         [GroupAccumulator::new("total", AccumulatorOp::sum("amount"))],
    ///     ),
    /// ])?;
    /// # let _ = totals; Ok(()) }
    /// ```
    pub fn document_aggregate(
        &mut self,
        collection: &str,
        pipeline: &[AggregateStage],
    ) -> Result<Vec<Document>> {
        let response = self.send(op(json!({"Aggregate": {
            "collection": collection,
            "pipeline": pipeline,
        }})))?;
        documents_of(&response, "Aggregate")
    }

    /// Swap a document's whole body for a new one.
    ///
    /// [`Client::document_update`] merges, so a field left out survives; after
    /// a replace the stored body is exactly `document` plus `_id`. An `_id`
    /// inside `document` may agree with `id` or be absent, never disagree.
    pub fn document_replace(
        &mut self,
        collection: &str,
        id: &str,
        document: &impl Serialize,
        options: &ReplaceOptions,
    ) -> Result<ReplaceResult> {
        let mut body = Map::new();
        body.insert("collection".into(), json!(collection));
        body.insert("id".into(), json!(id));
        body.insert("document".into(), object_value(document)?);
        body.insert("upsert".into(), json!(options.upsert));
        if options.return_document != ReturnDocument::None {
            body.insert("return".into(), json!(options.return_document));
        }
        if let Some(version) = options.expected_version {
            body.insert("expected_version".into(), json!(version));
        }
        let response = self.send(op(json!({"Replace": Value::Object(body)})))?;
        response.decode("Replace")
    }

    /// Insert several documents as one atomic batch — every document lands or
    /// none does — and return their ids in order.
    ///
    /// Each document's id is its own `_id` when it carries one, and is
    /// generated otherwise.
    pub fn document_insert_many(
        &mut self,
        collection: &str,
        documents: &[impl Serialize],
    ) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Inserted {
            #[serde(default)]
            ids: Vec<String>,
        }
        let documents = documents
            .iter()
            .map(object_value)
            .collect::<Result<Vec<Value>>>()?;
        let response = self.send(op(json!({"InsertMany": {
            "collection": collection,
            "documents": documents,
        }})))?;
        let out: Inserted = response.decode("InsertMany")?;
        Ok(out.ids)
    }

    /// Delete every document matching a filter, returning how many went.
    pub fn document_delete_many(
        &mut self,
        collection: &str,
        filter: &DocumentFilter,
    ) -> Result<i64> {
        #[derive(Deserialize)]
        struct Deleted {
            #[serde(default)]
            deleted: i64,
        }
        let response = self.send(op(json!({"DeleteMany": {
            "collection": collection,
            "filter": filter,
        }})))?;
        let out: Deleted = response.decode("DeleteMany")?;
        Ok(out.deleted)
    }

    /// Move a collection, with its documents and indexes, to a new name.
    /// Refused when `to` already exists.
    pub fn document_rename_collection(&mut self, collection: &str, to: &str) -> Result<()> {
        self.send(op(json!({"RenameCollection": {
            "collection": collection,
            "to": to,
        }})))
        .map(|_| ())
    }

    /// Delete the documents a TTL index says have expired, now.
    pub fn document_purge_expired(&mut self, collection: &str) -> Result<PurgeCounts> {
        let response = self.send(op(json!({"PurgeExpired": {"collection": collection}})))?;
        response.decode("PurgeExpired")
    }

    /// Run a find and describe how it ran instead of returning its documents.
    pub fn document_explain(
        &mut self,
        collection: &str,
        filter: &DocumentFilter,
        options: &ExplainOptions,
    ) -> Result<DocumentPlan> {
        let mut body = Map::new();
        body.insert("collection".into(), json!(collection));
        body.insert("filter".into(), json!(filter));
        if let Some(limit) = options.limit {
            body.insert("limit".into(), json!(limit));
        }
        if !options.sort.is_empty() {
            body.insert("sort".into(), json!(options.sort));
        }
        if let Some(skip) = options.skip {
            body.insert("skip".into(), json!(skip));
        }
        let response = self.send(op(json!({"Explain": Value::Object(body)})))?;
        response.decode("Explain")
    }

    /// Open a transaction spanning requests, and return its id.
    ///
    /// Nothing joins the transaction by itself: pass the id to
    /// [`Client::set_request_txn`], and every later request reads and writes
    /// inside it, seeing its own writes and nobody else's since it began. With
    /// `serializable`, its reads are validated at commit as well.
    ///
    /// ```no_run
    /// # fn main() -> tricoredb::Result<()> {
    /// # let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
    /// use serde_json::json;
    /// let txn = db.document_begin_transaction(false)?;
    /// db.set_request_txn(Some(&txn));
    /// db.document_insert_with_id("orders", "o1", &json!({"total": 10}))?;
    /// db.document_commit_transaction(&txn)?;
    /// # Ok(()) }
    /// ```
    pub fn document_begin_transaction(&mut self, serializable: bool) -> Result<String> {
        #[derive(Deserialize)]
        struct Begun {
            #[serde(default)]
            txn: String,
        }
        let response = self.send_in(
            op(json!({"BeginTransaction": {"serializable": serializable}})),
            None,
        )?;
        let out: Begun = response.decode("BeginTransaction")?;
        if out.txn.is_empty() {
            return Err(Error::protocol(
                "BeginTransaction answered with no transaction id",
            ));
        }
        Ok(out.txn)
    }

    /// Commit a transaction: every write lands as one atomic batch, or a
    /// conflict is returned and nothing lands.
    ///
    /// Clears [`Client::set_request_txn`] when it names this transaction,
    /// whatever the outcome: a transaction that failed to commit is over.
    pub fn document_commit_transaction(&mut self, txn: &str) -> Result<DocumentCommit> {
        let outcome = self.send_in(op(json!("CommitTransaction")), Some(txn));
        self.forget_txn(txn);
        outcome?.decode("CommitTransaction")
    }

    /// Discard a transaction and everything written inside it.
    pub fn document_rollback_transaction(&mut self, txn: &str) -> Result<()> {
        let outcome = self.send_in(op(json!("RollbackTransaction")), Some(txn));
        self.forget_txn(txn);
        outcome.map(|_| ())
    }

    /// Rebuild one index from the stored documents, replacing its entries in
    /// one batch.
    pub fn document_rebuild_index(
        &mut self,
        collection: &str,
        index_name: &str,
    ) -> Result<IndexRebuild> {
        let response = self.send(op(json!({"RebuildIndex": {
            "collection": collection,
            "index_name": index_name,
        }})))?;
        response.decode("RebuildIndex")
    }

    /// Set a collection's options, replacing any it had, and return how many
    /// stored documents were checked against the new schema. Refused when an
    /// existing document would break it.
    pub fn document_set_collection_options(
        &mut self,
        collection: &str,
        options: &CollectionOptions,
    ) -> Result<i64> {
        #[derive(Deserialize)]
        struct Updated {
            #[serde(default)]
            documents_checked: i64,
        }
        let response = self.send(op(json!({"SetCollectionOptions": {
            "collection": collection,
            "options": options,
        }})))?;
        let out: Updated = response.decode("SetCollectionOptions")?;
        Ok(out.documents_checked)
    }

    /// Read a collection's options.
    pub fn document_get_collection_options(
        &mut self,
        collection: &str,
    ) -> Result<CollectionOptions> {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(default)]
            options: CollectionOptions,
        }
        let response = self.send(op(
            json!({"GetCollectionOptions": {"collection": collection}}),
        ))?;
        let out: Wire = response.decode("GetCollectionOptions")?;
        Ok(out.options)
    }

    /// Many inserts, updates, replaces and deletes on one collection, applied
    /// in order as one atomic batch: every operation lands or none does. A
    /// later operation sees the effect of an earlier one on the same id.
    pub fn document_bulk_write(
        &mut self,
        collection: &str,
        ops: &[BulkOp],
    ) -> Result<BulkWriteResult> {
        let response = self.send(op(json!({"BulkWrite": {
            "collection": collection,
            "ops": ops,
        }})))?;
        response.decode("BulkWrite")
    }
}

/// A document as the JSON object it must be.
fn object_value(document: &impl Serialize) -> Result<Value> {
    let document = serde_json::to_value(document)
        .map_err(|e| Error::invalid(format!("the document cannot be encoded as JSON: {e}")))?;
    if !document.is_object() {
        return Err(Error::invalid(
            "a document must encode to a JSON object, not a scalar or an array",
        ));
    }
    Ok(document)
}

#[derive(Deserialize)]
struct UpdateOne {
    #[serde(default)]
    #[allow(dead_code)]
    updated: bool,
    #[serde(default)]
    inserted: bool,
}

fn documents_of(response: &Response, what: &str) -> Result<Vec<Document>> {
    let data = response.expect("Documents", what)?;
    serde_json::from_value(data.clone())
        .map_err(|e| Error::protocol(format!("malformed Documents payload from {what}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(value: &impl Serialize) -> Value {
        serde_json::to_value(value).unwrap()
    }

    #[test]
    fn filters_are_externally_tagged() {
        assert_eq!(wire(&DocumentFilter::all()), json!("All"));
        assert_eq!(
            wire(&DocumentFilter::eq("kind", "widget")),
            json!({"Eq": {"field": "kind", "value": "widget"}})
        );
        assert_eq!(
            wire(&DocumentFilter::lt("price", 10)),
            json!({"Lt": {"field": "price", "value": 10}})
        );
        assert_eq!(
            wire(&DocumentFilter::is_in("id", ["a", "b"])),
            json!({"In": {"field": "id", "values": ["a", "b"]}})
        );
    }

    #[test]
    fn and_nests_its_children_in_order() {
        let filter = DocumentFilter::and([DocumentFilter::eq("a", 1), DocumentFilter::gt("b", 2)]);
        let value = wire(&filter);
        assert_eq!(value["And"][0]["Eq"]["field"], "a");
        assert_eq!(value["And"][1]["Gt"]["field"], "b");
    }

    #[test]
    fn an_update_omits_the_half_it_does_not_use() {
        let update = DocumentUpdate::new().set("name", "ada");
        assert_eq!(wire(&update), json!({"set": {"name": "ada"}}));
        let update = DocumentUpdate::new().inc("hits", 1);
        assert_eq!(wire(&update), json!({"inc": {"hits": 1}}));
        assert!(DocumentUpdate::new().is_empty());
    }

    #[test]
    fn pipeline_stages_keep_the_servers_own_names() {
        assert_eq!(
            wire(&AggregateStage::filter(DocumentFilter::all())),
            json!({"Match": "All"})
        );
        assert_eq!(wire(&AggregateStage::skip(2)), json!({"Skip": 2}));
        assert_eq!(wire(&AggregateStage::limit(5)), json!({"Limit": 5}));
        assert_eq!(
            wire(&AggregateStage::count("n")),
            json!({"Count": {"field": "n"}})
        );
        assert_eq!(
            wire(&AggregateStage::project(["a", "b"], true)),
            json!({"Project": {"fields": ["a", "b"], "include": true}})
        );
    }

    #[test]
    fn a_group_stage_carries_its_key_and_accumulators() {
        let stage = AggregateStage::group(
            GroupKey::field("customer"),
            [
                GroupAccumulator::new("total", AccumulatorOp::sum("amount")),
                GroupAccumulator::new("orders", AccumulatorOp::count()),
            ],
        );
        let value = wire(&stage);
        assert_eq!(value["Group"]["by"], json!({"Field": "customer"}));
        assert_eq!(value["Group"]["accumulators"][0]["output"], "total");
        assert_eq!(
            value["Group"]["accumulators"][0]["op"],
            json!({"Sum": "amount"})
        );
        assert_eq!(value["Group"]["accumulators"][1]["op"], json!("Count"));
    }

    #[test]
    fn sort_keys_say_which_way_they_go() {
        assert_eq!(
            wire(&AggregateStage::sort([SortKey::descending("total")])),
            json!({"Sort": [{"field": "total", "descending": true}]})
        );
        assert_eq!(
            wire(&SortKey::ascending("name")),
            json!({"field": "name", "descending": false})
        );
    }
}
