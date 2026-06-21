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

/// Render an account-id-like value (a 32-byte composite of u8s, possibly wrapped
/// in a newtype layer) as a Substrate **SS58** address (the `5…` form, prefix 42
/// — matching Unit's `SS58Prefix`).
///
/// Returns `None` if the value isn't exactly 32 bytes (so we don't emit a
/// mis-encoded address for an unexpected shape).
pub fn as_account_ss58(value: &Value) -> Option<String> {
    let bytes = collect_bytes(value)?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    // subxt's AccountId32 Display impl produces the SS58-check string with the
    // default Substrate prefix (42), verified compatible with sp_core.
    Some(subxt::utils::AccountId32(arr).to_string())
}

/// Recursively collect a byte array from a value that is either a composite of
/// u8 primitives (how AccountId32 commonly decodes dynamically) or a primitive
/// byte string.
fn collect_bytes(value: &Value) -> Option<Vec<u8>> {
    match &value.value {
        ValueDef::Composite(Composite::Unnamed(items)) => {
            // A newtype wrapper like `AccountId32([u8; 32])` decodes as a single
            // unnamed element that is itself the byte array — unwrap that layer.
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
            // e.g. a single named inner field wrapping the bytes; recurse into it.
            if fields.len() == 1 {
                fields.first().and_then(|(_, v)| collect_bytes(v))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Whether a value is a single byte-sized primitive (used to decide if an
/// unnamed composite is the byte array itself vs a newtype wrapper around it).
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
    use scale_value::Value;

    fn deposited(asset: u128, account_byte: u8, amount: u128) -> Value {
        // AccountId as a 32-byte unnamed composite of u8 primitives.
        let account = Value::unnamed_composite(
            (0..32)
                .map(|_| Value::u128(account_byte as u128))
                .collect::<Vec<_>>(),
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
        let who = as_account_ss58(field(&v, "who").unwrap()).unwrap();
        // SS58 (prefix 42) addresses start with '5' and are ~48 base58 chars.
        assert!(
            who.starts_with('5'),
            "ss58 address should start with 5, got {who}"
        );
        assert!(
            (47..=49).contains(&who.len()),
            "unexpected ss58 length: {}",
            who.len()
        );
        // Round-trips back to the same 32 bytes.
        let decoded: subxt::utils::AccountId32 = who.parse().unwrap();
        assert_eq!(decoded.0, [0xab; 32]);
    }

    #[test]
    fn missing_field_is_none() {
        let v = deposited(1, 1, 1);
        assert!(field(&v, "nonexistent").is_none());
    }

    #[test]
    fn extracts_newtype_wrapped_account() {
        // AccountId32 decodes as Unnamed([ Unnamed([u8; 32]) ]) — a newtype layer
        // around the byte array. as_account_ss58 must unwrap it.
        let inner =
            Value::unnamed_composite((0..32).map(|_| Value::u128(0xcd)).collect::<Vec<_>>());
        let wrapped = Value::unnamed_composite(vec![inner]);
        let ss58 = as_account_ss58(&wrapped).unwrap();
        assert!(ss58.starts_with('5'));
        let decoded: subxt::utils::AccountId32 = ss58.parse().unwrap();
        assert_eq!(decoded.0, [0xcd; 32], "round-trips to the original bytes");
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
