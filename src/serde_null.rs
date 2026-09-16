//! Reading a `null` where a collection is expected.
//!
//! The server writes `"properties": null` for a node with no properties, and
//! `"metadata": null` for a vector with none. Serde's `default` covers a field
//! that is *absent*, not one that is present and null, so without this a node
//! with no properties would be a protocol error rather than a node.

use serde::{Deserialize, Deserializer};

/// Deserialize a value, reading `null` as the type's default.
pub(crate) fn or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Map, Value};

    #[derive(Debug, Deserialize)]
    struct Node {
        #[serde(default, deserialize_with = "or_default")]
        labels: Vec<String>,
        #[serde(default, deserialize_with = "or_default")]
        properties: Map<String, Value>,
    }

    #[test]
    fn null_absent_and_present_all_decode() {
        let explicit_null: Node =
            serde_json::from_value(json!({"labels": null, "properties": null})).unwrap();
        assert!(explicit_null.labels.is_empty());
        assert!(explicit_null.properties.is_empty());

        let absent: Node = serde_json::from_value(json!({})).unwrap();
        assert!(absent.labels.is_empty());

        let present: Node =
            serde_json::from_value(json!({"labels": ["User"], "properties": {"name": "ada"}}))
                .unwrap();
        assert_eq!(present.labels, vec!["User"]);
        assert_eq!(present.properties["name"], "ada");
    }
}
