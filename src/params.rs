//! SQL parameter values and their wire encoding.
//!
//! Parameters travel as JSON scalars bound by the server. The encoding rules:
//!
//! | Rust value | Sent as |
//! | --- | --- |
//! | `None` / [`Param::Null`] | `null` |
//! | `bool` | `true` / `false` |
//! | any integer up to `i128`/`u128` | an exact JSON number (never via `f64`) |
//! | `f32` / `f64` | a JSON number; NaN and infinities are refused |
//! | [`Param::decimal`] | a plain-digit string with no exponent |
//! | `Vec<u8>` / `&[u8]` / [`Param::bytes`] | `"0x"` + lowercase hex |
//! | `&str` / `String` | a JSON string (timestamps, UUIDs, JSON text) |
//!
//! A value wider than `i64` binds as a decimal on the server.

use serde::ser::{Serialize, Serializer};

use crate::error::{Error, Result};

/// One SQL parameter.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Param {
    /// SQL `NULL`.
    Null,
    /// A boolean.
    Bool(bool),
    /// A signed integer of any width up to 128 bits.
    Int(i128),
    /// An unsigned integer of any width up to 128 bits.
    UInt(u128),
    /// A finite floating-point number.
    Float(f64),
    /// An exact decimal as plain digits (validated by [`Param::decimal`]).
    Decimal(String),
    /// Raw bytes, sent as `0x` hex text.
    Bytes(Vec<u8>),
    /// Text.
    Text(String),
}

impl Param {
    /// An exact decimal from its text form, such as `"-12.500"`.
    ///
    /// The text must be an optional sign, digits, and an optional fractional
    /// part. Exponents are refused because the server would read `1.5E+3` as a
    /// `DOUBLE`. Scale and precision limits (18 and 38) are enforced by the server.
    pub fn decimal(text: impl Into<String>) -> Result<Param> {
        let text = text.into();
        validate_decimal(&text)?;
        Ok(Param::Decimal(text))
    }

    /// Raw bytes for a `BLOB` column.
    pub fn bytes(bytes: impl Into<Vec<u8>>) -> Param {
        Param::Bytes(bytes.into())
    }

    /// Text.
    pub fn text(text: impl Into<String>) -> Param {
        Param::Text(text.into())
    }

    pub(crate) fn validate(&self, index: usize) -> Result<()> {
        match self {
            Param::Float(f) if !f.is_finite() => Err(Error::invalid(format!(
                "parameter {} is {f}, which has no SQL representation; only finite floats can be bound",
                index + 1
            ))),
            Param::Decimal(d) => validate_decimal(d),
            _ => Ok(()),
        }
    }
}

fn validate_decimal(text: &str) -> Result<()> {
    let body = text.strip_prefix(['-', '+']).unwrap_or(text);
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (body, None),
    };
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let ok = digits(int)
        && frac.is_none_or(digits)
        && (!int.is_empty() || frac.is_some_and(|f| !f.is_empty()))
        && !(int.is_empty() && frac.is_none());
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "`{text}` is not a plain decimal: use digits with an optional sign and fractional part, and no exponent"
        )))
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    s
}

impl Serialize for Param {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Param::Null => s.serialize_none(),
            Param::Bool(b) => s.serialize_bool(*b),
            Param::Int(i) => match i64::try_from(*i) {
                Ok(v) => s.serialize_i64(v),
                Err(_) => s.serialize_i128(*i),
            },
            Param::UInt(u) => match u64::try_from(*u) {
                Ok(v) => s.serialize_u64(v),
                Err(_) => s.serialize_u128(*u),
            },
            Param::Float(f) => s.serialize_f64(*f),
            Param::Decimal(d) => s.serialize_str(d),
            Param::Bytes(b) => s.serialize_str(&hex(b)),
            Param::Text(t) => s.serialize_str(t),
        }
    }
}

macro_rules! from_int {
    ($variant:ident, $wide:ty, $($t:ty),*) => {
        $(impl From<$t> for Param {
            fn from(v: $t) -> Self {
                Param::$variant(<$wide>::from(v))
            }
        })*
    };
}

from_int!(Int, i128, i8, i16, i32, i64, i128);
from_int!(UInt, u128, u8, u16, u32, u64, u128);

impl From<isize> for Param {
    fn from(v: isize) -> Self {
        Param::Int(v as i128)
    }
}

impl From<usize> for Param {
    fn from(v: usize) -> Self {
        Param::UInt(v as u128)
    }
}

impl From<bool> for Param {
    fn from(v: bool) -> Self {
        Param::Bool(v)
    }
}

impl From<f64> for Param {
    fn from(v: f64) -> Self {
        Param::Float(v)
    }
}

impl From<f32> for Param {
    fn from(v: f32) -> Self {
        Param::Float(f64::from(v))
    }
}

impl From<&str> for Param {
    fn from(v: &str) -> Self {
        Param::Text(v.to_string())
    }
}

impl From<String> for Param {
    fn from(v: String) -> Self {
        Param::Text(v)
    }
}

impl From<&String> for Param {
    fn from(v: &String) -> Self {
        Param::Text(v.clone())
    }
}

impl From<Vec<u8>> for Param {
    fn from(v: Vec<u8>) -> Self {
        Param::Bytes(v)
    }
}

impl From<&[u8]> for Param {
    fn from(v: &[u8]) -> Self {
        Param::Bytes(v.to_vec())
    }
}

impl<T: Into<Param>> From<Option<T>> for Param {
    fn from(v: Option<T>) -> Self {
        v.map_or(Param::Null, Into::into)
    }
}

/// Build a `Vec<Param>` from heterogeneous values.
///
/// ```
/// use tricoredb::{params, Param};
/// let p = params![1, "ada", None::<i64>, vec![0xffu8]];
/// assert_eq!(p[1], Param::Text("ada".into()));
/// ```
#[macro_export]
macro_rules! params {
    () => { ::std::vec::Vec::<$crate::Param>::new() };
    ($($v:expr),+ $(,)?) => { ::std::vec![$($crate::Param::from($v)),+] };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(p: &[Param]) -> String {
        serde_json::to_string(p).unwrap()
    }

    #[test]
    fn scalars_encode_as_json_scalars() {
        assert_eq!(
            wire(&params![None::<i32>, true, 42, -7i64, 1.5, "O'Brien"]),
            r#"[null,true,42,-7,1.5,"O'Brien"]"#
        );
    }

    #[test]
    fn wide_integers_are_exact_json_numbers() {
        assert_eq!(wire(&params![i64::MAX]), "[9223372036854775807]");
        assert_eq!(wire(&params![u64::MAX]), "[18446744073709551615]");
        assert_eq!(
            wire(&params![170141183460469231731687303715884105727i128]),
            "[170141183460469231731687303715884105727]"
        );
        assert_eq!(
            wire(&params![u128::MAX]),
            "[340282366920938463463374607431768211455]"
        );
        assert_eq!(
            wire(&params![-9223372036854775809i128]),
            "[-9223372036854775809]"
        );
    }

    #[test]
    fn decimals_are_plain_text_without_an_exponent() {
        let d = Param::decimal("0.123456789012345678").unwrap();
        assert_eq!(wire(&[d]), r#"["0.123456789012345678"]"#);
        assert!(Param::decimal("-1.50").is_ok());
        assert!(Param::decimal("+10").is_ok());
        assert!(Param::decimal(".5").is_ok());
        assert!(Param::decimal("5.").is_ok());
        for bad in ["1.5E+3", "1e10", "", "-", ".", "1.2.3", "12a", "NaN", " 1"] {
            assert!(Param::decimal(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn bytes_are_lowercase_hex_text() {
        assert_eq!(
            wire(&[Param::bytes(vec![0x00, 0xab, 0xff, 0x10])]),
            r#"["0x00abff10"]"#
        );
        assert_eq!(wire(&params![Vec::<u8>::new()]), r#"["0x"]"#);
        let invalid_utf8: &[u8] = &[0xc3, 0x28];
        assert_eq!(wire(&params![invalid_utf8]), r#"["0xc328"]"#);
    }

    #[test]
    fn non_finite_floats_are_refused_by_name() {
        for f in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = Param::Float(f).validate(2).unwrap_err();
            assert_eq!(err.kind, crate::ErrorKind::InvalidArgument);
            assert!(err.message.contains("parameter 3"), "{}", err.message);
        }
        assert!(Param::Float(1.0).validate(0).is_ok());
    }

    #[test]
    fn f32_widens_without_changing_the_value() {
        assert_eq!(wire(&params![0.5f32]), "[0.5]");
    }
}
