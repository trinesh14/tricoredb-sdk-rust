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

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::client::Client;
use crate::error::{Error, Result};
use crate::response::decode_variant;
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

/// One key and its value, as [`Client::cache_mset`] and [`Client::cache_warm`]
/// take them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    /// The key.
    pub key: String,
    /// The value, as bytes.
    pub value: Vec<u8>,
}

impl CacheEntry {
    /// An entry from a key and any byte string.
    pub fn new(key: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }

    fn to_wire(&self) -> Value {
        json!({"key": self.key, "value": byte_list(&self.value)})
    }
}

/// When [`Client::cache_set_if`] is allowed to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub enum SetCondition {
    /// Write unconditionally.
    #[default]
    Always,
    /// Write only when the key is absent. An expired key counts as absent.
    IfAbsent,
    /// Write only when the key is present and unexpired.
    IfPresent,
}

/// What an operation does with a key's deadline.
///
/// Three intents rather than an optional duration, because "leave the TTL
/// alone" and "remove it" would otherwise share a spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheExpiry {
    /// Leave the deadline as it is.
    Keep,
    /// Expire this long from now.
    Set(Duration),
    /// Remove the deadline, making the key permanent.
    Clear,
}

impl CacheExpiry {
    fn to_wire(self) -> Value {
        match self {
            CacheExpiry::Keep => json!("Keep"),
            CacheExpiry::Set(ttl) => json!({"Set": {"ttl_ms": millis(ttl)}}),
            CacheExpiry::Clear => json!("Clear"),
        }
    }
}

/// Which end of a list an element leaves or joins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ListEnd {
    /// The head, where `LPush` and `LPop` work.
    Left,
    /// The tail, where `RPush` and `RPop` work.
    Right,
}

/// The bitwise operation [`Client::cache_bit_op`] applies across its sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum BitOpKind {
    /// Bits set in every source.
    And,
    /// Bits set in any source.
    Or,
    /// Bits set in an odd number of sources.
    Xor,
    /// Every bit inverted. Takes exactly one source.
    Not,
}

/// One member of a sorted set and its score.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredMember {
    /// The score the set is ordered by. Must be finite.
    pub score: f64,
    /// The member, as bytes.
    pub member: Vec<u8>,
}

impl ScoredMember {
    /// A member from a score and any byte string.
    pub fn new(score: f64, member: impl Into<Vec<u8>>) -> Self {
        Self {
            score,
            member: member.into(),
        }
    }
}

/// One end of a score range.
///
/// A stored score is always finite, and JSON cannot carry an infinity, so the
/// open ends are [`ScoreBound::lowest`] and [`ScoreBound::highest`] rather than
/// `f64::NEG_INFINITY` and `f64::INFINITY`. A non-finite bound is refused
/// before anything is sent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreBound {
    /// The score at this end.
    pub value: f64,
    /// Whether a member with exactly this score is left out.
    pub exclusive: bool,
}

impl ScoreBound {
    /// A bound that admits `value` itself.
    pub fn inclusive(value: f64) -> Self {
        Self {
            value,
            exclusive: false,
        }
    }

    /// A bound that stops just short of `value`.
    pub fn exclusive(value: f64) -> Self {
        Self {
            value,
            exclusive: true,
        }
    }

    /// Below every score a set can hold.
    pub fn lowest() -> Self {
        Self::inclusive(-f64::MAX)
    }

    /// Above every score a set can hold.
    pub fn highest() -> Self {
        Self::inclusive(f64::MAX)
    }

    fn to_wire(self, name: &str) -> Result<Value> {
        Ok(json!({"value": score_value(self.value, name)?, "exclusive": self.exclusive}))
    }
}

/// What [`Client::cache_zadd`] changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct ZAddCounts {
    /// Members that were not in the set before.
    #[serde(default)]
    pub added: i64,
    /// Members already there whose score changed.
    #[serde(default)]
    pub updated: i64,
}

/// What [`Client::cache_warm`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct WarmCounts {
    /// Entries that were written.
    #[serde(default)]
    pub written: i64,
    /// Entries left alone because a live value was already cached.
    #[serde(default, rename = "alreadyCached")]
    pub already_cached: i64,
}

/// Which of its four answers [`Client::cache_get_fresh`] gave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshState {
    /// The value is cached and within its fresh window.
    Fresh,
    /// The value is cached but past its fresh window.
    Stale,
    /// The source is known to have no value for this key.
    Missing,
    /// Nothing is cached for this key.
    Miss,
}

/// The answer to [`Client::cache_get_fresh`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshRead {
    /// Which answer this is.
    pub state: FreshState,
    /// The cached value, when the state is fresh or stale.
    pub value: Option<Vec<u8>>,
    /// On a stale read, whether this caller holds the refresh lease and should
    /// recompute the value.
    pub refresh: bool,
    /// On a miss, whether this caller holds the lease and should compute the
    /// value.
    pub lease: bool,
    /// On a miss another caller is already computing, how long to wait before
    /// asking again.
    pub retry_after_ms: Option<u64>,
}

/// The answer to [`Client::cache_lock_acquire`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CacheLock {
    /// Whether the caller now holds the lock.
    #[serde(default)]
    pub acquired: bool,
    /// The fencing token, when the lock was acquired. It rises with every
    /// acquisition and is never reused; pass it to the resource the lock
    /// protects.
    #[serde(default)]
    pub token: Option<u64>,
    /// Who holds the lock.
    #[serde(default)]
    pub holder: Option<String>,
    /// When the current lease ends, in Unix epoch milliseconds.
    #[serde(default, rename = "expiresAtMs")]
    pub expires_at_ms: Option<i64>,
}

/// The answer to [`Client::cache_lock_renew`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct CacheLockRenewal {
    /// Whether the lease was extended. False once it has lapsed, even when
    /// nobody else took the lock since.
    #[serde(default)]
    pub renewed: bool,
    /// The fencing token of the lease that was renewed.
    #[serde(default)]
    pub token: Option<u64>,
    /// When the renewed lease ends, in Unix epoch milliseconds.
    #[serde(default, rename = "expiresAtMs")]
    pub expires_at_ms: Option<i64>,
}

/// What [`Client::cache_ltrim`] left behind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct TrimCounts {
    /// How many elements the list holds now.
    #[serde(default)]
    pub length: i64,
    /// How many elements were dropped.
    #[serde(default)]
    pub removed: i64,
}

/// What [`Client::cache_expire_at`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct ExpireAtOutcome {
    /// Whether a live key was given the deadline.
    #[serde(default)]
    pub updated: bool,
    /// Whether the key was deleted because the deadline had already passed.
    #[serde(default)]
    pub deleted: bool,
}

/// The answer to [`Client::cache_compare_and_swap`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareAndSwap {
    /// Whether the write happened.
    pub swapped: bool,
    /// On a mismatch, the value actually present, when it is valid UTF-8.
    pub current: Option<Vec<u8>>,
    /// On a mismatch, whether the value present is binary. The server does
    /// not return such a value; read it with [`Client::cache_get`].
    pub current_is_binary: bool,
}

/// The memory budget in [`CacheStats`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct CacheMemoryStats {
    /// Whether a memory budget is enforced.
    #[serde(default)]
    pub enabled: bool,
    /// The ceiling, in bytes.
    #[serde(default, rename = "maxBytes")]
    pub max_bytes: Option<u64>,
    /// The level eviction brings usage back down to, in bytes.
    #[serde(default, rename = "lowWaterBytes")]
    pub low_water_bytes: Option<u64>,
    /// The last measured resident size, when one has been taken.
    #[serde(default, rename = "residentBytes")]
    pub resident_bytes: Option<u64>,
    /// How many measurements have been taken.
    #[serde(default)]
    pub measurements: u64,
}

/// The evictor in [`CacheStats`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct CacheEvictionStats {
    /// The eviction policy's name.
    #[serde(default)]
    pub policy: String,
    /// How many entries have been evicted.
    #[serde(default)]
    pub evicted: u64,
}

/// What one node's cache is doing. Node-wide, not per database.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct CacheStats {
    /// Lookups that found a value.
    #[serde(default)]
    pub hits: u64,
    /// Lookups that found none.
    #[serde(default)]
    pub misses: u64,
    /// Hits over lookups, when there has been a lookup.
    #[serde(default, rename = "hitRatio")]
    pub hit_ratio: Option<f64>,
    /// The memory budget.
    #[serde(default)]
    pub memory: CacheMemoryStats,
    /// The evictor.
    #[serde(default)]
    pub eviction: CacheEvictionStats,
}

/// A namespace's limit and what it holds now.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NamespaceLimit {
    /// The namespace.
    #[serde(default)]
    pub namespace: String,
    /// How many live keys it holds.
    #[serde(default)]
    pub keys: u64,
    /// How many bytes of values it holds.
    #[serde(default)]
    pub bytes: u64,
    /// The cap on live keys, when there is one.
    #[serde(default)]
    pub max_keys: Option<u64>,
    /// The cap on value bytes, when there is one.
    #[serde(default)]
    pub max_bytes: Option<u64>,
}

/// One message read by [`Client::cache_poll`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChannelMessage {
    /// The message's id. Ids rise across every channel.
    #[serde(default)]
    pub id: u64,
    /// The channel it was published to.
    #[serde(default)]
    pub channel: String,
    /// The message text.
    #[serde(default)]
    pub message: String,
    /// When it was published, in Unix epoch milliseconds.
    #[serde(default)]
    pub at_ms: i64,
}

/// One page of messages from [`Client::cache_poll`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct PolledMessages {
    /// The messages, oldest first.
    #[serde(default)]
    pub messages: Vec<ChannelMessage>,
    /// The id to pass as `after` on the next poll.
    #[serde(default)]
    pub next: u64,
}

/// One cache operation inside [`Client::cache_pipeline`],
/// [`Client::cache_transaction`] or a script.
///
/// The constructors cover the common operations; [`CacheCommand::new`] builds
/// any other from the server's own variant name and fields.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct CacheCommand(Value);

impl CacheCommand {
    /// Any cache operation, by the server's variant name and its fields.
    pub fn new(variant: &str, body: Value) -> Self {
        CacheCommand(json!({variant: body}))
    }

    /// An operation that takes no fields, such as `Ping`.
    pub fn unit(variant: &str) -> Self {
        CacheCommand(Value::String(variant.to_string()))
    }

    /// Read a value.
    pub fn get(namespace: &str, key: &str) -> Self {
        Self::new("Get", Value::Object(ns_key(namespace, key)))
    }

    /// Store a value, expiring after `ttl` when one is given.
    pub fn set(namespace: &str, key: &str, value: &[u8], ttl: Option<Duration>) -> Self {
        let mut body = ns_key(namespace, key);
        body.insert("value".into(), byte_list(value));
        body.insert("ttl_ms".into(), ttl_value(ttl));
        Self::new("Set", Value::Object(body))
    }

    /// Delete a key.
    pub fn delete(namespace: &str, key: &str) -> Self {
        Self::new("Delete", Value::Object(ns_key(namespace, key)))
    }

    /// Whether a key is present and unexpired.
    pub fn exists(namespace: &str, key: &str) -> Self {
        Self::new("Exists", Value::Object(ns_key(namespace, key)))
    }

    /// Add to a counter.
    pub fn incr(namespace: &str, key: &str, by: i64) -> Self {
        let mut body = ns_key(namespace, key);
        body.insert("by".into(), json!(by));
        Self::new("Incr", Value::Object(body))
    }
}

/// What one operation of a pipeline, transaction or script answered.
#[derive(Debug, Clone, PartialEq)]
pub enum CacheReply {
    /// The operation succeeded.
    Ok {
        /// The name of the data variant, as in [`crate::Response::kind`].
        kind: String,
        /// That variant's payload.
        data: Value,
    },
    /// The operation failed, with the server's message. The operations after
    /// it still ran.
    Error(String),
}

impl CacheReply {
    /// Whether the operation succeeded.
    pub fn is_ok(&self) -> bool {
        matches!(self, CacheReply::Ok { .. })
    }

    /// The bytes of a `CacheValue` reply. `None` for a miss, and for a reply
    /// that is not a value at all.
    pub fn value(&self) -> Option<Vec<u8>> {
        match self {
            CacheReply::Ok { kind, data } if kind == "CacheValue" => decode_byte_list(data).ok(),
            _ => None,
        }
    }

    fn from_wire(value: &Value) -> Result<CacheReply> {
        if let Some(error) = value.get("error") {
            return Ok(CacheReply::Error(match error.as_str() {
                Some(text) => text.to_string(),
                None => error.to_string(),
            }));
        }
        match value.get("ok") {
            Some(data) => {
                let (kind, data) = decode_variant(data.clone());
                Ok(CacheReply::Ok { kind, data })
            }
            None => Err(Error::protocol(
                "a pipeline result is neither `ok` nor `error`".to_string(),
            )),
        }
    }
}

fn millis(duration: Duration) -> i64 {
    duration.as_millis().min(i64::MAX as u128) as i64
}

/// A score as JSON. NaN has no place in an ordering and JSON has no spelling
/// for an infinity, so both are refused here by name.
fn score_value(score: f64, name: &str) -> Result<Value> {
    serde_json::Number::from_f64(score)
        .map(Value::Number)
        .ok_or_else(|| Error::invalid(format!("`{name}` must be a finite number, got {score}")))
}

fn keys_body(namespace: &str, keys: &[&str]) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("namespace".into(), Value::String(namespace.to_string()));
    body.insert("keys".into(), json!(keys));
    body
}

fn range_value(range: Option<(i64, i64)>) -> Value {
    range.map_or(Value::Null, |(start, end)| json!([start, end]))
}

fn decode_hex(text: &str) -> Result<Vec<u8>> {
    let malformed = || Error::protocol("malformed hex value in a cache reply".to_string());
    if text.len() % 2 != 0 || !text.is_ascii() {
        return Err(malformed());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| malformed()))
        .collect()
}

fn scored_members(value: &Value, what: &str) -> Result<Vec<ScoredMember>> {
    let malformed = || Error::protocol(format!("malformed {what} payload: no scored members"));
    value
        .get("members")
        .and_then(Value::as_array)
        .ok_or_else(malformed)?
        .iter()
        .map(|entry| {
            let score = entry
                .get("score")
                .and_then(Value::as_f64)
                .ok_or_else(malformed)?;
            let member = decode_byte_list(entry.get("member").unwrap_or(&Value::Null))?;
            Ok(ScoredMember { score, member })
        })
        .collect()
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

fn entries_op(
    variant: &str,
    namespace: &str,
    entries: &[CacheEntry],
    ttl: Option<Duration>,
) -> Value {
    let mut body = Map::new();
    body.insert("namespace".into(), Value::String(namespace.to_string()));
    body.insert(
        "entries".into(),
        Value::Array(entries.iter().map(CacheEntry::to_wire).collect()),
    );
    body.insert("ttl_ms".into(), ttl_value(ttl));
    op(variant, body)
}

fn zrange_op(
    namespace: &str,
    key: &str,
    start: i64,
    stop: i64,
    rev: bool,
    with_scores: bool,
) -> Value {
    let mut body = ns_key(namespace, key);
    body.insert("start".into(), json!(start));
    body.insert("stop".into(), json!(stop));
    body.insert("rev".into(), json!(rev));
    body.insert("with_scores".into(), json!(with_scores));
    op("ZRange", body)
}

fn zrange_by_score_op(
    namespace: &str,
    key: &str,
    min: ScoreBound,
    max: ScoreBound,
    rev: bool,
    with_scores: bool,
    limit: Option<(u64, u64)>,
) -> Result<Value> {
    let mut body = ns_key(namespace, key);
    body.insert("min".into(), min.to_wire("min")?);
    body.insert("max".into(), max.to_wire("max")?);
    body.insert("rev".into(), json!(rev));
    body.insert("with_scores".into(), json!(with_scores));
    body.insert(
        "limit".into(),
        limit.map_or(Value::Null, |(offset, count)| json!([offset, count])),
    );
    Ok(op("ZRangeByScore", body))
}

fn lock_op(variant: &str, namespace: &str, key: &str, owner: &str, ttl: Option<Duration>) -> Value {
    let mut body = ns_key(namespace, key);
    body.insert("owner".into(), json!(owner));
    if let Some(ttl) = ttl {
        body.insert("ttl_ms".into(), json!(millis(ttl)));
    }
    op(variant, body)
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

    // -- many keys, conditions and atomic reads -------------------------------

    /// Read many values in one round trip.
    ///
    /// One result per key, in the order asked, with `None` where the key is
    /// absent or expired. Not a snapshot: another writer may land between two
    /// of the reads.
    pub fn cache_mget(&mut self, namespace: &str, keys: &[&str]) -> Result<Vec<Option<Vec<u8>>>> {
        self.cache_values(op("MGet", keys_body(namespace, keys)), "MGet")
    }

    /// Write many values as one batch, all with the same optional TTL,
    /// returning how many were written. A repeated key is refused by the
    /// server rather than resolved last-wins.
    pub fn cache_mset(
        &mut self,
        namespace: &str,
        entries: &[CacheEntry],
        ttl: Option<Duration>,
    ) -> Result<i64> {
        #[derive(Deserialize)]
        struct Set {
            #[serde(default)]
            set: i64,
        }
        let out: Set = self.cache_json(entries_op("MSet", namespace, entries, ttl), "MSet")?;
        Ok(out.set)
    }

    /// Cache warming: write the entries that are not already cached, leaving
    /// live values alone.
    pub fn cache_warm(
        &mut self,
        namespace: &str,
        entries: &[CacheEntry],
        ttl: Option<Duration>,
    ) -> Result<WarmCounts> {
        self.cache_json(entries_op("Warm", namespace, entries, ttl), "Warm")
    }

    /// Write a value under a condition, saying what happens to the deadline.
    ///
    /// `false` means the condition was not met, not that anything failed.
    /// [`CacheExpiry::Keep`] is Redis's `KEEPTTL`; [`CacheExpiry::Clear`] is
    /// what a plain set does.
    pub fn cache_set_if(
        &mut self,
        namespace: &str,
        key: &str,
        value: &[u8],
        condition: SetCondition,
        expiry: CacheExpiry,
    ) -> Result<bool> {
        #[derive(Deserialize)]
        struct Set {
            #[serde(default)]
            set: bool,
        }
        let mut body = ns_key(namespace, key);
        body.insert("value".into(), byte_list(value));
        body.insert("condition".into(), json!(condition));
        body.insert("expiry".into(), expiry.to_wire());
        let out: Set = self.cache_json(op("SetIf", body), "SetIf")?;
        Ok(out.set)
    }

    /// Write only if the key still holds what the caller last read.
    ///
    /// `expected` of `None` requires the key to be absent, and `value` of
    /// `None` deletes it. On a mismatch nothing is written and the answer
    /// carries what is actually there.
    pub fn cache_compare_and_swap(
        &mut self,
        namespace: &str,
        key: &str,
        expected: Option<&[u8]>,
        value: Option<&[u8]>,
        ttl: Option<Duration>,
    ) -> Result<CompareAndSwap> {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(default)]
            swapped: bool,
            #[serde(default)]
            current: Option<String>,
            #[serde(default, rename = "currentIsBinary")]
            current_is_binary: bool,
        }
        let mut body = ns_key(namespace, key);
        body.insert("expected".into(), expected.map_or(Value::Null, byte_list));
        body.insert("value".into(), value.map_or(Value::Null, byte_list));
        body.insert("ttl_ms".into(), ttl_value(ttl));
        let out: Wire = self.cache_json(op("CompareAndSwap", body), "CompareAndSwap")?;
        Ok(CompareAndSwap {
            swapped: out.swapped,
            current: out.current.map(String::into_bytes),
            current_is_binary: out.current_is_binary,
        })
    }

    /// Read a key and delete it in one atomic step. `None` when it was absent.
    pub fn cache_get_del(&mut self, namespace: &str, key: &str) -> Result<Option<Vec<u8>>> {
        self.cache_value(op("GetDel", ns_key(namespace, key)), "GetDel")
    }

    /// Write a value and return the one it replaced, atomically. The old
    /// value's TTL is dropped; pass `ttl` to set one.
    pub fn cache_get_set(
        &mut self,
        namespace: &str,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<Option<Vec<u8>>> {
        let mut body = ns_key(namespace, key);
        body.insert("value".into(), byte_list(value));
        body.insert("ttl_ms".into(), ttl_value(ttl));
        self.cache_value(op("GetSet", body), "GetSet")
    }

    /// Read a key and change its deadline in the same atomic step.
    pub fn cache_get_ex(
        &mut self,
        namespace: &str,
        key: &str,
        expiry: CacheExpiry,
    ) -> Result<Option<Vec<u8>>> {
        let mut body = ns_key(namespace, key);
        body.insert("expiry".into(), expiry.to_wire());
        self.cache_value(op("GetEx", body), "GetEx")
    }

    /// Concatenate bytes onto a key's value, creating it when absent, and
    /// return the new length. An existing key keeps its expiry.
    pub fn cache_append(&mut self, namespace: &str, key: &str, value: &[u8]) -> Result<i64> {
        let mut body = ns_key(namespace, key);
        body.insert("value".into(), byte_list(value));
        let out: Length = self.cache_json(op("Append", body), "Append")?;
        Ok(out.length)
    }

    /// Set the instant an existing live key dies, in Unix epoch milliseconds.
    /// A deadline already passed deletes the key.
    pub fn cache_expire_at(
        &mut self,
        namespace: &str,
        key: &str,
        at_ms: i64,
    ) -> Result<ExpireAtOutcome> {
        let mut body = ns_key(namespace, key);
        body.insert("at_ms".into(), json!(at_ms));
        self.cache_json(op("ExpireAt", body), "ExpireAt")
    }

    // -- stale-while-revalidate ------------------------------------------------

    /// Write a value that reads as fresh for `fresh` and as stale after that,
    /// until its TTL (if any) ends. Returns when it stops being fresh, in Unix
    /// epoch milliseconds.
    pub fn cache_set_fresh(
        &mut self,
        namespace: &str,
        key: &str,
        value: &[u8],
        fresh: Duration,
        ttl: Option<Duration>,
    ) -> Result<i64> {
        #[derive(Deserialize)]
        struct Fresh {
            #[serde(default, rename = "freshUntilMs")]
            fresh_until_ms: i64,
        }
        let mut body = ns_key(namespace, key);
        body.insert("value".into(), byte_list(value));
        body.insert("fresh_ms".into(), json!(millis(fresh)));
        body.insert("ttl_ms".into(), ttl_value(ttl));
        let out: Fresh = self.cache_json(op("SetFresh", body), "SetFresh")?;
        Ok(out.fresh_until_ms)
    }

    /// Read with stale-while-revalidate and stampede protection.
    ///
    /// `owner` names this caller; of the callers that find a value stale or
    /// missing, one is handed a lease of `lease` to recompute it and the rest
    /// are told to serve the stale value or wait.
    pub fn cache_get_fresh(
        &mut self,
        namespace: &str,
        key: &str,
        owner: &str,
        lease: Duration,
    ) -> Result<FreshRead> {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(default)]
            state: String,
            #[serde(default)]
            value: Option<String>,
            #[serde(default, rename = "valueHex")]
            value_hex: Option<String>,
            #[serde(default)]
            refresh: bool,
            #[serde(default)]
            lease: bool,
            #[serde(default, rename = "retryAfterMs")]
            retry_after_ms: Option<u64>,
        }
        let mut body = ns_key(namespace, key);
        body.insert("owner".into(), json!(owner));
        body.insert("lease_ms".into(), json!(millis(lease)));
        let out: Wire = self.cache_json(op("GetFresh", body), "GetFresh")?;
        let state = match out.state.as_str() {
            "fresh" => FreshState::Fresh,
            "stale" => FreshState::Stale,
            "missing" => FreshState::Missing,
            "miss" => FreshState::Miss,
            other => {
                return Err(Error::protocol(format!(
                    "GetFresh answered with the unknown state `{other}`"
                )))
            }
        };
        // A value that is valid UTF-8 arrives as text, any other as hex.
        let value = match (out.value, out.value_hex) {
            (Some(text), _) => Some(text.into_bytes()),
            (None, Some(hex)) => Some(decode_hex(&hex)?),
            (None, None) => None,
        };
        Ok(FreshRead {
            state,
            value,
            refresh: out.refresh,
            lease: out.lease,
            retry_after_ms: out.retry_after_ms,
        })
    }

    /// Negative caching: remember for `ttl` that the source has no value for
    /// `key`.
    pub fn cache_set_missing(&mut self, namespace: &str, key: &str, ttl: Duration) -> Result<()> {
        let mut body = ns_key(namespace, key);
        body.insert("ttl_ms".into(), json!(millis(ttl)));
        self.send(op("SetMissing", body)).map(|_| ())
    }

    // -- sorted sets -----------------------------------------------------------

    /// Add scored members to a sorted set, or re-score ones already there. A
    /// score that is not finite is refused.
    pub fn cache_zadd(
        &mut self,
        namespace: &str,
        key: &str,
        members: &[ScoredMember],
    ) -> Result<ZAddCounts> {
        if members.is_empty() {
            return Err(Error::invalid("`members` must not be empty"));
        }
        let members = members
            .iter()
            .map(|m| {
                Ok(json!({
                    "score": score_value(m.score, "score")?,
                    "member": byte_list(&m.member),
                }))
            })
            .collect::<Result<Vec<Value>>>()?;
        let mut body = ns_key(namespace, key);
        body.insert("members".into(), Value::Array(members));
        self.cache_json(op("ZAdd", body), "ZAdd")
    }

    /// Remove members from a sorted set, returning how many were there.
    pub fn cache_zrem(
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
        let out: Removed = self.cache_json(op("ZRem", body), "ZRem")?;
        Ok(out.removed)
    }

    /// One member's score, or `None` when it is not in the set.
    pub fn cache_zscore(
        &mut self,
        namespace: &str,
        key: &str,
        member: &[u8],
    ) -> Result<Option<f64>> {
        #[derive(Deserialize)]
        struct Scored {
            #[serde(default)]
            score: Option<f64>,
        }
        let mut body = ns_key(namespace, key);
        body.insert("member".into(), byte_list(member));
        let out: Scored = self.cache_json(op("ZScore", body), "ZScore")?;
        Ok(out.score)
    }

    /// How many members the sorted set holds; zero when the key is missing.
    pub fn cache_zcard(&mut self, namespace: &str, key: &str) -> Result<i64> {
        #[derive(Deserialize)]
        struct Cardinality {
            #[serde(default)]
            cardinality: i64,
        }
        let out: Cardinality = self.cache_json(op("ZCard", ns_key(namespace, key)), "ZCard")?;
        Ok(out.cardinality)
    }

    /// A member's position in the ordering, or `None` when it is absent.
    /// `rev` counts from the high-scoring end.
    pub fn cache_zrank(
        &mut self,
        namespace: &str,
        key: &str,
        member: &[u8],
        rev: bool,
    ) -> Result<Option<i64>> {
        #[derive(Deserialize)]
        struct Ranked {
            #[serde(default)]
            rank: Option<i64>,
        }
        let mut body = ns_key(namespace, key);
        body.insert("member".into(), byte_list(member));
        body.insert("rev".into(), json!(rev));
        let out: Ranked = self.cache_json(op("ZRank", body), "ZRank")?;
        Ok(out.rank)
    }

    /// Members by rank, `start..=stop` inclusive. Negative indices count from
    /// the end, so `0, -1` is the whole set.
    pub fn cache_zrange(
        &mut self,
        namespace: &str,
        key: &str,
        start: i64,
        stop: i64,
        rev: bool,
    ) -> Result<Vec<Vec<u8>>> {
        let value: Value =
            self.cache_json(zrange_op(namespace, key, start, stop, rev, false), "ZRange")?;
        decode_byte_lists(value.get("members").unwrap_or(&Value::Null))
    }

    /// [`Client::cache_zrange`], with each member's score.
    pub fn cache_zrange_with_scores(
        &mut self,
        namespace: &str,
        key: &str,
        start: i64,
        stop: i64,
        rev: bool,
    ) -> Result<Vec<ScoredMember>> {
        let value: Value =
            self.cache_json(zrange_op(namespace, key, start, stop, rev, true), "ZRange")?;
        scored_members(&value, "ZRange")
    }

    /// Members by score, between two bounds.
    ///
    /// `limit` is `(offset, count)` applied after the range is selected, which
    /// is what pages a wide range.
    pub fn cache_zrange_by_score(
        &mut self,
        namespace: &str,
        key: &str,
        min: ScoreBound,
        max: ScoreBound,
        rev: bool,
        limit: Option<(u64, u64)>,
    ) -> Result<Vec<Vec<u8>>> {
        let request = zrange_by_score_op(namespace, key, min, max, rev, false, limit)?;
        let value: Value = self.cache_json(request, "ZRangeByScore")?;
        decode_byte_lists(value.get("members").unwrap_or(&Value::Null))
    }

    /// [`Client::cache_zrange_by_score`], with each member's score.
    pub fn cache_zrange_by_score_with_scores(
        &mut self,
        namespace: &str,
        key: &str,
        min: ScoreBound,
        max: ScoreBound,
        rev: bool,
        limit: Option<(u64, u64)>,
    ) -> Result<Vec<ScoredMember>> {
        let request = zrange_by_score_op(namespace, key, min, max, rev, true, limit)?;
        let value: Value = self.cache_json(request, "ZRangeByScore")?;
        scored_members(&value, "ZRangeByScore")
    }

    /// Add `by` to a member's score, creating it at `by` when absent, and
    /// return the new score.
    pub fn cache_zincr_by(
        &mut self,
        namespace: &str,
        key: &str,
        member: &[u8],
        by: f64,
    ) -> Result<f64> {
        #[derive(Deserialize)]
        struct Scored {
            score: f64,
        }
        let mut body = ns_key(namespace, key);
        body.insert("member".into(), byte_list(member));
        body.insert("by".into(), score_value(by, "by")?);
        let out: Scored = self.cache_json(op("ZIncrBy", body), "ZIncrBy")?;
        Ok(out.score)
    }

    // -- locks -----------------------------------------------------------------

    /// Take a lease on a lock, or refresh one `owner` already holds.
    ///
    /// Acquiring a lock you hold returns the same token with a refreshed
    /// lease, so a retried acquire is safe.
    pub fn cache_lock_acquire(
        &mut self,
        namespace: &str,
        key: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<CacheLock> {
        self.cache_json(
            lock_op("LockAcquire", namespace, key, owner, Some(ttl)),
            "LockAcquire",
        )
    }

    /// Extend a lease the caller still holds.
    pub fn cache_lock_renew(
        &mut self,
        namespace: &str,
        key: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<CacheLockRenewal> {
        self.cache_json(
            lock_op("LockRenew", namespace, key, owner, Some(ttl)),
            "LockRenew",
        )
    }

    /// Give up a lock. `false` when `owner` is not its live holder.
    pub fn cache_lock_release(&mut self, namespace: &str, key: &str, owner: &str) -> Result<bool> {
        #[derive(Deserialize)]
        struct Released {
            #[serde(default)]
            released: bool,
        }
        let out: Released = self.cache_json(
            lock_op("LockRelease", namespace, key, owner, None),
            "LockRelease",
        )?;
        Ok(out.released)
    }

    // -- bitmaps ---------------------------------------------------------------

    /// Set one bit of a string value, growing it with zero bytes if needed,
    /// and return the bit's old value. Bits are numbered from the most
    /// significant bit of byte 0.
    pub fn cache_set_bit(
        &mut self,
        namespace: &str,
        key: &str,
        offset: u64,
        value: bool,
    ) -> Result<bool> {
        #[derive(Deserialize)]
        struct Previous {
            #[serde(default)]
            previous: u8,
        }
        let mut body = ns_key(namespace, key);
        body.insert("offset".into(), json!(offset));
        body.insert("value".into(), json!(value));
        let out: Previous = self.cache_json(op("SetBit", body), "SetBit")?;
        Ok(out.previous != 0)
    }

    /// Read one bit. Past the end of the value, and on a missing key, every
    /// bit is unset.
    pub fn cache_get_bit(&mut self, namespace: &str, key: &str, offset: u64) -> Result<bool> {
        #[derive(Deserialize)]
        struct Bit {
            #[serde(default)]
            bit: u8,
        }
        let mut body = ns_key(namespace, key);
        body.insert("offset".into(), json!(offset));
        let out: Bit = self.cache_json(op("GetBit", body), "GetBit")?;
        Ok(out.bit != 0)
    }

    /// Count the set bits, optionally within an inclusive **byte** range whose
    /// negative indices count from the end.
    pub fn cache_bit_count(
        &mut self,
        namespace: &str,
        key: &str,
        range: Option<(i64, i64)>,
    ) -> Result<i64> {
        #[derive(Deserialize)]
        struct Count {
            #[serde(default)]
            count: i64,
        }
        let mut body = ns_key(namespace, key);
        body.insert("range".into(), range_value(range));
        let out: Count = self.cache_json(op("BitCount", body), "BitCount")?;
        Ok(out.count)
    }

    /// The position of the first bit equal to `bit`, optionally within a byte
    /// range, or `-1` when there is none.
    pub fn cache_bit_pos(
        &mut self,
        namespace: &str,
        key: &str,
        bit: bool,
        range: Option<(i64, i64)>,
    ) -> Result<i64> {
        #[derive(Deserialize)]
        struct Position {
            position: i64,
        }
        let mut body = ns_key(namespace, key);
        body.insert("bit".into(), json!(bit));
        body.insert("range".into(), range_value(range));
        let out: Position = self.cache_json(op("BitPos", body), "BitPos")?;
        Ok(out.position)
    }

    /// Combine several values bitwise, store the result at `dest`, and return
    /// its size in bytes. Shorter sources are zero-padded to the longest.
    pub fn cache_bit_op(
        &mut self,
        namespace: &str,
        kind: BitOpKind,
        dest: &str,
        keys: &[&str],
    ) -> Result<i64> {
        let mut body = Map::new();
        body.insert("namespace".into(), json!(namespace));
        body.insert("op".into(), json!(kind));
        body.insert("dest".into(), json!(dest));
        body.insert("keys".into(), json!(keys));
        let out: Length = self.cache_json(op("BitOp", body), "BitOp")?;
        Ok(out.length)
    }

    // -- HyperLogLog -----------------------------------------------------------

    /// Add elements to a HyperLogLog, creating it when absent. Returns whether
    /// the estimate may have moved.
    pub fn cache_pf_add(
        &mut self,
        namespace: &str,
        key: &str,
        elements: &[impl AsRef<[u8]>],
    ) -> Result<bool> {
        #[derive(Deserialize)]
        struct Changed {
            #[serde(default)]
            changed: bool,
        }
        let mut body = ns_key(namespace, key);
        body.insert("elements".into(), byte_lists(elements, "elements")?);
        let out: Changed = self.cache_json(op("PfAdd", body), "PfAdd")?;
        Ok(out.changed)
    }

    /// The estimated number of distinct elements. With several keys, the
    /// estimate is of their union.
    pub fn cache_pf_count(&mut self, namespace: &str, keys: &[&str]) -> Result<i64> {
        #[derive(Deserialize)]
        struct Count {
            #[serde(default)]
            count: i64,
        }
        let out: Count = self.cache_json(op("PfCount", keys_body(namespace, keys)), "PfCount")?;
        Ok(out.count)
    }

    /// Merge several HyperLogLogs into `dest`, which counts as a source when
    /// it already exists.
    pub fn cache_pf_merge(&mut self, namespace: &str, dest: &str, sources: &[&str]) -> Result<()> {
        let mut body = Map::new();
        body.insert("namespace".into(), json!(namespace));
        body.insert("dest".into(), json!(dest));
        body.insert("sources".into(), json!(sources));
        self.send(op("PfMerge", body)).map(|_| ())
    }

    // -- more lists, sets and hashes -------------------------------------------

    /// Atomically pop an element from one end of `source` and push it onto one
    /// end of `destination`. `None` when `source` is empty. The two may be the
    /// same key, which rotates the list.
    pub fn cache_lmove(
        &mut self,
        namespace: &str,
        source: &str,
        destination: &str,
        from: ListEnd,
        to: ListEnd,
    ) -> Result<Option<Vec<u8>>> {
        let mut body = Map::new();
        body.insert("namespace".into(), json!(namespace));
        body.insert("source".into(), json!(source));
        body.insert("destination".into(), json!(destination));
        body.insert("from".into(), json!(from));
        body.insert("to".into(), json!(to));
        self.cache_value(op("LMove", body), "LMove")
    }

    /// Keep only the inclusive index range of a list, dropping the rest. A
    /// range selecting nothing empties the list, which deletes the key.
    pub fn cache_ltrim(
        &mut self,
        namespace: &str,
        key: &str,
        start: i64,
        stop: i64,
    ) -> Result<TrimCounts> {
        let mut body = ns_key(namespace, key);
        body.insert("start".into(), json!(start));
        body.insert("stop".into(), json!(stop));
        self.cache_json(op("LTrim", body), "LTrim")
    }

    /// Pop the head of the first non-empty list among `keys`, waiting up to
    /// `timeout` for one to receive an element. Returns the key it came from
    /// and the element, or `None` when the wait ran out.
    ///
    /// The wait happens on the server, so keep `timeout` under this
    /// connection's read timeout.
    pub fn cache_blpop(
        &mut self,
        namespace: &str,
        keys: &[&str],
        timeout: Duration,
    ) -> Result<Option<(String, Vec<u8>)>> {
        self.cache_blocking_pop("BLPop", namespace, keys, timeout)
    }

    /// [`Client::cache_blpop`] from the tail.
    pub fn cache_brpop(
        &mut self,
        namespace: &str,
        keys: &[&str],
        timeout: Duration,
    ) -> Result<Option<(String, Vec<u8>)>> {
        self.cache_blocking_pop("BRPop", namespace, keys, timeout)
    }

    fn cache_blocking_pop(
        &mut self,
        variant: &str,
        namespace: &str,
        keys: &[&str],
        timeout: Duration,
    ) -> Result<Option<(String, Vec<u8>)>> {
        let mut body = keys_body(namespace, keys);
        body.insert("timeout_ms".into(), json!(millis(timeout)));
        let mut values = self.cache_values(op(variant, body), variant)?.into_iter();
        match (values.next().flatten(), values.next().flatten()) {
            (Some(key), Some(value)) => {
                Ok(Some((String::from_utf8_lossy(&key).into_owned(), value)))
            }
            _ => Ok(None),
        }
    }

    /// Every member of every named set, without duplicates. A missing key is
    /// the empty set.
    pub fn cache_sunion(&mut self, namespace: &str, keys: &[&str]) -> Result<Vec<Vec<u8>>> {
        self.cache_members("SUnion", keys_body(namespace, keys))
    }

    /// The members present in every named set.
    pub fn cache_sinter(&mut self, namespace: &str, keys: &[&str]) -> Result<Vec<Vec<u8>>> {
        self.cache_members("SInter", keys_body(namespace, keys))
    }

    /// The members of the first set that are in none of the rest.
    pub fn cache_sdiff(&mut self, namespace: &str, keys: &[&str]) -> Result<Vec<Vec<u8>>> {
        self.cache_members("SDiff", keys_body(namespace, keys))
    }

    /// Random members of a set, without removing them.
    ///
    /// `count` of `None` gives at most one member; a positive `n` up to `n`
    /// distinct members; a negative `n` exactly `|n|` members with repeats
    /// allowed.
    pub fn cache_srand_member(
        &mut self,
        namespace: &str,
        key: &str,
        count: Option<i64>,
    ) -> Result<Vec<Vec<u8>>> {
        let mut body = ns_key(namespace, key);
        body.insert("count".into(), count.map_or(Value::Null, |c| json!(c)));
        self.cache_members("SRandMember", body)
    }

    fn cache_members(&mut self, variant: &str, body: Map<String, Value>) -> Result<Vec<Vec<u8>>> {
        let value: Value = self.cache_json(op(variant, body), variant)?;
        decode_byte_lists(value.get("members").unwrap_or(&Value::Null))
    }

    /// Add `by` to one hash field, atomically, and return the new value. A
    /// missing field starts at zero.
    pub fn cache_hincr_by(
        &mut self,
        namespace: &str,
        key: &str,
        field: &[u8],
        by: i64,
    ) -> Result<i64> {
        #[derive(Deserialize)]
        struct CounterValue {
            #[serde(default)]
            value: i64,
        }
        let mut body = ns_key(namespace, key);
        body.insert("field".into(), byte_list(field));
        body.insert("by".into(), json!(by));
        let out: CounterValue = self.cache_json(op("HIncrBy", body), "HIncrBy")?;
        Ok(out.value)
    }

    // -- pipelines, transactions and scripts -----------------------------------

    /// Run operations in order in one round trip, each exactly as if it had
    /// been sent alone. A failing operation does not stop the rest.
    pub fn cache_pipeline(&mut self, ops: &[CacheCommand]) -> Result<Vec<CacheReply>> {
        self.cache_replies(json!({"Cache": {"Pipeline": {"ops": ops}}}), "Pipeline")
    }

    /// Run operations in order holding the cache write lock throughout, so no
    /// other writer interleaves with them.
    ///
    /// Every operation's inputs are checked before any runs. After that there
    /// is no rollback: one that fails is reported and the rest still run.
    pub fn cache_transaction(&mut self, ops: &[CacheCommand]) -> Result<Vec<CacheReply>> {
        self.cache_replies(
            json!({"Cache": {"Transaction": {"ops": ops}}}),
            "Transaction",
        )
    }

    /// Store a script: operations that run as one transaction, where any
    /// string or value written `$1`, `$2`, … is replaced by that argument at
    /// run time. Loading the same name again replaces it.
    pub fn cache_script_load(&mut self, name: &str, ops: &[CacheCommand]) -> Result<()> {
        self.send(json!({"Cache": {"ScriptLoad": {"name": name, "ops": ops}}}))
            .map(|_| ())
    }

    /// Run a stored script with these arguments as one transaction.
    pub fn cache_script_run(&mut self, name: &str, args: &[&str]) -> Result<Vec<CacheReply>> {
        self.cache_replies(
            json!({"Cache": {"ScriptRun": {"name": name, "args": args}}}),
            "ScriptRun",
        )
    }

    fn cache_replies(&mut self, op: Value, what: &str) -> Result<Vec<CacheReply>> {
        let value: Value = self.cache_json(op, what)?;
        value
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::protocol(format!("malformed {what} payload: no results")))?
            .iter()
            .map(CacheReply::from_wire)
            .collect()
    }

    // -- publish and poll ------------------------------------------------------

    /// Append a message to a channel, returning the id it was given.
    pub fn cache_publish(&mut self, channel: &str, message: &str) -> Result<u64> {
        #[derive(Deserialize)]
        struct Published {
            #[serde(default)]
            id: u64,
        }
        let out: Published = self.cache_json(
            json!({"Cache": {"Publish": {"channel": channel, "message": message}}}),
            "Publish",
        )?;
        Ok(out.id)
    }

    /// The messages after id `after` on these channels. This never blocks.
    ///
    /// A channel name ending in `*` is a pattern, and `__keyspace__*` or
    /// `__keyevent__*` turns keyspace notifications on while it is polled.
    /// `limit` of `None` leaves the cap to the server (100).
    pub fn cache_poll(
        &mut self,
        channels: &[&str],
        after: u64,
        limit: Option<u32>,
    ) -> Result<PolledMessages> {
        let mut body = Map::new();
        body.insert("channels".into(), json!(channels));
        body.insert("after".into(), json!(after));
        if let Some(limit) = limit {
            body.insert("limit".into(), json!(limit));
        }
        self.cache_json(op("Poll", body), "Poll")
    }

    // -- the node and the database ---------------------------------------------

    /// What this node's cache is doing: hits and misses, the memory budget and
    /// the eviction policy. Node-wide, not per database.
    pub fn cache_stats(&mut self) -> Result<CacheStats> {
        self.cache_json(json!({"Cache": "Stats"}), "Stats")
    }

    /// Delete every cache key in this connection's database, across every
    /// namespace, returning how many went.
    pub fn cache_flush_database(&mut self) -> Result<i64> {
        #[derive(Deserialize)]
        struct Cleared {
            #[serde(default)]
            cleared: i64,
        }
        let out: Cleared = self.cache_json(json!({"Cache": "FlushDatabase"}), "FlushDatabase")?;
        Ok(out.cleared)
    }

    /// Delete every expired cache key in this database now, rather than
    /// leaving it to compaction, returning how many went.
    pub fn cache_purge_expired(&mut self) -> Result<i64> {
        #[derive(Deserialize)]
        struct Purged {
            #[serde(default)]
            purged: i64,
        }
        let out: Purged = self.cache_json(json!({"Cache": "PurgeExpired"}), "PurgeExpired")?;
        Ok(out.purged)
    }

    /// The most frequently addressed keys on this node since it started.
    ///
    /// The server answers only when `[cache] hot_keys` is above zero in its
    /// configuration, and refuses otherwise. The payload is returned as the
    /// server sent it.
    pub fn cache_hot_keys(&mut self, limit: Option<usize>) -> Result<Value> {
        let mut body = Map::new();
        if let Some(limit) = limit {
            body.insert("limit".into(), json!(limit));
        }
        self.cache_json(op("HotKeys", body), "HotKeys")
    }

    /// Cap a namespace at `max_keys` live keys and `max_bytes` of values. A
    /// write that would pass either is refused. Both `None` removes the limit.
    pub fn cache_set_namespace_limit(
        &mut self,
        namespace: &str,
        max_keys: Option<u64>,
        max_bytes: Option<u64>,
    ) -> Result<NamespaceLimit> {
        let mut body = Map::new();
        body.insert("namespace".into(), json!(namespace));
        body.insert("max_keys".into(), json!(max_keys));
        body.insert("max_bytes".into(), json!(max_bytes));
        self.cache_json(op("SetNamespaceLimit", body), "SetNamespaceLimit")
    }

    /// A namespace's limit and what it holds now.
    pub fn cache_get_namespace_limit(&mut self, namespace: &str) -> Result<NamespaceLimit> {
        let mut body = Map::new();
        body.insert("namespace".into(), json!(namespace));
        self.cache_json(op("GetNamespaceLimit", body), "GetNamespaceLimit")
    }

    // -- shared --------------------------------------------------------------

    /// Read `CacheValues`: one value per position, null where there is none.
    fn cache_values(&mut self, op: Value, what: &str) -> Result<Vec<Option<Vec<u8>>>> {
        let response = self.send(op)?;
        let data = response.expect("CacheValues", what)?;
        data.as_array()
            .ok_or_else(|| Error::protocol(format!("malformed {what} payload: not a list")))?
            .iter()
            .map(|item| {
                if item.is_null() {
                    Ok(None)
                } else {
                    decode_byte_list(item).map(Some)
                }
            })
            .collect()
    }

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
