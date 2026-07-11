//! Processor configuration.

use subdex_core::BlockNumber;

/// Tuning knobs for the [`Processor`](crate::Processor) run loop.
///
/// Note: how many block rows the *store* retains for reorg detection is a
/// **store** concern — see `StoreConfig::reorg_retention` in `subdex-store`
/// (which prunes `subdex_block` on commit). Keep it ≥ [`max_reorg_depth`](Self::max_reorg_depth)
/// so a reorg's fork point is still in the table.
#[derive(Clone, Debug)]
pub struct ProcessorConfig {
    /// Block height to start indexing from when the store has no cursor yet
    /// (a fresh database). Ignored once a cursor exists — the processor always
    /// resumes from `cursor + 1`. Defaults to 0 (genesis) via [`Default`].
    pub start_height: BlockNumber,
    /// Maximum number of blocks to request per backfill batch from the source.
    /// The source may return fewer. Defaults to 100.
    pub batch_size: u32,
    /// Maximum depth (in blocks) a reorg may rewind before the processor treats
    /// it as a hard error rather than rolling back further. On a reorg the engine
    /// walks back to the true common ancestor; if that ancestor is more than
    /// `max_reorg_depth` blocks below the cursor, it errors instead of rewinding —
    /// such depth on a finalized-block indexer signals a misconfiguration (e.g. a
    /// non-finalized source) rather than a real fork. `0` means unbounded (never
    /// error on depth). Defaults to 64.
    pub max_reorg_depth: u32,
}

impl Default for ProcessorConfig {
    fn default() -> Self {
        Self {
            start_height: 0,
            batch_size: 100,
            max_reorg_depth: 64,
        }
    }
}

impl ProcessorConfig {
    /// A config starting backfill from `start_height` with default batch size.
    pub fn from_height(start_height: BlockNumber) -> Self {
        Self {
            start_height,
            ..Default::default()
        }
    }

    /// Override the backfill batch size (floored at 1).
    pub fn with_batch_size(mut self, batch_size: u32) -> Self {
        self.batch_size = batch_size.max(1);
        self
    }

    /// Override the maximum reorg depth (`0` = unbounded).
    pub fn with_max_reorg_depth(mut self, max_reorg_depth: u32) -> Self {
        self.max_reorg_depth = max_reorg_depth;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = ProcessorConfig::default();
        assert_eq!(c.start_height, 0);
        assert_eq!(c.batch_size, 100);
        assert_eq!(c.max_reorg_depth, 64);
    }

    #[test]
    fn builders() {
        let c = ProcessorConfig::from_height(500)
            .with_batch_size(0)
            .with_max_reorg_depth(0);
        assert_eq!(c.start_height, 500);
        assert_eq!(c.batch_size, 1, "batch size floored at 1");
        assert_eq!(c.max_reorg_depth, 0, "0 = unbounded");
    }
}
