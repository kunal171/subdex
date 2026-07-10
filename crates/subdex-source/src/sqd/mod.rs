//! [`SqdPortalSource`]: a [`DataSource`](subdex_core::DataSource) that backfills
//! history from the SQD (Subsquid) portal — pre-decoded, columnar, batched, and
//! far faster than per-block RPC.
//!
//! **Backfill-only.** The SQD portal serves finalized history but does not stream
//! a live tip for Substrate chains, so this source implements `finalized_head`
//! and `fetch_batch` but not `next_finalized` (which returns a clear error). A
//! full indexer pairs it with an RPC source for the live tip (a future
//! `HybridSource`); on its own it is ideal for the historical catch-up phase.
//!
//! Behind the `sqd` feature.

mod client;
mod mapping;

use crate::config::{DataSelection, RetryConfig};
use crate::retry::retry_async;
use crate::sqd::client::PortalClient;
use crate::sqd::mapping::map_block;
use async_trait::async_trait;
use subdex_core::{BlockBatch, BlockNumber, DataSource, Result, SubdexError};

/// Connection + batching configuration for a [`SqdPortalSource`].
#[derive(Clone, Debug)]
pub struct SqdConfig {
    /// Portal base URL, e.g. `https://portal.sqd.dev`.
    pub portal_url: String,
    /// Dataset name, e.g. `polkadot`, `kusama`, `moonbeam`.
    pub dataset: String,
    /// Maximum number of blocks requested per `fetch_batch` call.
    pub batch_size: u32,
    /// Which parts of each block to request (events / extrinsics). The portal's
    /// field selector is built from this, so unused data isn't fetched.
    pub selection: DataSelection,
    /// Retry policy for transient HTTP failures (timeouts, 5xx, dropped conns).
    pub retry: RetryConfig,
    /// SS58 network prefix for rendering a call's signer as a `5…`-style address
    /// (default 42), matching the RPC source. Applied when the portal's `origin`
    /// carries a 32-byte hex account.
    pub ss58_prefix: u16,
}

impl SqdConfig {
    /// Config for `portal_url` + `dataset` with sensible defaults (batch size
    /// 1000 — the portal is columnar and rewards large ranges — fetch everything,
    /// default retry, SS58 prefix 42).
    pub fn new(portal_url: impl Into<String>, dataset: impl Into<String>) -> Self {
        Self {
            portal_url: portal_url.into(),
            dataset: dataset.into(),
            batch_size: 1000,
            selection: DataSelection::default(),
            retry: RetryConfig::default(),
            ss58_prefix: 42,
        }
    }

    /// Override which block data to request (e.g. [`DataSelection::events_only`]).
    pub fn with_selection(mut self, selection: DataSelection) -> Self {
        self.selection = selection;
        self
    }

    /// Override the maximum batch size (floored at 1).
    pub fn with_batch_size(mut self, batch_size: u32) -> Self {
        self.batch_size = batch_size.max(1);
        self
    }

    /// Override the retry policy.
    pub fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }

    /// Override the SS58 network prefix used for signer addresses (default 42).
    pub fn with_ss58_prefix(mut self, prefix: u16) -> Self {
        self.ss58_prefix = prefix;
        self
    }
}

/// A backfill-only [`DataSource`] backed by the SQD portal. Decodes the portal's
/// JSON blocks into the framework's chain-agnostic
/// [`Block`](subdex_core::Block) model.
pub struct SqdPortalSource {
    client: PortalClient,
    config: SqdConfig,
}

impl SqdPortalSource {
    /// Connect to the portal dataset in `config`.
    pub fn connect(config: SqdConfig) -> Result<Self> {
        let client = PortalClient::new(&config.portal_url, &config.dataset)?;
        Ok(Self { client, config })
    }
}

#[async_trait]
impl DataSource for SqdPortalSource {
    async fn finalized_head(&self) -> Result<BlockNumber> {
        retry_async(self.config.retry, "sqd.finalized_head", || {
            self.client.finalized_head()
        })
        .await
    }

    async fn fetch_batch(&self, from: BlockNumber, to: BlockNumber) -> Result<BlockBatch> {
        if from > to {
            return Ok(BlockBatch { blocks: vec![] });
        }
        // Cap the requested span to the configured batch size.
        let end = to.min(from.saturating_add(self.config.batch_size.saturating_sub(1)));
        let selection = self.config.selection;

        let portal_blocks = retry_async(self.config.retry, "sqd.fetch_batch", || {
            self.client.fetch_range(from, end, selection)
        })
        .await?;

        let prefix = self.config.ss58_prefix;
        let blocks = portal_blocks
            .into_iter()
            .map(|pb| map_block(pb, prefix))
            .collect();
        Ok(BlockBatch { blocks })
    }

    async fn next_finalized(&self) -> Result<BlockBatch> {
        // The SQD portal does not stream a live tip for Substrate. Pair this
        // source with an RPC source (e.g. via a HybridSource) for live-follow.
        Err(SubdexError::Source(
            "SqdPortalSource is backfill-only: the SQD portal does not stream a live \
             tip for Substrate. Use an RPC source for next_finalized (live follow)."
                .into(),
        ))
    }

    fn name(&self) -> &str {
        "sqd-portal"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_builders() {
        let c = SqdConfig::new("https://portal.sqd.dev", "polkadot");
        assert_eq!(c.batch_size, 1000);
        assert!(c.selection.events && c.selection.extrinsics);

        let c = c
            .with_batch_size(0)
            .with_selection(DataSelection::events_only());
        assert_eq!(c.batch_size, 1, "floored at 1");
        assert!(c.selection.events && !c.selection.extrinsics);
    }

    #[tokio::test]
    async fn fetch_batch_empty_when_from_gt_to() {
        let src =
            SqdPortalSource::connect(SqdConfig::new("https://portal.sqd.dev", "polkadot")).unwrap();
        let batch = src.fetch_batch(10, 5).await.unwrap();
        assert!(batch.is_empty());
    }

    #[tokio::test]
    async fn next_finalized_is_a_clear_error() {
        let src =
            SqdPortalSource::connect(SqdConfig::new("https://portal.sqd.dev", "polkadot")).unwrap();
        let err = src.next_finalized().await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("backfill-only"), "got: {msg}");
    }

    #[test]
    fn name_is_sqd_portal() {
        let src =
            SqdPortalSource::connect(SqdConfig::new("https://portal.sqd.dev", "polkadot")).unwrap();
        assert_eq!(src.name(), "sqd-portal");
    }
}
