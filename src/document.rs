//! Documents: collections, filters, updates, indexes and aggregation.

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
