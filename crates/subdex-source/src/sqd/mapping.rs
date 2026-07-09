//! Mapping from the SQD portal's JSON block shape into the framework's
//! chain-agnostic [`Block`] model.
//!
//! The portal returns blocks as JSON (one per line), with **already-decoded**
//! event/call arguments as JSON. subdex's model holds decoded values as
//! [`scale_value::Value`], so [`json_to_value`] bridges the two. See the note on
//! [`json_to_value`] for why this is *equivalent* to, but not byte-identical
//! with, the RPC source's SCALE-decoded values.

use scale_value::Value;
use serde::Deserialize;
use subdex_core::{Block, BlockId, Event, Extrinsic};

/// One block as returned by the SQD portal (`.../stream`). Only the fields we map
/// are declared; unknown fields are ignored. Selectors control which of
/// `events` / `calls` are populated (see [`crate::DataSelection`]).
#[derive(Debug, Deserialize)]
pub(crate) struct PortalBlock {
    pub header: PortalHeader,
    #[serde(default)]
    pub events: Vec<PortalEvent>,
    #[serde(default)]
    pub calls: Vec<PortalCall>,
}

/// Portal block header. `timestamp` is ms since epoch (already extracted by the
/// portal — no `Timestamp.set` extrinsic parse needed, unlike the RPC source).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PortalHeader {
    pub number: u32,
    pub hash: String,
    pub parent_hash: String,
    #[serde(default)]
    pub spec_version: u32,
    #[serde(default)]
    pub timestamp: Option<u64>,
}

/// Portal event. `args` is decoded JSON (object or array). `phase` distinguishes
/// apply-extrinsic events from init/finalization ones; we surface `extrinsicIndex`
/// directly. The portal does **not** carry a per-block event index, so we derive
/// it from the event's position in the block's array (matching the RPC source).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PortalEvent {
    #[serde(default)]
    pub extrinsic_index: Option<u32>,
    /// Fully-qualified `Pallet.Event`, e.g. `"Balances.Transfer"`.
    pub name: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// Portal call (extrinsic). `args` is decoded JSON; `success` is provided.
///
/// `extrinsicIndex` is only present when the `extrinsic` field is also selected;
/// with just the `call` selector the portal omits it, so it's optional and we
/// fall back to the call's position in the block (matching the RPC source).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PortalCall {
    #[serde(default)]
    pub extrinsic_index: Option<u32>,
    /// Fully-qualified `Pallet.call`, e.g. `"Balances.transfer_keep_alive"`.
    pub name: String,
    #[serde(default = "default_true")]
    pub success: bool,
    #[serde(default)]
    pub args: serde_json::Value,
    /// Present when the call is signed; carries the origin/signer address.
    #[serde(default)]
    pub origin: Option<serde_json::Value>,
}

fn default_true() -> bool {
    true
}

/// Split a portal's `"Pallet.Item"` name into `(pallet, item)`. If there's no
/// dot, the whole string is the item and the pallet is empty.
fn split_qualified(name: &str) -> (String, String) {
    match name.split_once('.') {
        Some((pallet, item)) => (pallet.to_string(), item.to_string()),
        None => (String::new(), name.to_string()),
    }
}

/// Bridge decoded JSON (from the portal) into a [`scale_value::Value`] so handlers
/// see the same *type* they get from the RPC source.
///
/// **Equivalence, not identity.** The RPC source decodes SCALE bytes against
/// subxt metadata, producing `Value`s whose composite/variant structure mirrors
/// the on-chain type (named fields, enum variants, byte arrays as `[u8]`). The
/// portal pre-decodes to JSON, losing that type information, so this bridge maps:
/// - object → named composite, array → unnamed composite,
/// - integer → u128 (or i128 if negative), float → u128 (truncated; chain values
///   are integers — floats shouldn't occur), string → string, bool → bool,
///   null → unit.
///
/// Scalars and simple structs match the RPC shape; enums and byte arrays are
/// represented differently (a JSON string/object vs a SCALE variant/`[u8]`). A
/// handler reading a scalar field by name works against both; one that pattern-
/// matches on `Value` variant structure may see a different shape. This is an
/// inherent property of sourcing pre-decoded data.
pub(crate) fn json_to_value(json: &serde_json::Value) -> Value {
    use serde_json::Value as J;
    match json {
        J::Null => Value::unnamed_composite(Vec::new()),
        J::Bool(b) => Value::bool(*b),
        J::Number(n) => {
            if let Some(u) = n.as_u64() {
                Value::u128(u as u128)
            } else if let Some(i) = n.as_i64() {
                Value::i128(i as i128)
            } else {
                // Non-integer (float) — chain values are integers, but be safe.
                Value::u128(n.as_f64().unwrap_or(0.0) as u128)
            }
        }
        J::String(s) => Value::string(s.clone()),
        J::Array(items) => {
            Value::unnamed_composite(items.iter().map(json_to_value).collect::<Vec<_>>())
        }
        J::Object(map) => Value::named_composite(
            map.iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect::<Vec<_>>(),
        ),
    }
}

/// Best-effort signer extraction from a call's `origin` JSON. Substrate signed
/// origins are commonly `{ "__kind": "Signed", "value": "0x…" }` or nested under
/// a `value` key. We pull the first **address-looking** string (starts with
/// `0x`), skipping variant tags like `"Signed"`.
///
/// When that address is a 32-byte hex account, it's rendered as an SS58 address
/// with `ss58_prefix` — consistent with the RPC source. A non-account or
/// non-hex string is kept as-is; anything else falls back to the first string.
fn extract_signer(origin: &Option<serde_json::Value>, ss58_prefix: u16) -> (bool, Option<String>) {
    let Some(origin) = origin else {
        return (false, None);
    };
    // Origin present ⇒ signed. Try for an address; fall back to any string.
    let raw = first_address(origin).or_else(|| first_string(origin));
    let addr = raw.map(|s| ss58_from_hex_account(&s, ss58_prefix).unwrap_or(s));
    (true, addr)
}

/// If `s` is a `0x`-prefixed 32-byte hex account, return its SS58 encoding;
/// otherwise `None` (caller keeps the original string).
fn ss58_from_hex_account(s: &str, prefix: u16) -> Option<String> {
    let hex = s.strip_prefix("0x")?;
    let bytes = hex::decode(hex).ok()?;
    let account: [u8; 32] = bytes.try_into().ok()?;
    Some(crate::ss58::encode(&account, prefix))
}

/// Find the first `0x…`-prefixed string in a JSON value (depth-first) — the
/// address in a signed origin, without picking up the `"Signed"` variant tag.
fn first_address(json: &serde_json::Value) -> Option<String> {
    use serde_json::Value as J;
    match json {
        J::String(s) if s.starts_with("0x") => Some(s.clone()),
        J::Array(items) => items.iter().find_map(first_address),
        J::Object(map) => map.values().find_map(first_address),
        _ => None,
    }
}

/// Find the first string in a JSON value (depth-first), as a fallback when no
/// `0x…` address is present.
fn first_string(json: &serde_json::Value) -> Option<String> {
    use serde_json::Value as J;
    match json {
        J::String(s) => Some(s.clone()),
        J::Array(items) => items.iter().find_map(first_string),
        J::Object(map) => map.values().find_map(first_string),
        _ => None,
    }
}

/// Convert one [`PortalBlock`] into the framework's [`Block`]. `ss58_prefix`
/// renders a signed call's account origin as an SS58 address (default 42).
pub(crate) fn map_block(pb: PortalBlock, ss58_prefix: u16) -> Block {
    let events = pb
        .events
        .into_iter()
        .enumerate()
        .map(|(i, e)| {
            let (pallet, name) = split_qualified(&e.name);
            Event {
                // The portal carries no per-block event index; derive it from
                // position (events are delivered in block order).
                index: i as u32,
                pallet,
                name,
                fields: json_to_value(&e.args),
                extrinsic_index: e.extrinsic_index,
            }
        })
        .collect();

    let extrinsics = pb
        .calls
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let (pallet, call) = split_qualified(&c.name);
            let (signed, signer) = extract_signer(&c.origin, ss58_prefix);
            Extrinsic {
                // Prefer the portal's extrinsicIndex when present; else derive
                // from position (calls are delivered in block order).
                index: c.extrinsic_index.unwrap_or(i as u32),
                pallet,
                call,
                args: json_to_value(&c.args),
                signed,
                signer,
                success: c.success,
            }
        })
        .collect();

    Block {
        id: BlockId {
            number: pb.header.number,
            hash: pb.header.hash,
        },
        parent_hash: pb.header.parent_hash,
        timestamp: pb.header.timestamp,
        spec_version: pb.header.spec_version,
        // The portal serves only finalized blocks.
        finalized: true,
        extrinsics,
        events,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scale_value::{Primitive, ValueDef};

    #[test]
    fn json_scalars_bridge_to_value() {
        assert!(matches!(
            json_to_value(&serde_json::json!(true)).value,
            ValueDef::Primitive(Primitive::Bool(true))
        ));
        assert!(matches!(
            json_to_value(&serde_json::json!(42)).value,
            ValueDef::Primitive(Primitive::U128(42))
        ));
        assert!(matches!(
            json_to_value(&serde_json::json!(-5)).value,
            ValueDef::Primitive(Primitive::I128(-5))
        ));
        match json_to_value(&serde_json::json!("0xabcd")).value {
            ValueDef::Primitive(Primitive::String(s)) => assert_eq!(s, "0xabcd"),
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn json_object_becomes_named_composite() {
        let v = json_to_value(&serde_json::json!({"from": "0x01", "amount": 100}));
        match v.value {
            ValueDef::Composite(scale_value::Composite::Named(fields)) => {
                let names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
                assert!(names.contains(&"from"));
                assert!(names.contains(&"amount"));
            }
            other => panic!("expected named composite, got {other:?}"),
        }
    }

    #[test]
    fn splits_qualified_names() {
        assert_eq!(
            split_qualified("Balances.Transfer"),
            ("Balances".to_string(), "Transfer".to_string())
        );
        assert_eq!(split_qualified("Bare"), (String::new(), "Bare".to_string()));
    }

    #[test]
    fn maps_a_full_portal_block() {
        // A recorded-shape portal block: header + one event + one signed call.
        let json = serde_json::json!({
            "header": {
                "number": 100,
                "hash": "0xaaa",
                "parentHash": "0x999",
                "specVersion": 145,
                "timestamp": 1_700_000_000_000u64
            },
            "events": [
                {
                    "extrinsicIndex": 2,
                    "name": "Balances.Transfer",
                    "phase": "ApplyExtrinsic",
                    "args": { "from": "0x01", "to": "0x02", "amount": 500 }
                }
            ],
            "calls": [
                {
                    "extrinsicIndex": 2,
                    "name": "Balances.transfer_keep_alive",
                    "success": true,
                    "args": { "dest": "0x02", "value": 500 },
                    "origin": { "__kind": "Signed", "value": "0x01" }
                }
            ]
        });
        let pb: PortalBlock = serde_json::from_value(json).unwrap();
        let b = map_block(pb, 42);

        assert_eq!(b.id.number, 100);
        assert_eq!(b.id.hash, "0xaaa");
        assert_eq!(b.parent_hash, "0x999");
        assert_eq!(b.spec_version, 145);
        assert_eq!(b.timestamp, Some(1_700_000_000_000));
        assert!(b.finalized);

        assert_eq!(b.events.len(), 1);
        let ev = &b.events[0];
        assert_eq!(ev.pallet, "Balances");
        assert_eq!(ev.name, "Transfer");
        assert_eq!(
            ev.index, 0,
            "derived from position (portal has no event index)"
        );
        assert_eq!(ev.extrinsic_index, Some(2));

        assert_eq!(b.extrinsics.len(), 1);
        let ext = &b.extrinsics[0];
        assert_eq!(ext.pallet, "Balances");
        assert_eq!(ext.call, "transfer_keep_alive");
        assert_eq!(ext.index, 2);
        assert!(ext.success);
        assert!(ext.signed);
        // The 0x-prefixed address is picked (not the "Signed" tag); it's not a
        // 32-byte account, so it stays as the raw hex string.
        assert_eq!(ext.signer.as_deref(), Some("0x01"));
    }

    #[test]
    fn signer_32byte_hex_origin_becomes_ss58() {
        // A signed call whose origin carries a full 32-byte hex account is
        // rendered as an SS58 address (prefix 42 → starts '5'), consistent with
        // the RPC source.
        let acct_hex = format!("0x{}", "ab".repeat(32)); // 32 bytes
        let json = serde_json::json!({
            "header": { "number": 5, "hash": "0x5", "parentHash": "0x4" },
            "calls": [{
                "extrinsicIndex": 0,
                "name": "Balances.transfer",
                "origin": { "__kind": "Signed", "value": acct_hex }
            }]
        });
        let pb: PortalBlock = serde_json::from_value(json).unwrap();
        let b = map_block(pb, 42);
        let signer = b.extrinsics[0].signer.as_deref().unwrap();
        assert!(signer.starts_with('5'), "expected SS58, got {signer}");
        // Round-trips back to the same 32 bytes.
        let decoded: subxt::utils::AccountId32 = signer.parse().unwrap();
        assert_eq!(decoded.0, [0xab; 32]);
    }

    #[test]
    fn unsigned_call_has_no_signer() {
        let json = serde_json::json!({
            "header": { "number": 1, "hash": "0x1", "parentHash": "0x0" },
            "calls": [ { "extrinsicIndex": 0, "name": "Timestamp.set", "args": { "now": 1 } } ]
        });
        let pb: PortalBlock = serde_json::from_value(json).unwrap();
        let b = map_block(pb, 42);
        assert!(!b.extrinsics[0].signed);
        assert_eq!(b.extrinsics[0].signer, None);
    }

    #[test]
    fn events_only_block_has_no_extrinsics() {
        let json = serde_json::json!({
            "header": { "number": 1, "hash": "0x1", "parentHash": "0x0" },
            "events": [ { "index": 0, "name": "System.ExtrinsicSuccess", "args": {} } ]
        });
        let pb: PortalBlock = serde_json::from_value(json).unwrap();
        let b = map_block(pb, 42);
        assert_eq!(b.events.len(), 1);
        assert!(b.extrinsics.is_empty());
        assert_eq!(b.spec_version, 0, "defaulted when absent");
        assert_eq!(b.timestamp, None);
    }
}
