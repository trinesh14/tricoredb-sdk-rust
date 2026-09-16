//! The cache: values with TTLs and counters, plus lists, sets, hashes and
//! streams.
//!
//! Values are bytes throughout, never strings: the server stores opaque bytes,
//! and a client that spoke only text would corrupt anything that is not valid
//! UTF-8. The `_text` helpers exist for the common case and say what encoding
//! they impose.
//!
//! A key holds one type at a time. Using the wrong family on a key is an error,
//! never a silent conversion. A collection that becomes empty deletes its key,
//! and changing a collection never resets the key's TTL.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::client::Client;
use crate::error::{Error, Result};
use crate::wire::{byte_list, byte_lists, decode_byte_list, decode_byte_lists, decode_pair};

/// One live key in a namespace.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CacheKeyInfo {
    /// The key's name.
    pub key: String,
    /// Milliseconds until it expires, or `None` when it does not.
    #[serde(default)]
    pub ttl_ms: Option<i64>,
    /// How many bytes the value occupies.
    #[serde(default)]
    pub bytes: i64,
}

/// One `(field, value)` entry of a hash or a stream.
///
/// Fields are arbitrary bytes and need not be UTF-8, which is why this is a
/// pair rather than a map key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachePair {
    /// The field name, as bytes.
    pub field: Vec<u8>,
    /// The value, as bytes.
    pub value: Vec<u8>,
}

impl CachePair {
    /// A pair from any two byte strings.
    pub fn new(field: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        Self {
            field: field.into(),
            value: value.into(),
        }
    }

    /// Pairs from text, encoded UTF-8.
    pub fn from_text<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<CachePair> {
        entries
            .into_iter()
            .map(|(f, v)| CachePair::new(f.as_bytes(), v.as_bytes()))
            .collect()
    }

    /// The field as text, when it is valid UTF-8.
    pub fn field_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.field).ok()
    }

    /// The value as text, when it is valid UTF-8.
    pub fn value_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.value).ok()
    }

    fn to_wire(&self) -> Value {
        Value::Array(vec![byte_list(&self.field), byte_list(&self.value)])
    }
}

/// One entry of a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEntry {
    /// The entry's `<ms>-<seq>` id.
    pub id: String,
    /// Its fields, in the order the server returned them.
    pub fields: Vec<CachePair>,
}

impl StreamEntry {
    /// The fields as text. Wrong for binary payloads, where
    /// [`StreamEntry::fields`] stays authoritative.
    pub fn text(&self) -> HashMap<String, String> {
        self.fields
            .iter()
            .map(|p| {
                (
                    String::from_utf8_lossy(&p.field).into_owned(),
                    String::from_utf8_lossy(&p.value).into_owned(),
                )
            })
            .collect()
    }
}

/// `{"namespace": …, "key": …}`, which every keyed cache operation starts from.
fn ns_key(namespace: &str, key: &str) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("namespace".into(), Value::String(namespace.to_string()));
    body.insert("key".into(), Value::String(key.to_string()));
    body
}

fn ttl_value(ttl: Option<Duration>) -> Value {
    match ttl.filter(|t| !t.is_zero()) {
        Some(t) => json!(t.as_millis().min(i64::MAX as u128) as i64),
        None => Value::Null,
    }
}

fn op(variant: &str, body: Map<String, Value>) -> Value {
    json!({"Cache": {variant: Value::Object(body)}})
}

#[derive(Deserialize)]
struct Deleted {
    #[serde(default)]
    deleted: i64,
}

#[derive(Deserialize)]
struct Length {
    #[serde(default)]
    length: i64,
}

impl Client {
    /// A liveness check routed through the cache module.
    ///
    /// Unlike [`Client::ping`], which never reaches a module, this proves
    /// authentication, routing and dispatch all work.
    pub fn cache_ping(&mut self) -> Result<()> {
        self.send(json!({"Cache": "Ping"})).map(|_| ())
    }

    /// Read a value. `None` is a miss — which is how a miss is told apart from
    /// a stored empty value.
    pub fn cache_get(&mut self, namespace: &str, key: &str) -> Result<Option<Vec<u8>>> {
        self.cache_value(op("Get", ns_key(namespace, key)), "Get")
    }

    /// Store a value with no expiry.
    pub fn cache_set(&mut self, namespace: &str, key: &str, value: &[u8]) -> Result<()> {
        self.cache_set_ttl(namespace, key, value, None)
    }

    /// Store a value, expiring after `ttl`. `None` means no expiry.
    pub fn cache_set_ttl(
        &mut self,
        namespace: &str,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<()> {
        let mut body = ns_key(namespace, key);
        body.insert("value".into(), byte_list(value));
        body.insert("ttl_ms".into(), ttl_value(ttl));
        self.send(op("Set", body)).map(|_| ())
    }

    /// Store a value only if the key is absent, reporting whether it was
    /// written. The primitive behind a distributed lock.
    pub fn cache_set_nx(
        &mut self,
        namespace: &str,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<bool> {
        #[derive(Deserialize)]
        struct Set {
            #[serde(default)]
            set: bool,
        }
        let mut body = ns_key(namespace, key);
        body.insert("value".into(), byte_list(value));
        body.insert("ttl_ms".into(), ttl_value(ttl));
        let out: Set = self.cache_json(op("SetNx", body), "SetNx")?;
        Ok(out.set)
    }

    /// Delete a key, reporting whether it was there. Deleting an absent key is
    /// not an error.
    pub fn cache_delete(&mut self, namespace: &str, key: &str) -> Result<bool> {
        #[derive(Deserialize)]
        struct DeletedFlag {
            #[serde(default)]
            deleted: bool,
        }
        let out: DeletedFlag = self.cache_json(op("Delete", ns_key(namespace, key)), "Delete")?;
        Ok(out.deleted)
    }

    /// Whether the key is present and unexpired.
    pub fn cache_exists(&mut self, namespace: &str, key: &str) -> Result<bool> {
        #[derive(Deserialize)]
        struct Exists {
            #[serde(default)]
            exists: bool,
        }
        let out: Exists = self.cache_json(op("Exists", ns_key(namespace, key)), "Exists")?;
        Ok(out.exists)
    }

    /// How long until the key expires.
    ///
    /// `None` means the key is missing **or** has no expiry; use
    /// [`Client::cache_exists`] to tell those apart.
    pub fn cache_ttl(&mut self, namespace: &str, key: &str) -> Result<Option<Duration>> {
        #[derive(Deserialize)]
        struct Ttl {
            #[serde(default)]
            ttl_ms: Option<i64>,
        }
        let out: Ttl = self.cache_json(op("Ttl", ns_key(namespace, key)), "Ttl")?;
        Ok(out
            .ttl_ms
            .filter(|ms| *ms >= 0)
            .map(|ms| Duration::from_millis(ms as u64)))
    }

    /// Set or replace a key's TTL. `false` when the key does not exist.
    pub fn cache_expire(&mut self, namespace: &str, key: &str, ttl: Duration) -> Result<bool> {
        #[derive(Deserialize)]
        struct Updated {
            #[serde(default)]
            updated: bool,
        }
        let mut body = ns_key(namespace, key);
        body.insert(
            "ttl_ms".into(),
            json!(ttl.as_millis().min(i64::MAX as u128) as i64),
        );
        let out: Updated = self.cache_json(op("Expire", body), "Expire")?;
        Ok(out.updated)
    }

    /// Remove a key's TTL, making it permanent. `false` when it had none.
    pub fn cache_persist(&mut self, namespace: &str, key: &str) -> Result<bool> {
        #[derive(Deserialize)]
        struct Persisted {
            #[serde(default)]
            persisted: bool,
        }
        let out: Persisted = self.cache_json(op("Persist", ns_key(namespace, key)), "Persist")?;
        Ok(out.persisted)
    }

    /// Add to a counter and read the new value. A missing key starts at zero; a
    /// key holding something that is not a number is an error, not a
    /// conversion.
    pub fn cache_incr(&mut self, namespace: &str, key: &str, by: i64) -> Result<i64> {
        #[derive(Deserialize)]
        struct CounterValue {
            #[serde(default)]
            value: i64,
        }
        let mut body = ns_key(namespace, key);
        body.insert("by".into(), json!(by));
        let out: CounterValue = self.cache_json(op("Incr", body), "Incr")?;
        Ok(out.value)
    }

    /// Delete every key in a namespace, returning how many went.
    pub fn cache_clear_namespace(&mut self, namespace: &str) -> Result<i64> {
        #[derive(Deserialize)]
        struct Cleared {
            #[serde(default)]
            cleared: i64,
        }
        let mut body = Map::new();
        body.insert("namespace".into(), Value::String(namespace.to_string()));
        let out: Cleared = self.cache_json(op("ClearNamespace", body), "ClearNamespace")?;
        Ok(out.cleared)
    }

    /// List live keys in a namespace.
    ///
    /// `pattern` is a simple glob where `*` matches any run of characters
    /// (`"user:*"`, `"*:session"`); `None` lists everything. `limit` of `None`
    /// leaves the cap to the server.
    pub fn cache_keys(
        &mut self,
        namespace: &str,
        pattern: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<CacheKeyInfo>> {
        #[derive(Deserialize)]
        struct Keys {
            #[serde(default)]
            keys: Vec<CacheKeyInfo>,
        }
        let mut body = Map::new();
        body.insert("namespace".into(), Value::String(namespace.to_string()));
        body.insert("pattern".into(), pattern.map_or(Value::Null, |p| json!(p)));
        body.insert("limit".into(), limit.map_or(Value::Null, |l| json!(l)));
        let out: Keys = self.cache_json(op("Keys", body), "Keys")?;
        Ok(out.keys)
    }

    // -- lists ---------------------------------------------------------------

    /// Prepend elements, returning the list's new length.
    pub fn cache_lpush(
        &mut self,
        namespace: &str,
        key: &str,
        values: &[impl AsRef<[u8]>],
    ) -> Result<i64> {
        self.cache_push("LPush", namespace, key, values)
    }

    /// Append elements, returning the list's new length.
    pub fn cache_rpush(
        &mut self,
        namespace: &str,
        key: &str,
        values: &[impl AsRef<[u8]>],
    ) -> Result<i64> {
        self.cache_push("RPush", namespace, key, values)
    }

    fn cache_push(
        &mut self,
        variant: &str,
        namespace: &str,
        key: &str,
        values: &[impl AsRef<[u8]>],
    ) -> Result<i64> {
        let mut body = ns_key(namespace, key);
        body.insert("values".into(), byte_lists(values, "values")?);
        let out: Length = self.cache_json(op(variant, body), variant)?;
        Ok(out.length)
    }

    /// Remove and return the first element. `None` when the list is empty or
    /// missing.
    pub fn cache_lpop(&mut self, namespace: &str, key: &str) -> Result<Option<Vec<u8>>> {
        self.cache_value(op("LPop", ns_key(namespace, key)), "LPop")
    }

    /// Remove and return the last element.
    pub fn cache_rpop(&mut self, namespace: &str, key: &str) -> Result<Option<Vec<u8>>> {
        self.cache_value(op("RPop", ns_key(namespace, key)), "RPop")
    }

    /// Read an inclusive index range. Negative indices count from the end
    /// (`-1` is the last element) and out-of-range bounds are clamped.
    pub fn cache_lrange(
        &mut self,
        namespace: &str,
        key: &str,
        start: i64,
        stop: i64,
    ) -> Result<Vec<Vec<u8>>> {
        let mut body = ns_key(namespace, key);
        body.insert("start".into(), json!(start));
        body.insert("stop".into(), json!(stop));
        let value: Value = self.cache_json(op("LRange", body), "LRange")?;
        decode_byte_lists(value.get("values").unwrap_or(&Value::Null))
    }

    /// How many elements the list holds; zero when the key is missing.
    pub fn cache_llen(&mut self, namespace: &str, key: &str) -> Result<i64> {
        let out: Length = self.cache_json(op("LLen", ns_key(namespace, key)), "LLen")?;
        Ok(out.length)
    }

    /// One element by index; negative counts from the end. `None` when the
    /// index is out of range.
    pub fn cache_lindex(
        &mut self,
        namespace: &str,
        key: &str,
        index: i64,
    ) -> Result<Option<Vec<u8>>> {
        let mut body = ns_key(namespace, key);
        body.insert("index".into(), json!(index));
        self.cache_value(op("LIndex", body), "LIndex")
    }

    // -- sets ----------------------------------------------------------------

    /// Add members, returning how many were new.
    pub fn cache_sadd(
        &mut self,
        namespace: &str,
        key: &str,
        members: &[impl AsRef<[u8]>],
    ) -> Result<i64> {
        #[derive(Deserialize)]
        struct Added {
            #[serde(default)]
            added: i64,
        }
        let mut body = ns_key(namespace, key);
        body.insert("members".into(), byte_lists(members, "members")?);
        let out: Added = self.cache_json(op("SAdd", body), "SAdd")?;
        Ok(out.added)
    }

    /// Remove members, returning how many were there.
    pub fn cache_srem(
        &mut self,
        namespace: &str,
        key: &str,
        members: &[impl AsRef<[u8]>],
    ) -> Result<i64> {
        #[derive(Deserialize)]
        struct Removed {
            #[serde(default)]
            removed: i64,
        }
        let mut body = ns_key(namespace, key);
        body.insert("members".into(), byte_lists(members, "members")?);
        let out: Removed = self.cache_json(op("SRem", body), "SRem")?;
        Ok(out.removed)
    }

    /// Whether a member is in the set.
    pub fn cache_sismember(&mut self, namespace: &str, key: &str, member: &[u8]) -> Result<bool> {
        #[derive(Deserialize)]
        struct IsMember {
            #[serde(default)]
            is_member: bool,
        }
        let mut body = ns_key(namespace, key);
        body.insert("member".into(), byte_list(member));
        let out: IsMember = self.cache_json(op("SIsMember", body), "SIsMember")?;
        Ok(out.is_member)
    }

    /// How many members the set holds; zero when the key is missing.
    pub fn cache_scard(&mut self, namespace: &str, key: &str) -> Result<i64> {
        #[derive(Deserialize)]
        struct Cardinality {
            #[serde(default)]
            cardinality: i64,
        }
        let out: Cardinality = self.cache_json(op("SCard", ns_key(namespace, key)), "SCard")?;
        Ok(out.cardinality)
    }

    /// Every member, in ascending byte order.
    pub fn cache_smembers(&mut self, namespace: &str, key: &str) -> Result<Vec<Vec<u8>>> {
        let value: Value = self.cache_json(op("SMembers", ns_key(namespace, key)), "SMembers")?;
        decode_byte_lists(value.get("members").unwrap_or(&Value::Null))
    }

    // -- hashes --------------------------------------------------------------

    /// Set fields, returning how many were created rather than overwritten.
    pub fn cache_hset(&mut self, namespace: &str, key: &str, entries: &[CachePair]) -> Result<i64> {
        #[derive(Deserialize)]
        struct Created {
            #[serde(default)]
            created: i64,
        }
        if entries.is_empty() {
            return Err(Error::invalid("`entries` must not be empty"));
        }
        let mut body = ns_key(namespace, key);
        body.insert(
            "entries".into(),
            Value::Array(entries.iter().map(CachePair::to_wire).collect()),
        );
        let out: Created = self.cache_json(op("HSet", body), "HSet")?;
        Ok(out.created)
    }

    /// Set text fields, encoded UTF-8.
    pub fn cache_hset_text<'a>(
        &mut self,
        namespace: &str,
        key: &str,
        entries: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Result<i64> {
        self.cache_hset(namespace, key, &CachePair::from_text(entries))
    }

    /// Read one field. `None` when the field or the key is absent.
    pub fn cache_hget(
        &mut self,
        namespace: &str,
        key: &str,
        field: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        let mut body = ns_key(namespace, key);
        body.insert("field".into(), byte_list(field));
        self.cache_value(op("HGet", body), "HGet")
    }

    /// Delete fields, returning how many were there.
    pub fn cache_hdel(
        &mut self,
        namespace: &str,
        key: &str,
        fields: &[impl AsRef<[u8]>],
    ) -> Result<i64> {
        let mut body = ns_key(namespace, key);
        body.insert("fields".into(), byte_lists(fields, "fields")?);
        let out: Deleted = self.cache_json(op("HDel", body), "HDel")?;
        Ok(out.deleted)
    }

    /// Every field and value, in ascending field order.
    pub fn cache_hgetall(&mut self, namespace: &str, key: &str) -> Result<Vec<CachePair>> {
        let value: Value = self.cache_json(op("HGetAll", ns_key(namespace, key)), "HGetAll")?;
        let entries = value
            .get("entries")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::protocol("malformed HGetAll payload: no entries".to_string()))?;
        entries
            .iter()
            .map(|entry| {
                let (field, value) = decode_pair(entry)?;
                Ok(CachePair { field, value })
            })
            .collect()
    }

    /// Whether a field exists in the hash.
    pub fn cache_hexists(&mut self, namespace: &str, key: &str, field: &[u8]) -> Result<bool> {
        #[derive(Deserialize)]
        struct Exists {
            #[serde(default)]
            exists: bool,
        }
        let mut body = ns_key(namespace, key);
        body.insert("field".into(), byte_list(field));
        let out: Exists = self.cache_json(op("HExists", body), "HExists")?;
        Ok(out.exists)
    }

    /// How many fields the hash holds; zero when the key is missing.
    pub fn cache_hlen(&mut self, namespace: &str, key: &str) -> Result<i64> {
        let out: Length = self.cache_json(op("HLen", ns_key(namespace, key)), "HLen")?;
        Ok(out.length)
    }

    // -- streams -------------------------------------------------------------

    /// Append an entry, returning the id it was given.
    ///
    /// `id` is `None` or `"*"` to generate one, `"<ms>"` or `"<ms>-*"` to fix
    /// the millisecond, or `"<ms>-<seq>"` for an exact id. Ids must increase.
    pub fn cache_xadd(
        &mut self,
        namespace: &str,
        key: &str,
        fields: &[CachePair],
        id: Option<&str>,
    ) -> Result<String> {
        #[derive(Deserialize)]
        struct EntryId {
            #[serde(default)]
            id: String,
        }
        if fields.is_empty() {
            return Err(Error::invalid("`fields` must not be empty"));
        }
        let mut body = ns_key(namespace, key);
        body.insert(
            "fields".into(),
            Value::Array(fields.iter().map(CachePair::to_wire).collect()),
        );
        body.insert(
            "id".into(),
            id.filter(|i| !i.is_empty())
                .map_or(Value::Null, |i| json!(i)),
        );
        let out: EntryId = self.cache_json(op("XAdd", body), "XAdd")?;
        Ok(out.id)
    }

    /// Append an entry whose fields are text, encoded UTF-8.
    pub fn cache_xadd_text<'a>(
        &mut self,
        namespace: &str,
        key: &str,
        fields: impl IntoIterator<Item = (&'a str, &'a str)>,
        id: Option<&str>,
    ) -> Result<String> {
        self.cache_xadd(namespace, key, &CachePair::from_text(fields), id)
    }

    /// How many entries the stream holds; zero when the key is missing.
    pub fn cache_xlen(&mut self, namespace: &str, key: &str) -> Result<i64> {
        let out: Length = self.cache_json(op("XLen", ns_key(namespace, key)), "XLen")?;
        Ok(out.length)
    }

    /// Read entries whose id falls in an inclusive range. `"-"` and `"+"` are
    /// the smallest and largest ids; a bare `"<ms>"` spans that millisecond.
    pub fn cache_xrange(
        &mut self,
        namespace: &str,
        key: &str,
        start: &str,
        end: &str,
        count: Option<usize>,
    ) -> Result<Vec<StreamEntry>> {
        let mut body = ns_key(namespace, key);
        body.insert("start".into(), json!(start));
        body.insert("end".into(), json!(end));
        body.insert("count".into(), count.map_or(Value::Null, |c| json!(c)));
        self.stream_entries(op("XRange", body), "XRange")
    }

    /// Read entries newer than `after` — the non-blocking poll. Pass `"$"` for
    /// "only entries added from now on". This never blocks.
    pub fn cache_xread(
        &mut self,
        namespace: &str,
        key: &str,
        after: &str,
        count: Option<usize>,
    ) -> Result<Vec<StreamEntry>> {
        let mut body = ns_key(namespace, key);
        body.insert("after".into(), json!(after));
        body.insert("count".into(), count.map_or(Value::Null, |c| json!(c)));
        self.stream_entries(op("XRead", body), "XRead")
    }

    /// Delete entries by exact id, returning how many were there.
    pub fn cache_xdel(&mut self, namespace: &str, key: &str, ids: &[&str]) -> Result<i64> {
        if ids.is_empty() {
            return Err(Error::invalid("`ids` must not be empty"));
        }
        let mut body = ns_key(namespace, key);
        body.insert("ids".into(), json!(ids));
        let out: Deleted = self.cache_json(op("XDel", body), "XDel")?;
        Ok(out.deleted)
    }

    /// Cap the stream by evicting its oldest entries, returning how many went.
    pub fn cache_xtrim(&mut self, namespace: &str, key: &str, max_len: usize) -> Result<i64> {
        #[derive(Deserialize)]
        struct Trimmed {
            #[serde(default)]
            trimmed: i64,
        }
        let mut body = ns_key(namespace, key);
        body.insert("max_len".into(), json!(max_len));
        let out: Trimmed = self.cache_json(op("XTrim", body), "XTrim")?;
        Ok(out.trimmed)
    }

    // -- shared --------------------------------------------------------------

    fn cache_json<T: serde::de::DeserializeOwned>(&mut self, op: Value, what: &str) -> Result<T> {
        let response = self.send(op)?;
        response.decode(what)
    }

    /// Read a `CacheValue`, where a null payload is a miss.
    fn cache_value(&mut self, op: Value, what: &str) -> Result<Option<Vec<u8>>> {
        let response = self.send(op)?;
        let data = response.expect("CacheValue", what)?;
        if data.is_null() {
            return Ok(None);
        }
        decode_byte_list(data).map(Some)
    }

    fn stream_entries(&mut self, op: Value, what: &str) -> Result<Vec<StreamEntry>> {
        #[derive(Deserialize)]
        struct Entries {
            #[serde(default)]
            entries: Vec<WireEntry>,
        }
        #[derive(Deserialize)]
        struct WireEntry {
            #[serde(default)]
            id: String,
            #[serde(default)]
            fields: Vec<Value>,
        }
        let out: Entries = self.cache_json(op, what)?;
        out.entries
            .into_iter()
            .map(|entry| {
                let fields = entry
                    .fields
                    .iter()
                    .map(|pair| {
                        let (field, value) = decode_pair(pair)?;
                        Ok(CachePair { field, value })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(StreamEntry {
                    id: entry.id,
                    fields,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_ttl_is_null_rather_than_zero() {
        assert_eq!(ttl_value(None), Value::Null);
        assert_eq!(ttl_value(Some(Duration::ZERO)), Value::Null);
        assert_eq!(ttl_value(Some(Duration::from_millis(1500))), json!(1500));
    }

    #[test]
    fn a_keyed_operation_carries_its_namespace_and_key() {
        let body = op("Get", ns_key("sessions", "u1"));
        assert_eq!(body["Cache"]["Get"]["namespace"], "sessions");
        assert_eq!(body["Cache"]["Get"]["key"], "u1");
    }

    #[test]
    fn pairs_encode_both_halves_as_byte_arrays() {
        let pair = CachePair::new("name", "ada");
        assert_eq!(pair.to_wire(), json!([[110, 97, 109, 101], [97, 100, 97]]));
        assert_eq!(pair.field_text(), Some("name"));
        assert_eq!(pair.value_text(), Some("ada"));
    }

    #[test]
    fn text_pairs_keep_their_order() {
        let pairs = CachePair::from_text([("a", "1"), ("b", "2")]);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].field_text(), Some("a"));
        assert_eq!(pairs[1].value_text(), Some("2"));
    }

    #[test]
    fn a_stream_entry_reads_back_as_text() {
        let entry = StreamEntry {
            id: "1-0".into(),
            fields: CachePair::from_text([("msg", "hi")]),
        };
        assert_eq!(entry.text()["msg"], "hi");
    }
}
