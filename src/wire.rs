//! Byte values on the wire.
//!
//! Cache values, hash fields and the authentication secret travel as JSON
//! arrays of small integers, not as base64 or as text. The server stores opaque
//! bytes, so a client that spoke only strings would corrupt every value that is
//! not valid UTF-8.

use serde_json::{Number, Value};

use crate::error::{Error, Result};

/// Encode bytes the way the server expects them.
pub(crate) fn byte_list(bytes: &[u8]) -> Value {
    Value::Array(
        bytes
            .iter()
            .map(|b| Value::Number(Number::from(*b)))
            .collect(),
    )
}

/// Encode several byte strings, refusing an empty list here so the error names
/// the argument rather than arriving from the server.
pub(crate) fn byte_lists(values: &[impl AsRef<[u8]>], name: &str) -> Result<Value> {
    if values.is_empty() {
        return Err(Error::invalid(format!("`{name}` must not be empty")));
    }
    Ok(Value::Array(
        values.iter().map(|v| byte_list(v.as_ref())).collect(),
    ))
}

/// Decode a JSON array of small integers back into bytes.
///
/// A value outside `0..=255` is not a byte: it is refused rather than masked,
/// because masking would quietly corrupt the payload.
pub(crate) fn decode_byte_list(value: &Value) -> Result<Vec<u8>> {
    let Some(items) = value.as_array() else {
        return Err(Error::protocol(format!(
            "expected an array of bytes, got {}",
            kind_of(value)
        )));
    };
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let n = item
            .as_u64()
            .filter(|n| *n <= u8::MAX as u64)
            .ok_or_else(|| {
                Error::protocol(format!(
                    "byte {index} of the value is {item}, which is not a number in 0..=255"
                ))
            })?;
        out.push(n as u8);
    }
    Ok(out)
}

/// Decode an array of byte arrays.
pub(crate) fn decode_byte_lists(value: &Value) -> Result<Vec<Vec<u8>>> {
    let Some(items) = value.as_array() else {
        return Err(Error::protocol(format!(
            "expected an array of byte arrays, got {}",
            kind_of(value)
        )));
    };
    items.iter().map(decode_byte_list).collect()
}

/// Decode a `[field, value]` pair.
pub(crate) fn decode_pair(value: &Value) -> Result<(Vec<u8>, Vec<u8>)> {
    let items = value
        .as_array()
        .filter(|items| items.len() == 2)
        .ok_or_else(|| Error::protocol("expected a [field, value] pair".to_string()))?;
    Ok((decode_byte_list(&items[0])?, decode_byte_list(&items[1])?))
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bytes_travel_as_numbers_not_as_base64() {
        assert_eq!(byte_list(b"hi"), json!([104, 105]));
        assert_eq!(byte_list(&[]), json!([]));
        assert_eq!(byte_list(&[0x00, 0xff]), json!([0, 255]));
    }

    #[test]
    fn a_value_round_trips_byte_for_byte() {
        let original: Vec<u8> = (0..=255).collect();
        let decoded = decode_byte_list(&byte_list(&original)).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn a_non_byte_element_is_refused_rather_than_masked() {
        let err = decode_byte_list(&json!([1, 256])).unwrap_err();
        assert!(err.message.contains("0..=255"), "{}", err.message);
        assert!(decode_byte_list(&json!([1, -1])).is_err());
        assert!(decode_byte_list(&json!("hi")).is_err());
    }

    #[test]
    fn an_empty_list_of_values_is_refused_by_name() {
        let empty: [&[u8]; 0] = [];
        let err = byte_lists(&empty, "members").unwrap_err();
        assert_eq!(err.kind, crate::ErrorKind::InvalidArgument);
        assert!(err.message.contains("members"), "{}", err.message);
    }

    #[test]
    fn pairs_need_exactly_two_halves() {
        let (field, value) = decode_pair(&json!([[97], [98]])).unwrap();
        assert_eq!((field, value), (vec![97], vec![98]));
        assert!(decode_pair(&json!([[97]])).is_err());
    }
}
