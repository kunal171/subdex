//! Configuration for the subxt-backed [`SubxtSource`](crate::SubxtSource).

/// Selects which parts of each block to fetch + decode.
///
/// Fetching block data is the latency-bound cost; an indexer that only needs
/// events shouldn't pay to fetch and decode extrinsics every block. Disable what
/// you don't use to cut RPC work.
///
/// The block header is always fetched (its `parent_hash` is required for
/// reorg-safety). Note: the block `timestamp` is derived from the `Timestamp.set`
/// **extrinsic**, so it is `None` when `extrinsics` is disabled.
#[derive(Clone, Copy, Debug)]
pub struct DataSelection {
    /// Fetch + decode the block's events (and derive per-extrinsic success).
    pub events: bool,
    /// Fetch + decode the block's extrinsics (and the block timestamp).
    pub extrinsics: bool,
}

impl Default for DataSelection {
    /// Everything, for correctness out of the box.
    fn default() -> Self {
        Self {
            events: true,
            extrinsics: true,
        }
    }
}

impl DataSelection {
    /// Only events (skip extrinsics). Timestamp will be `None`.
    pub fn events_only() -> Self {
        Self {
            events: true,
            extrinsics: false,
        }
    }

    /// Only extrinsics (skip events). Note: per-extrinsic `success` can't be
    /// derived without events, so it defaults to `true` in this mode.
    pub fn extrinsics_only() -> Self {
        Self {
            events: false,
            extrinsics: true,
        }
    }
}

/// Connection + batching configuration for a [`SubxtSource`](crate::SubxtSource).
#[derive(Clone, Debug)]
pub struct SourceConfig {
    /// WebSocket RPC endpoint of the chain, e.g. `wss://your-substrate-node:9944`.
    pub url: String,
    /// Maximum number of blocks [`fetch_batch`](subdex_core::DataSource::fetch_batch)
    /// returns per call.
    ///
    /// Defaults to 100 via [`SourceConfig::new`].
    pub batch_size: u32,
    /// How many blocks within a batch to fetch+decode **concurrently**.
    ///
    /// Direct RPC is latency-bound (each block is several round-trips), so
    /// fetching sequentially wastes most of the time waiting on the network.
    /// Issuing up to `concurrency` block fetches in flight at once hides that
    /// latency and is the single biggest backfill throughput lever (analogous to
    /// the connection `capacity` in other indexers). Results are still returned
    /// in block order.
    ///
    /// Defaults to 16. Higher values sync faster but put more load on the node;
    /// keep it at/under what the endpoint tolerates.
    pub concurrency: usize,
    /// Which parts of each block to fetch + decode. Defaults to everything; set
    /// to e.g. [`DataSelection::events_only`] to skip the extrinsics fetch.
    pub selection: DataSelection,
}

impl SourceConfig {
    /// Create a config for `url` with sensible defaults (batch size 100,
    /// concurrency 16, fetch everything).
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            batch_size: 100,
            concurrency: 16,
            selection: DataSelection::default(),
        }
    }

    /// Override which block data to fetch (e.g. [`DataSelection::events_only`]).
    pub fn with_selection(mut self, selection: DataSelection) -> Self {
        self.selection = selection;
        self
    }

    /// Override the maximum batch size (floored at 1).
    pub fn with_batch_size(mut self, batch_size: u32) -> Self {
        self.batch_size = batch_size.max(1);
        self
    }

    /// Override how many blocks within a batch are fetched concurrently
    /// (floored at 1).
    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = SourceConfig::new("ws://localhost");
        assert_eq!(c.batch_size, 100);
        assert_eq!(c.concurrency, 16);
    }

    #[test]
    fn builders_floor_at_one() {
        let c = SourceConfig::new("ws://localhost")
            .with_batch_size(0)
            .with_concurrency(0);
        assert_eq!(c.batch_size, 1);
        assert_eq!(c.concurrency, 1);
    }

    #[test]
    fn selection_default_is_everything() {
        let s = DataSelection::default();
        assert!(s.events && s.extrinsics);
        // SourceConfig default mirrors it.
        let c = SourceConfig::new("ws://localhost");
        assert!(c.selection.events && c.selection.extrinsics);
    }

    #[test]
    fn selection_presets() {
        let e = DataSelection::events_only();
        assert!(e.events && !e.extrinsics);
        let x = DataSelection::extrinsics_only();
        assert!(!x.events && x.extrinsics);

        let c = SourceConfig::new("ws://localhost").with_selection(DataSelection::events_only());
        assert!(c.selection.events && !c.selection.extrinsics);
    }
}
