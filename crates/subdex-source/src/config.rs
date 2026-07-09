//! Configuration for the subxt-backed [`SubxtSource`](crate::SubxtSource).

use std::time::Duration;

/// Bounded retry-with-backoff policy for the source's network operations.
///
/// Direct RPC is unreliable in the small: a request can time out, the WebSocket
/// can drop, the node can restart, or a public endpoint can rate-limit (HTTP
/// 429). Without retries a single such blip aborts the whole run. This policy
/// retries a failed network op with **exponential backoff + jitter**, giving the
/// node time to recover, and only fails after `max_retries` attempts.
///
/// Delays grow as `base_delay * 2^attempt`, capped at `max_delay`, with a small
/// random jitter added so many concurrent retries don't thunder at the node in
/// lockstep. Only transient (network) errors are retried; decode errors fail fast.
#[derive(Clone, Copy, Debug)]
pub struct RetryConfig {
    /// How many times to retry after the first attempt fails (`0` disables retry).
    pub max_retries: u32,
    /// Initial backoff before the first retry; doubles each subsequent attempt.
    pub base_delay: Duration,
    /// Upper bound on any single backoff delay.
    pub max_delay: Duration,
}

impl Default for RetryConfig {
    /// 5 retries, 250ms base doubling to a 30s cap — sane for public nodes.
    fn default() -> Self {
        Self {
            max_retries: 5,
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl RetryConfig {
    /// A policy that never retries (fail on the first error).
    pub fn disabled() -> Self {
        Self {
            max_retries: 0,
            base_delay: Duration::from_millis(0),
            max_delay: Duration::from_millis(0),
        }
    }

    /// The backoff delay before retry `attempt` (0-based): `base * 2^attempt`,
    /// capped at `max_delay`, plus up to ~25% jitter. Deterministic part only
    /// here; jitter is added by the caller so this stays pure/testable.
    pub(crate) fn backoff(&self, attempt: u32) -> Duration {
        let factor = 2u64.saturating_pow(attempt);
        let millis = (self.base_delay.as_millis() as u64).saturating_mul(factor);
        Duration::from_millis(millis).min(self.max_delay)
    }
}

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
    /// Retry-with-backoff policy for transient RPC failures. Defaults to
    /// [`RetryConfig::default`]; use [`RetryConfig::disabled`] to fail fast.
    pub retry: RetryConfig,
    /// SS58 network prefix used to render a signed extrinsic's `signer` as a
    /// canonical `5…`-style address. Defaults to **42** (the generic Substrate
    /// prefix); set your chain's prefix (e.g. 0 for Polkadot, 2 for Kusama) for
    /// addresses that match block explorers.
    pub ss58_prefix: u16,
}

impl SourceConfig {
    /// Create a config for `url` with sensible defaults (batch size 100,
    /// concurrency 16, fetch everything, SS58 prefix 42).
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            batch_size: 100,
            concurrency: 16,
            selection: DataSelection::default(),
            retry: RetryConfig::default(),
            ss58_prefix: 42,
        }
    }

    /// Override which block data to fetch (e.g. [`DataSelection::events_only`]).
    pub fn with_selection(mut self, selection: DataSelection) -> Self {
        self.selection = selection;
        self
    }

    /// Override the transient-failure retry policy (e.g.
    /// [`RetryConfig::disabled`] to fail fast, or a custom backoff).
    pub fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
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

    /// Override the SS58 network prefix used to render signer addresses
    /// (default 42; e.g. 0 for Polkadot, 2 for Kusama).
    pub fn with_ss58_prefix(mut self, prefix: u16) -> Self {
        self.ss58_prefix = prefix;
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
