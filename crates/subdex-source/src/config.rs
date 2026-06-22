//! Configuration for the subxt-backed [`SubxtSource`](crate::SubxtSource).

/// Connection + batching configuration for a [`SubxtSource`](crate::SubxtSource).
#[derive(Clone, Debug)]
pub struct SourceConfig {
    /// WebSocket RPC endpoint of the chain, e.g. `wss://archive2.mainnet-unit.com`.
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
}

impl SourceConfig {
    /// Create a config for `url` with sensible defaults (batch size 100,
    /// concurrency 16).
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            batch_size: 100,
            concurrency: 16,
        }
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
}
