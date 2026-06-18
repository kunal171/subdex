//! The [`DataSource`] trait: where decoded blocks come from.
//!
//! The framework ships a direct-RPC source (`subdex-source`, built on `subxt`)
//! that works against any Substrate chain. Additional sources (an SQD-portal
//! streaming client, a columnar archive reader) can implement the same trait
//! without touching the processor or handlers.

use crate::error::Result;
use crate::types::{BlockBatch, BlockNumber};
use async_trait::async_trait;

/// A source of decoded blocks for some block range, plus the live tip.
///
/// Implementations are responsible for decoding raw chain data into the
/// framework's [`Block`](crate::types::Block) model, including decoding each
/// block against the metadata for *its own* spec version.
#[async_trait]
pub trait DataSource: Send + Sync {
    /// The highest finalized block currently available from this source.
    async fn finalized_head(&self) -> Result<BlockNumber>;

    /// Fetch a batch of blocks in `[from, to]` (inclusive). A source may return
    /// fewer than requested (e.g. it caps batch size); the processor advances by
    /// whatever it returns. Returns an empty batch if `from > to`.
    async fn fetch_batch(&self, from: BlockNumber, to: BlockNumber) -> Result<BlockBatch>;

    /// Subscribe to newly finalized blocks at the tip, one batch at a time.
    /// Returns the next batch once available; implementations should block (await)
    /// until there is something to deliver. Used for live indexing after backfill.
    async fn next_finalized(&self) -> Result<BlockBatch>;

    /// A human-readable name for logs/metrics, e.g. `"subxt-rpc"`.
    fn name(&self) -> &str;
}
