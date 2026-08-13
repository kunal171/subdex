//! Safe field-extraction helpers for handler code.
//!
//! A decoded event's `fields` arrive as a dynamic [`scale_value::Value`]. For
//! named-field events like `Assets.Deposited { asset_id, who, amount }` that is a
//! *named composite*; these helpers look a field up by name and coerce it to the
//! Rust type you want to store, so a handler reads as
//!
//! ```ignore
//! let who = field_account_ss58(&ev.fields, "who", 42);
//! let amount = field_u128(&ev.fields, "amount");
//! require_fields(&ev.fields, &["asset_id", "who", "amount"])?;
//! ```
//!
//! rather than open-coding the same `match` on `ValueDef` in every handler. This
//! is the Rust equivalent of the Subsquid indexer's `utils/value.ts` toolkit,
//! promoted into the framework so it isn't copy-pasted per example.
//!
//! Every reader is **total** — it returns `None` (or a caught error for
//! [`require_fields`]) for a shape it doesn't recognize, never panics. A handler
//! decides whether a missing/odd field is a hard error or a tolerated `NULL`.

use scale_value::{Composite, Primitive, Value, ValueDef};

/// Find a named field within a named-composite value.
///
/// Returns `None` if `value` isn't a named composite or has no field `name`.
pub fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    match &value.value {
        ValueDef::Composite(Composite::Named(fields)) => {
            fields.iter().find(|(n, _)| n == name).map(|(_, v)| v)
        }
        _ => None,
    }
}

/// Read a named field and coerce it to `u128` (see [`as_u128`]).
pub fn field_u128(value: &Value, name: &str) -> Option<u128> {
    field(value, name).and_then(as_u128)
}

/// Read a named field as a decimal **string** (`u128` rendered as text).
///
/// Chain balances routinely exceed `i64`, and Postgres `NUMERIC` is bound from a
/// decimal string — this is the shape [subdex-codegen]'s upserts expect for a
/// `BigInt` column.
///
/// [subdex-codegen]: https://github.com/kunal171/subdex
pub fn field_bigint(value: &Value, name: &str) -> Option<String> {
    field_u128(value, name).map(|n| n.to_string())
}

/// Read a named field as a `bool` (a boolean primitive).
pub fn field_bool(value: &Value, name: &str) -> Option<bool> {
    match field(value, name)?.value {
        ValueDef::Primitive(Primitive::Bool(b)) => Some(b),
        _ => None,
    }
}

/// Read a named field as a UTF-8 `String` (a string primitive).
pub fn field_str(value: &Value, name: &str) -> Option<String> {
    match &field(value, name)?.value {
        ValueDef::Primitive(Primitive::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Read a named byte-array field as a `0x`-prefixed hex string.
///
/// Handles the two common byte shapes: an unnamed composite of `u8` primitives
/// (how a `[u8; N]` / `Vec<u8>` often decodes dynamically), possibly wrapped in a
/// newtype layer. Use this for opaque bytes (a tx hash, a raw id) — hex is
/// **NUL-safe**, unlike decoding raw bytes as UTF-8 into a text column.
pub fn field_hex(value: &Value, name: &str) -> Option<String> {
    let bytes = collect_bytes(field(value, name)?)?;
    Some(format!("0x{}", hex::encode(bytes)))
}

/// Read a named account-id field as a Substrate **SS58** address for `prefix`
/// (42 = generic Substrate `5…`, 0 = Polkadot `1…`). See [`as_account_ss58`].
pub fn field_account_ss58(value: &Value, name: &str, prefix: u16) -> Option<String> {
    field(value, name).and_then(|v| as_account_ss58(v, prefix))
}

/// Assert that every name in `names` is present on the named composite `value`.
///
/// Returns the **first** missing field name as `Err`, so a handler can turn an
/// unexpected event shape into a typed error instead of silently writing NULLs.
pub fn require_fields(value: &Value, names: &[&str]) -> Result<(), MissingField> {
    for &name in names {
        if field(value, name).is_none() {
            return Err(MissingField(name.to_string()));
        }
    }
    Ok(())
}

/// The name of the first required field that was absent (see [`require_fields`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingField(pub String);

impl std::fmt::Display for MissingField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "missing required field `{}`", self.0)
    }
}

impl std::error::Error for MissingField {}

/// Coerce a value to `u128` if it is an unsigned/positive integer primitive.
///
/// Accepts `U128`, a `U256` whose high 16 bytes are zero (little-endian), and a
/// non-negative `I128`.
pub fn as_u128(value: &Value) -> Option<u128> {
    match &value.value {
        ValueDef::Primitive(Primitive::U128(n)) => Some(*n),
        ValueDef::Primitive(Primitive::U256(bytes)) => {
            if bytes[16..].iter().all(|b| *b == 0) {
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&bytes[..16]);
                Some(u128::from_le_bytes(buf))
            } else {
                None
            }
        }
        ValueDef::Primitive(Primitive::I128(n)) if *n >= 0 => Some(*n as u128),
        _ => None,
    }
}

/// Render an account-id-like value (a 32-byte composite of `u8`s, possibly
/// wrapped in a newtype layer) as an SS58 address for network `prefix`.
///
/// Returns `None` if the value isn't exactly 32 bytes (so a mis-shaped value
/// yields no address rather than a wrong one).
pub fn as_account_ss58(value: &Value, prefix: u16) -> Option<String> {
    let bytes = collect_bytes(value)?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(crate::ss58::encode(&arr, prefix))
}

/// Recursively collect a byte array from a value that is a composite of `u8`
/// primitives (how a byte array commonly decodes dynamically), unwrapping a
/// single newtype layer (e.g. `AccountId32([u8; 32])`).
fn collect_bytes(value: &Value) -> Option<Vec<u8>> {
    match &value.value {
        ValueDef::Composite(Composite::Unnamed(items)) => {
            if items.len() == 1 && !is_byte(&items[0]) {
                return collect_bytes(&items[0]);
            }
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(byte_of(it)?);
            }
            Some(out)
        }
        ValueDef::Composite(Composite::Named(fields)) => {
            if fields.len() == 1 {
                fields.first().and_then(|(_, v)| collect_bytes(v))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Whether a value is a single byte-sized primitive (used to tell a byte array
/// apart from a newtype wrapper around one).
fn is_byte(value: &Value) -> bool {
    matches!(&value.value, ValueDef::Primitive(Primitive::U128(n)) if *n <= u8::MAX as u128)
}

/// Extract a single byte from a small unsigned primitive value.
fn byte_of(value: &Value) -> Option<u8> {
    match &value.value {
        ValueDef::Primitive(Primitive::U128(n)) if *n <= u8::MAX as u128 => Some(*n as u8),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(byte: u8) -> Value {
        Value::unnamed_composite(
            (0..32)
                .map(|_| Value::u128(byte as u128))
                .collect::<Vec<_>>(),
        )
    }

    fn deposited() -> Value {
        Value::named_composite(vec![
            ("asset_id".to_string(), Value::u128(7)),
            ("who".to_string(), account(0xab)),
            ("amount".to_string(), Value::u128(1_000_000_000_000)),
            ("ok".to_string(), Value::bool(true)),
        ])
    }

    #[test]
    fn typed_field_readers() {
        let v = deposited();
        assert_eq!(field_u128(&v, "asset_id"), Some(7));
        assert_eq!(
            field_bigint(&v, "amount"),
            Some("1000000000000".to_string())
        );
        assert_eq!(field_bool(&v, "ok"), Some(true));
        // Missing / wrong-type fields are None, never a panic.
        assert_eq!(field_u128(&v, "nope"), None);
        assert_eq!(field_bool(&v, "amount"), None);
    }

    #[test]
    fn account_field_is_ss58_with_prefix() {
        let v = deposited();
        let sub = field_account_ss58(&v, "who", 42).unwrap();
        let dot = field_account_ss58(&v, "who", 0).unwrap();
        assert!(sub.starts_with('5'), "substrate prefix: {sub}");
        assert!(dot.starts_with('1'), "polkadot prefix: {dot}");
        assert_ne!(sub, dot);
    }

    #[test]
    fn hex_field_is_nul_safe() {
        // A 4-byte opaque value → 0x-prefixed hex (never a NUL-bearing string).
        let bytes = Value::unnamed_composite(
            [0x00u8, 0xde, 0xad, 0xff]
                .iter()
                .map(|b| Value::u128(*b as u128))
                .collect::<Vec<_>>(),
        );
        let v = Value::named_composite(vec![("tx".to_string(), bytes)]);
        assert_eq!(field_hex(&v, "tx"), Some("0x00deadff".to_string()));
    }

    #[test]
    fn require_fields_reports_first_missing() {
        let v = deposited();
        assert!(require_fields(&v, &["asset_id", "who", "amount"]).is_ok());
        let err = require_fields(&v, &["asset_id", "missing_one", "who"]).unwrap_err();
        assert_eq!(err.0, "missing_one");
        assert_eq!(err.to_string(), "missing required field `missing_one`");
    }

    #[test]
    fn u256_fits_only_when_high_bytes_zero() {
        let mut bytes = [0u8; 32];
        bytes[0] = 5;
        assert_eq!(as_u128(&Value::primitive(Primitive::U256(bytes))), Some(5));
        bytes[31] = 1;
        assert_eq!(as_u128(&Value::primitive(Primitive::U256(bytes))), None);
    }

    #[test]
    fn newtype_wrapped_account_unwraps() {
        // AccountId32 often decodes as Unnamed([ Unnamed([u8; 32]) ]).
        let wrapped = Value::unnamed_composite(vec![account(0xcd)]);
        let ss58 = as_account_ss58(&wrapped, 42).unwrap();
        assert!(ss58.starts_with('5'));
    }
}
