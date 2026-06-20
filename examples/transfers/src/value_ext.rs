//! Small helpers for pulling typed fields out of a decoded
//! [`scale_value::Value`] (an event's `fields`).
//!
//! Event fields arrive as a dynamic `Value`. For named-field events like
//! `Assets.Deposited { asset_id, who, amount }` the value is a *named composite*;
//! these helpers look fields up by name and coerce them to the rust types we
//! store, so the handler stays readable.

use scale_value::{Composite, Primitive, Value, ValueDef};

/// Find a named field within a composite value.
pub fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    match &value.value {
        ValueDef::Composite(Composite::Named(fields)) => {
            fields.iter().find(|(n, _)| n == name).map(|(_, v)| v)
        }
        _ => None,
    }
}

/// Coerce a value to `u128` if it is an unsigned/positive integer primitive.
pub fn as_u128(value: &Value) -> Option<u128> {
    match &value.value {
        ValueDef::Primitive(Primitive::U128(n)) => Some(*n),
        ValueDef::Primitive(Primitive::U256(bytes)) => {
            // Fits in u128 only if the high 16 bytes are zero (little-endian).
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

/// Render an account-id-like value (a 32-byte composite of u8s, or a primitive
/// byte sequence) as a `0x…` hex string.
pub fn as_account_hex(value: &Value) -> Option<String> {
    let bytes = collect_bytes(value)?;
    if bytes.is_empty() {
        return None;
    }
    Some(format!("0x{}", hex::encode(bytes)))
}

/// Recursively collect a byte array from a value that is either a composite of
/// u8 primitives (how AccountId32 commonly decodes dynamically) or a primitive
/// byte string.
fn collect_bytes(value: &Value) -> Option<Vec<u8>> {
    match &value.value {
        ValueDef::Composite(Composite::Unnamed(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(byte_of(it)?);
            }
            Some(out)
        }
        ValueDef::Composite(Composite::Named(fields)) => {
            // e.g. AccountId32(([u8; 32])) — a single unnamed inner; recurse into
            // the first field's value.
            fields.first().and_then(|(_, v)| collect_bytes(v))
        }
        _ => None,
    }
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
    use scale_value::Value;

    fn deposited(asset: u128, account_byte: u8, amount: u128) -> Value {
        // AccountId as a 32-byte unnamed composite of u8 primitives.
        let account = Value::unnamed_composite(
            (0..32).map(|_| Value::u128(account_byte as u128)).collect::<Vec<_>>(),
        );
        Value::named_composite(vec![
            ("asset_id".to_string(), Value::u128(asset)),
            ("who".to_string(), account),
            ("amount".to_string(), Value::u128(amount)),
        ])
    }

    #[test]
    fn extracts_named_fields() {
        let v = deposited(2, 0xab, 1_000);
        assert_eq!(as_u128(field(&v, "asset_id").unwrap()), Some(2));
        assert_eq!(as_u128(field(&v, "amount").unwrap()), Some(1_000));
        let who = as_account_hex(field(&v, "who").unwrap()).unwrap();
        assert_eq!(who.len(), 2 + 64, "32 bytes -> 64 hex chars + 0x");
        assert!(who.starts_with("0xabab"));
    }

    #[test]
    fn missing_field_is_none() {
        let v = deposited(1, 1, 1);
        assert!(field(&v, "nonexistent").is_none());
    }

    #[test]
    fn u256_fits_when_high_bytes_zero() {
        let mut bytes = [0u8; 32];
        bytes[0] = 5; // value 5, little-endian
        let v: Value = Value::primitive(Primitive::U256(bytes));
        assert_eq!(as_u128(&v), Some(5));

        bytes[31] = 1; // set a high byte -> doesn't fit
        let v: Value = Value::primitive(Primitive::U256(bytes));
        assert_eq!(as_u128(&v), None);
    }
}
