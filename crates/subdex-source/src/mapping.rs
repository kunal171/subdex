//! Conversion from subxt's typed block view into the framework's chain-agnostic
//! [`Block`] / [`Event`] / [`Extrinsic`] model.
//!
//! All decoding is done **dynamically** against the metadata of the block's own
//! spec version (subxt's `ClientAtBlock` carries the right metadata), decoding
//! event fields and call args into [`scale_value::Value`]. This is what keeps the
//! framework correct across runtime upgrades without per-chain codegen.

use crate::ChainConfig;
use subdex_core::{Block, BlockId, Event, Extrinsic, SubdexError};
use subxt::client::{ClientAtBlock, OnlineClientAtBlockT};
use subxt::events::Phase;

/// A dynamically-decoded scale value. `scale_value::Value` defaults to context
/// `()`, which is the type that implements `DecodeAsFields` — the bound subxt's
/// `decode_*_fields_unchecked_as` requires. This is also what
/// [`subdex_core::Event::fields`] / [`subdex_core::Extrinsic::args`] hold.
type DynValue = scale_value::Value;

/// Build our [`Block`] from a subxt client positioned at a block.
///
/// Concrete to [`ChainConfig`] (subxt's `PolkadotConfig`) so we can read the
/// concrete `SubstrateHeader::parent_hash` field — the generic `Header` trait
/// does not expose a parent hash. `finalized` is supplied by the caller (the
/// source knows whether this height is at/below the finalized head).
pub async fn map_block<C>(
    at: &ClientAtBlock<ChainConfig, C>,
    finalized: bool,
) -> Result<Block, SubdexError>
where
    C: OnlineClientAtBlockT<ChainConfig>,
{
    let number = at.block_number() as u32;
    let hash = format!("0x{}", hex::encode(at.block_hash().as_ref()));
    let parent_hash = {
        let header = at
            .block_header()
            .await
            .map_err(|e| SubdexError::Source(format!("fetch header: {e}")))?;
        // `ChainConfig::Header` is `SubstrateHeader<H256>`, which exposes
        // `parent_hash` directly.
        format!("0x{}", hex::encode(header.parent_hash.as_ref()))
    };
    let spec_version = at.spec_version();

    let extrinsics = map_extrinsics(at).await?;
    let events = map_events(at).await?;
    let timestamp = extract_timestamp(&extrinsics);

    Ok(Block {
        id: BlockId { number, hash },
        parent_hash,
        timestamp,
        spec_version,
        finalized,
        extrinsics,
        events,
    })
}

/// Decode all extrinsics in the block into [`Extrinsic`]s, tagging each with
/// its dispatch success (derived from the `System.ExtrinsicSuccess/Failed`
/// event emitted at the matching phase).
async fn map_extrinsics<C>(
    at: &ClientAtBlock<ChainConfig, C>,
) -> Result<Vec<Extrinsic>, SubdexError>
where
    C: OnlineClientAtBlockT<ChainConfig>,
{
    let success = extrinsic_success_map(at).await?;

    let exts = at
        .extrinsics()
        .fetch()
        .await
        .map_err(|e| SubdexError::Source(format!("fetch extrinsics: {e}")))?;

    let mut out = Vec::new();
    for ext in exts.iter() {
        let ext = ext.map_err(|e| SubdexError::Decode(format!("extrinsic: {e}")))?;
        let index = ext.index() as u32;

        // Decode call args dynamically into a scale Value. We tolerate decode
        // failures on individual extrinsics by recording an empty value rather
        // than aborting the whole block.
        let args = ext
            .decode_call_data_fields_unchecked_as::<DynValue>()
            .unwrap_or_else(|_| scale_value::Value::unnamed_composite(Vec::new()));

        let signed = ext.is_signed();
        let signer = ext.address_bytes().map(|b| format!("0x{}", hex::encode(b)));

        out.push(Extrinsic {
            index,
            pallet: ext.pallet_name().to_string(),
            call: ext.call_name().to_string(),
            args,
            signed,
            signer,
            success: *success.get(&index).unwrap_or(&true),
        });
    }
    Ok(out)
}

/// Decode all events in the block into [`Event`]s.
async fn map_events<C>(at: &ClientAtBlock<ChainConfig, C>) -> Result<Vec<Event>, SubdexError>
where
    C: OnlineClientAtBlockT<ChainConfig>,
{
    let events = at
        .events()
        .fetch()
        .await
        .map_err(|e| SubdexError::Source(format!("fetch events: {e}")))?;

    let mut out = Vec::new();
    for (i, ev) in events.iter().enumerate() {
        let ev = ev.map_err(|e| SubdexError::Decode(format!("event: {e}")))?;

        let fields = ev
            .decode_fields_unchecked_as::<DynValue>()
            .unwrap_or_else(|_| scale_value::Value::unnamed_composite(Vec::new()));

        let extrinsic_index = match ev.phase() {
            Phase::ApplyExtrinsic(n) => Some(n),
            _ => None,
        };

        out.push(Event {
            index: i as u32,
            pallet: ev.pallet_name().to_string(),
            name: ev.event_name().to_string(),
            fields,
            extrinsic_index,
        });
    }
    Ok(out)
}

/// Build a map of `extrinsic_index -> success` by scanning `System` events:
/// `ExtrinsicSuccess` => true, `ExtrinsicFailed` => false, keyed by the phase's
/// extrinsic index.
async fn extrinsic_success_map<C>(
    at: &ClientAtBlock<ChainConfig, C>,
) -> Result<std::collections::HashMap<u32, bool>, SubdexError>
where
    C: OnlineClientAtBlockT<ChainConfig>,
{
    let events = at
        .events()
        .fetch()
        .await
        .map_err(|e| SubdexError::Source(format!("fetch events: {e}")))?;

    let mut map = std::collections::HashMap::new();
    for ev in events.iter() {
        let ev = ev.map_err(|e| SubdexError::Decode(format!("event: {e}")))?;
        if ev.pallet_name() == "System" {
            if let Phase::ApplyExtrinsic(idx) = ev.phase() {
                match ev.event_name() {
                    "ExtrinsicSuccess" => {
                        map.insert(idx, true);
                    }
                    "ExtrinsicFailed" => {
                        map.insert(idx, false);
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(map)
}

/// Extract the block timestamp (ms) from the `Timestamp.set { now }` extrinsic,
/// if present. The argument is decoded dynamically and read as a u128/u64.
fn extract_timestamp(extrinsics: &[Extrinsic]) -> Option<u64> {
    let ts = extrinsics
        .iter()
        .find(|e| e.pallet == "Timestamp" && e.call == "set")?;
    // `Timestamp.set` has a single compact u64 arg named `now`.
    value_first_u64(&ts.args)
}

/// Best-effort extraction of the first integer found in a decoded composite value.
fn value_first_u64(value: &scale_value::Value) -> Option<u64> {
    use scale_value::{Composite, Primitive, ValueDef};
    match &value.value {
        ValueDef::Primitive(Primitive::U128(n)) => Some(*n as u64),
        ValueDef::Composite(Composite::Named(fields)) => {
            fields.iter().find_map(|(_, v)| value_first_u64(v))
        }
        ValueDef::Composite(Composite::Unnamed(items)) => items.iter().find_map(value_first_u64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scale_value::Value;

    #[test]
    fn extracts_timestamp_from_named_composite() {
        // Simulate a decoded `Timestamp.set { now: 1_700_000_000_000 }`.
        let args = Value::named_composite(vec![(
            "now".to_string(),
            Value::u128(1_700_000_000_000u128),
        )]);
        let ext = Extrinsic {
            index: 0,
            pallet: "Timestamp".into(),
            call: "set".into(),
            args,
            signed: false,
            signer: None,
            success: true,
        };
        assert_eq!(extract_timestamp(&[ext]), Some(1_700_000_000_000));
    }

    #[test]
    fn no_timestamp_when_absent() {
        let ext = Extrinsic {
            index: 0,
            pallet: "Balances".into(),
            call: "transfer".into(),
            args: Value::unnamed_composite(Vec::new()),
            signed: true,
            signer: Some("0xabcd".into()),
            success: true,
        };
        assert_eq!(extract_timestamp(&[ext]), None);
    }

    #[test]
    fn value_first_u64_finds_nested_int() {
        let v = Value::named_composite(vec![("now".to_string(), Value::u128(42u128))]);
        assert_eq!(value_first_u64(&v), Some(42));
    }
}
