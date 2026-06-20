//! Processor configuration.

use subdex_core::BlockNumber;

/// Tuning knobs for the [`Processor`](crate::Processor) run loop.
#[derive(Clone, Debug)]
pub struct ProcessorConfig {
    /// Block height to start indexing from when the store has no cursor yet
    /// (a fresh database). Ignored once a cursor exists — the processor always
    /// resumes from `cursor + 1`. Defaults to 0 (genesis) via [`Default`].
    pub start_height: BlockNumber,
    /// Maximum number of blocks to request per backfill batch from the source.
    /// The source may return fewer. Defaults to 100.
    pub batch_size: u32,
    /// How many of the most-recent indexed blocks to retain in the bookkeeping
    /// table for reorg detection. Reorgs deeper than this cannot be detected
    /// (they are assumed impossible below finality). `0` means "retain all"
    /// (no pruning). Defaults to 0 until the processor implements pruning.
    pub reorg_retention: u32,
}

impl Default for ProcessorConfig {
    fn default() -> Self {
        Self {
            start_height: 0,
            batch_size: 100,
            reorg_retention: 0,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = ProcessorConfig::default();
        assert_eq!(c.start_height, 0);
        assert_eq!(c.batch_size, 100);
        assert_eq!(c.reorg_retention, 0);
    }

    #[test]
    fn builders() {
        let c = ProcessorConfig::from_height(500).with_batch_size(0);
        assert_eq!(c.start_height, 500);
        assert_eq!(c.batch_size, 1, "batch size floored at 1");
    }
}
