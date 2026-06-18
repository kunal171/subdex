//! Chain-agnostic data model that flows from a [`DataSource`](crate::DataSource)
//! through the processor to user [`Handler`](crate::Handler)s.
//!
//! These types are deliberately decoupled from `subxt`'s generated types so the
//! framework can support multiple data sources (direct RPC, SQD portal, columnar
//! archive) behind the same interface, and so handlers see a stable shape across
//! runtime upgrades.

use scale_value::Value;
use serde::{Deserialize, Serialize};

/// A block height (block number).
pub type BlockNumber = u32;

/// A block hash, hex-encoded (`0x…`).
pub type BlockHash = String;

/// Identifies a block by both number and hash. The hash is what makes
/// reorg detection possible: the same number can appear with different hashes
/// across a fork.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockId {
    pub number: BlockNumber,
    pub hash: BlockHash,
}

/// A fully decoded block: its identity, parent, the runtime spec version it was
/// authored under, and the decoded events and extrinsics within it.
///
/// `spec_version` is carried explicitly because correct decoding is
/// spec-version-dependent — the framework decodes each block against the
/// metadata for *its* spec, which is how it stays correct across runtime
/// upgrades (the failure mode behind real-world indexer/runtime drift).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub id: BlockId,
    pub parent_hash: BlockHash,
    /// Unix timestamp (ms) from the `Timestamp::set` inherent, if present.
    pub timestamp: Option<u64>,
    pub spec_version: u32,
    /// Whether this block is at or below the chain's finalized head at the time
    /// it was delivered. Unfinalized blocks may be rolled back on a reorg.
    pub finalized: bool,
    pub extrinsics: Vec<Extrinsic>,
    pub events: Vec<Event>,
}

/// A decoded extrinsic (call) within a block.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Extrinsic {
    /// 0-based index of this extrinsic within the block.
    pub index: u32,
    /// Pallet name, e.g. `"Deposit"`.
    pub pallet: String,
    /// Call name, e.g. `"deposit_token"`.
    pub call: String,
    /// Decoded call arguments as a dynamic scale `Value`.
    pub args: Value,
    /// Whether the extrinsic was signed.
    pub signed: bool,
    /// SS58/hex address of the signer, if signed.
    pub signer: Option<String>,
    /// `true` if the extrinsic dispatched successfully
    /// (`System.ExtrinsicSuccess`), `false` if it failed.
    pub success: bool,
}

/// A decoded event within a block.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    /// 0-based index of this event within the block.
    pub index: u32,
    /// Pallet name that emitted the event, e.g. `"Deposit"`.
    pub pallet: String,
    /// Event variant name, e.g. `"CreatedDeposit"`.
    pub name: String,
    /// Decoded event fields as a dynamic scale `Value`.
    pub fields: Value,
    /// Index of the extrinsic this event was emitted from, if applicable
    /// (`None` for initialization/finalization events).
    pub extrinsic_index: Option<u32>,
}

/// A contiguous batch of blocks delivered by a [`DataSource`](crate::DataSource).
/// Batching lets sources (especially streaming/columnar ones) amortize overhead;
/// a direct-RPC source may deliver batches of one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockBatch {
    pub blocks: Vec<Block>,
}

impl BlockBatch {
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
    pub fn first(&self) -> Option<&Block> {
        self.blocks.first()
    }
    pub fn last(&self) -> Option<&Block> {
        self.blocks.last()
    }
}
