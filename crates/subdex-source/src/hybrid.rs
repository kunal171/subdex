//! [`HybridSource`]: backfill from one [`DataSource`], follow the tip from
//! another.
//!
//! The SQD portal is fast for historical backfill but **can't stream a live tip**
//! for Substrate (see [`SqdPortalSource`](crate::SqdPortalSource)). RPC can follow
//! the tip but is slow for backfill. `HybridSource` composes the two so a full
//! indexer gets both: a fast catch-up *and* live indexing, with the engine and
//! handlers unchanged (it's just another `DataSource`).
//!
//! It is generic over any backfill source `B` and tip source `T` — not tied to
//! portal + RPC — so it's reusable and unit-testable with mock sources.
//!
//! Delegation:
//! - [`fetch_batch`](DataSource::fetch_batch) → **backfill** source `B`.
//! - [`next_finalized`](DataSource::next_finalized) → **tip** source `T`.
//! - [`finalized_head`](DataSource::finalized_head) → the **max** of both heads,
//!   so the engine backfills all the way to the true tip before switching to
//!   follow (the tip source is authoritative for "live", but the backfill source
//!   may be ahead or behind at any instant).

use async_trait::async_trait;
use subdex_core::{BlockBatch, BlockNumber, DataSource, Result};

/// A [`DataSource`] that backfills from `backfill` and follows the tip from `tip`.
///
/// Typical use: `HybridSource::new(sqd_portal, subxt_rpc)` — portal for the fast
/// historical sweep, RPC for the live finalized stream.
pub struct HybridSource<B, T> {
    backfill: B,
    tip: T,
}

impl<B, T> HybridSource<B, T>
where
    B: DataSource,
    T: DataSource,
{
    /// Compose a backfill source and a tip source.
    pub fn new(backfill: B, tip: T) -> Self {
        Self { backfill, tip }
    }

    /// The backfill source (e.g. for tests or direct access).
    pub fn backfill_source(&self) -> &B {
        &self.backfill
    }

    /// The tip source.
    pub fn tip_source(&self) -> &T {
        &self.tip
    }
}

#[async_trait]
impl<B, T> DataSource for HybridSource<B, T>
where
    B: DataSource,
    T: DataSource,
{
    /// The highest finalized head across both sources. Backfill runs until it
    /// reaches this, so the engine catches all the way up before following — even
    /// if one source momentarily lags the other.
    async fn finalized_head(&self) -> Result<BlockNumber> {
        let b = self.backfill.finalized_head().await?;
        let t = self.tip.finalized_head().await?;
        Ok(b.max(t))
    }

    /// Backfill history from the fast source.
    async fn fetch_batch(&self, from: BlockNumber, to: BlockNumber) -> Result<BlockBatch> {
        self.backfill.fetch_batch(from, to).await
    }

    /// Follow the live tip from the tip source (the backfill source may not
    /// support streaming — that's the whole point of the split).
    async fn next_finalized(&self) -> Result<BlockBatch> {
        self.tip.next_finalized().await
    }

    fn name(&self) -> &str {
        "hybrid"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::lock::Mutex;
    use subdex_core::{Block, BlockId, SubdexError};

    fn blk(n: u32) -> Block {
        Block {
            id: BlockId {
                number: n,
                hash: format!("0x{n:x}"),
            },
            parent_hash: format!("0x{:x}", n.saturating_sub(1)),
            timestamp: None,
            spec_version: 1,
            finalized: true,
            extrinsics: vec![],
            events: vec![],
        }
    }

    /// A mock source that records which method was called and returns canned data.
    struct MockSource {
        name: &'static str,
        head: BlockNumber,
        /// Tags each fetched block's hash so we can tell which source served it.
        tag: &'static str,
        /// Scripted tip blocks, popped one per `next_finalized`.
        tip: Mutex<Vec<Block>>,
        /// If true, `next_finalized` errors ("backfill-only" behaviour).
        no_tip: bool,
    }

    impl MockSource {
        fn backfill(head: BlockNumber) -> Self {
            Self {
                name: "mock-backfill",
                head,
                tag: "B",
                tip: Mutex::new(vec![]),
                no_tip: true,
            }
        }
        fn tip(head: BlockNumber, tip_blocks: Vec<Block>) -> Self {
            Self {
                name: "mock-tip",
                head,
                tag: "T",
                tip: Mutex::new(tip_blocks),
                no_tip: false,
            }
        }
    }

    #[async_trait]
    impl DataSource for MockSource {
        async fn finalized_head(&self) -> Result<BlockNumber> {
            Ok(self.head)
        }
        async fn fetch_batch(&self, from: BlockNumber, to: BlockNumber) -> Result<BlockBatch> {
            let blocks = (from..=to)
                .map(|n| {
                    let mut b = blk(n);
                    b.id.hash = format!("{}-{n}", self.tag); // tag which source served it
                    b
                })
                .collect();
            Ok(BlockBatch { blocks })
        }
        async fn next_finalized(&self) -> Result<BlockBatch> {
            if self.no_tip {
                return Err(SubdexError::Source("backfill-only".into()));
            }
            let mut tip = self.tip.lock().await;
            match tip.pop() {
                Some(b) => Ok(BlockBatch { blocks: vec![b] }),
                None => Ok(BlockBatch { blocks: vec![] }),
            }
        }
        fn name(&self) -> &str {
            self.name
        }
    }

    #[tokio::test]
    async fn fetch_batch_comes_from_the_backfill_source() {
        let h = HybridSource::new(MockSource::backfill(100), MockSource::tip(90, vec![]));
        let batch = h.fetch_batch(1, 3).await.unwrap();
        assert_eq!(batch.blocks.len(), 3);
        // Tagged "B-*" → served by the backfill source, not the tip.
        assert!(batch.blocks.iter().all(|b| b.id.hash.starts_with("B-")));
    }

    #[tokio::test]
    async fn next_finalized_comes_from_the_tip_source() {
        // The backfill source here is backfill-only (errors on next_finalized);
        // the hybrid must route follow to the tip source instead.
        let h = HybridSource::new(
            MockSource::backfill(100),
            MockSource::tip(100, vec![blk(101)]),
        );
        let batch = h.next_finalized().await.unwrap();
        assert_eq!(batch.blocks.len(), 1);
        assert_eq!(batch.blocks[0].id.number, 101);
    }

    #[tokio::test]
    async fn finalized_head_is_the_max_of_both() {
        // Backfill ahead.
        let h = HybridSource::new(MockSource::backfill(150), MockSource::tip(100, vec![]));
        assert_eq!(h.finalized_head().await.unwrap(), 150);
        // Tip ahead (the common live case: RPC sees the newest finalized block).
        let h = HybridSource::new(MockSource::backfill(100), MockSource::tip(150, vec![]));
        assert_eq!(h.finalized_head().await.unwrap(), 150);
    }

    #[test]
    fn name_is_hybrid() {
        let h = HybridSource::new(MockSource::backfill(1), MockSource::tip(1, vec![]));
        assert_eq!(h.name(), "hybrid");
    }
}
