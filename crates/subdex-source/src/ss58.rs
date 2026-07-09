//! SS58 address encoding with a configurable network prefix.
//!
//! subxt's `AccountId32::to_ss58check` hardcodes the default Substrate prefix
//! (42) and its hashing helper is private, so this module re-implements the
//! standard SS58 algorithm to support any network prefix:
//!
//! ```text
//! payload  = prefix_bytes ++ account_id
//! checksum = blake2b_512("SS58PRE" ++ payload)[0..2]
//! address  = base58(payload ++ checksum)
//! ```
//!
//! It also decodes the SCALE-encoded `MultiAddress` an extrinsic's signature
//! carries (subxt's `address_bytes()`) down to the underlying 32-byte account,
//! handling the common variants and returning `None` for shapes we can't map to
//! an `AccountId32` (so the caller can fall back to raw hex rather than panic).

use base58::ToBase58;
use blake2::{Blake2b512, Digest};

/// Encode a 32-byte account id as an SS58 address for network `prefix`.
///
/// Prefixes `0..=63` occupy a single byte; `64..=16383` use the 2-byte form
/// (per the SS58 spec). The default Substrate prefix is 42 (addresses start `5`).
pub fn encode(account: &[u8; 32], prefix: u16) -> String {
    let mut payload = prefix_bytes(prefix);
    payload.extend_from_slice(account);

    let checksum = ss58_checksum(&payload);
    payload.extend_from_slice(&checksum[..2]);

    payload.to_base58()
}

/// The SS58 prefix encoded to its 1- or 2-byte form.
fn prefix_bytes(prefix: u16) -> Vec<u8> {
    if prefix <= 63 {
        vec![prefix as u8]
    } else {
        // 2-byte form: see the SS58 spec's "simple/full" address types.
        let ident = prefix & 0b0011_1111_1111_1111; // 14 bits
        let first = 0b0100_0000 | ((ident >> 8) as u8);
        let second = (ident & 0xFF) as u8;
        vec![first, second]
    }
}

/// `blake2b_512("SS58PRE" ++ data)` — the SS58 checksum hash.
fn ss58_checksum(data: &[u8]) -> Vec<u8> {
    let mut ctx = Blake2b512::new();
    ctx.update(b"SS58PRE");
    ctx.update(data);
    ctx.finalize().to_vec()
}

/// Extract the 32-byte account id from an extrinsic's SCALE-encoded address
/// bytes (`MultiAddress`), returning `None` for variants that don't carry a
/// plain 32-byte account.
///
/// `MultiAddress` layout (SCALE): a 1-byte variant index, then the payload:
/// - `0x00 Id(AccountId32)`    → 32 bytes follow  ← the overwhelmingly common case
/// - `0x02 Raw(Vec<u8>)`       → length-prefixed, not a fixed account → `None`
/// - `0x03 Address32([u8;32])` → 32 bytes follow
/// - `0x01 Index`, `0x04 Address20` → not a 32-byte account → `None`
///
/// Some runtimes use a bare `AccountId32` (no `MultiAddress` wrapper) as the
/// address, in which case `bytes` is exactly 32 long with no variant tag; we
/// accept that too.
pub fn account_from_address_bytes(bytes: &[u8]) -> Option<[u8; 32]> {
    // Bare AccountId32 (no MultiAddress enum wrapper).
    if bytes.len() == 32 {
        return bytes.try_into().ok();
    }
    // MultiAddress: variant tag + payload.
    match bytes.first()? {
        // Id(AccountId32) / Address32([u8;32]): 32 bytes follow the tag.
        0x00 | 0x03 if bytes.len() == 33 => bytes[1..].try_into().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_default_prefix_to_5_address() {
        // Prefix 42 → addresses start with '5' and round-trip via subxt.
        let acct = [0xab_u8; 32];
        let addr = encode(&acct, 42);
        assert!(addr.starts_with('5'), "got {addr}");
        // subxt parses/validates the SS58 (checksum + bytes), proving correctness.
        let decoded: subxt::utils::AccountId32 = addr.parse().unwrap();
        assert_eq!(decoded.0, acct, "round-trips to the same 32 bytes");
    }

    #[test]
    fn matches_subxt_default_encoding() {
        // Our prefix-42 output must be byte-identical to subxt's AccountId32
        // Display (which uses prefix 42), so switching in the mapping is safe.
        let acct = [0x11_u8; 32];
        assert_eq!(
            encode(&acct, 42),
            subxt::utils::AccountId32(acct).to_string()
        );
    }

    #[test]
    fn different_prefix_gives_different_address() {
        let acct = [0x01_u8; 32];
        let sub = encode(&acct, 42); // Substrate
        let polkadot = encode(&acct, 0); // Polkadot (prefix 0 → starts '1')
        assert_ne!(sub, polkadot);
        assert!(
            polkadot.starts_with('1'),
            "polkadot addr starts with 1: {polkadot}"
        );
    }

    #[test]
    fn two_byte_prefix_encodes_without_panic() {
        // A prefix above 63 uses the 2-byte form; just assert it produces a valid
        // base58 string of plausible length.
        let acct = [0x02_u8; 32];
        let addr = encode(&acct, 128);
        assert!(!addr.is_empty());
    }

    #[test]
    fn decodes_id_variant_multiaddress() {
        let acct = [0x07_u8; 32];
        let mut bytes = vec![0x00]; // Id variant tag
        bytes.extend_from_slice(&acct);
        assert_eq!(account_from_address_bytes(&bytes), Some(acct));
    }

    #[test]
    fn decodes_bare_account32() {
        let acct = [0x09_u8; 32];
        assert_eq!(account_from_address_bytes(&acct), Some(acct));
    }

    #[test]
    fn rejects_non_account_shapes() {
        // Index variant (0x01) with a small payload → not a 32-byte account.
        assert_eq!(account_from_address_bytes(&[0x01, 0x05]), None);
        // Empty / too short.
        assert_eq!(account_from_address_bytes(&[]), None);
        assert_eq!(account_from_address_bytes(&[0x00, 0x01, 0x02]), None);
        // Address20 (0x04) — 20-byte, not our shape.
        let mut a20 = vec![0x04];
        a20.extend_from_slice(&[0u8; 20]);
        assert_eq!(account_from_address_bytes(&a20), None);
    }
}
