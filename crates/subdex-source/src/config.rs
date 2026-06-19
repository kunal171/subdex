//! Configuration for the subxt-backed [`SubxtSource`](crate::SubxtSource).

/// Connection + batching configuration for a [`SubxtSource`](crate::SubxtSource).
#[derive(Clone, Debug)]
pub struct SourceConfig {
    /// WebSocket RPC endpoint of the chain, e.g. `wss://archive2.mainnet-unit.com`.
    pub url: String,
    /// Maximum number of blocks [`fetch_batch`](crate::SubxtSource::fetch_batch)
    /// will return per call. Direct RPC fetches blocks one-by-one under the hood,
    /// so this bounds how many it groups into a single returned batch. Defaults
    /// to 100 via [`SourceConfig::new`].
    pub batch_size: u32,
}

impl SourceConfig {
    /// Create a config for `url` with a sensible default batch size (100).
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            batch_size: 100,
        }
    }

    /// Override the maximum batch size.
    pub fn with_batch_size(mut self, batch_size: u32) -> Self {
        self.batch_size = batch_size.max(1);
        self
    }
}
