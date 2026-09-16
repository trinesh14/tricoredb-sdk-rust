//! What the server answered, and how it produced it.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{Error, Result};

/// A decoded `RESPONSE` frame.
///
/// [`Response::data`] is the payload of whichever variant the operation
/// produced, and [`Response::kind`] names that variant — `"Json"`,
/// `"CacheValue"`, `"Rows"`, `"Documents"`, `"Message"`, `"Toon"` or
/// `"Empty"`. The typed methods on [`crate::Client`] decode this for you; it is
/// public so an operation this crate has no method for is still reachable
/// through [`crate::Client::request`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Response {
    /// The `request_id` the server echoed back.
    pub request_id: String,
    /// The server's status. `"ok"` on success; any other value is reported as
    /// an error rather than returned.
    pub status: String,
    /// The name of the data variant, or an empty string when there was no data.
    pub kind: String,
    /// That variant's payload, still as JSON.
    pub data: Value,
    /// Which route served the request.
    pub route: Option<String>,
    /// How long the server took.
    pub elapsed_ms: Option<i64>,
    /// Non-fatal warnings. Not decorative: a broadcast DDL that could not reach
    /// every shard reports it here while the status is still `"ok"`.
    pub warnings: Vec<String>,
    /// The server's machine-readable failure code, when it failed.
    pub error_code: Option<String>,
    /// On a `not_leader` refusal, the leader's `host:port` when the cluster
    /// knows one.
    pub leader_hint: Option<String>,
}

impl Response {
    /// Decode the payload into a type of your own.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_value(self.data.clone())
            .map_err(|e| Error::protocol(format!("unexpected {} payload: {e}", self.describe())))
    }

    /// The payload, when the variant is the expected one.
    pub(crate) fn expect(&self, kind: &str, what: &str) -> Result<&Value> {
        if self.kind == kind {
            Ok(&self.data)
        } else {
            Err(Error::protocol(format!(
                "expected {kind} from {what}, got {}",
                self.describe()
            )))
        }
    }

    /// Decode a `Json` payload, which is what most operations answer with.
    pub(crate) fn decode<T: DeserializeOwned>(&self, what: &str) -> Result<T> {
        let data = self.expect("Json", what)?;
        serde_json::from_value(data.clone())
            .map_err(|e| Error::protocol(format!("malformed {what} payload: {e}")))
    }

    fn describe(&self) -> String {
        if self.kind.is_empty() {
            "no data".to_string()
        } else {
            self.kind.clone()
        }
    }

    /// The message to report when the status was not `ok`.
    pub(crate) fn failure_message(&self) -> String {
        match self.kind.as_str() {
            "Message" | "Toon" => match self.data.as_str() {
                Some(text) if !text.is_empty() => text.to_string(),
                _ => self.data.to_string(),
            },
            "Json" => self.data.to_string(),
            _ if !self.status.is_empty() => format!("the server answered `{}`", self.status),
            _ => "the request failed".to_string(),
        }
    }
}

/// Unwrap an externally tagged `data` field — `{"Variant": payload}`, or the
/// bare string `"Empty"` — into the variant's name and its payload.
pub(crate) fn decode_variant(data: Value) -> (String, Value) {
    match data {
        Value::Null => (String::new(), Value::Null),
        // The unit variants ("Empty") arrive as a bare string.
        Value::String(name) => (name, Value::Null),
        Value::Object(map) => match map.into_iter().next() {
            Some((name, payload)) => (name, payload),
            None => (String::new(), Value::Null),
        },
        other => (String::new(), other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_externally_tagged_variant_splits_into_name_and_payload() {
        let (kind, data) = decode_variant(json!({"Rows": {"columns": ["a"], "rows": []}}));
        assert_eq!(kind, "Rows");
        assert_eq!(data["columns"][0], "a");
    }

    #[test]
    fn a_unit_variant_is_a_bare_string() {
        let (kind, data) = decode_variant(json!("Empty"));
        assert_eq!(kind, "Empty");
        assert_eq!(data, Value::Null);
    }

    #[test]
    fn absent_data_is_no_kind_at_all() {
        let (kind, _) = decode_variant(Value::Null);
        assert_eq!(kind, "");
    }

    #[test]
    fn the_wrong_variant_is_named_in_the_error() {
        let response = Response {
            request_id: "r1".into(),
            status: "ok".into(),
            kind: "Message".into(),
            data: json!("hi"),
            route: None,
            elapsed_ms: None,
            warnings: vec![],
            error_code: None,
            leader_hint: None,
        };
        let err = response.expect("Rows", "query").unwrap_err();
        assert!(err.message.contains("expected Rows"), "{}", err.message);
        assert!(err.message.contains("Message"), "{}", err.message);
    }

    #[test]
    fn a_failure_reports_the_servers_own_message() {
        let mut response = Response {
            request_id: "r1".into(),
            status: "error".into(),
            kind: "Message".into(),
            data: json!("no such table"),
            route: None,
            elapsed_ms: None,
            warnings: vec![],
            error_code: Some("sql.unknown_table".into()),
            leader_hint: None,
        };
        assert_eq!(response.failure_message(), "no such table");
        response.kind = "Empty".into();
        assert!(response.failure_message().contains("error"));
    }
}
