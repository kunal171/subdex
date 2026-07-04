//! [`SubxtSource`]: a [`DataSource`] backed by a direct WebSocket RPC connection
//! to a Substrate node, via `subxt`. Works against any Substrate chain.

use crate::config::SourceConfig;
use crate::mapping::map_block;
use crate::retry::retry_async;
use async_trait::async_trait;
use futures::lock::Mutex;
use subdex_core::{BlockBatch, BlockNumber, DataSource, Result, SubdexError};
use subxt::config::PolkadotConfig;
use subxt::OnlineClient;

/// The subxt `Config` the source uses. `PolkadotConfig` matches the common
/// Substrate defaults (H256 hashes, u32 block numbers, MultiAddress) and works
/// for most solochains and parachains. Exposed at the crate root so the
/// mapping module can be concrete to it (see [`crate::ChainConfig`]).
pub type ChainConfig = PolkadotConfig;

/// A direct-RPC data source. Decodes each block against the metadata of its own
/// spec version, producing the framework's chain-agnostic [`Block`](subdex_core::Block) model.
pub struct SubxtSource {
    client: OnlineClient<ChainConfig>,
    config: SourceConfig,
    /// The live finalized-block stream, lazily created on first `next_finalized`.
    /// Wrapped so `next_finalized(&self)` can advance it without `&mut self`.
    finalized_stream: Mutex<Option<subxt::client::Blocks<ChainConfig>>>,
}

impl SubxtSource {
    /// Connect to the chain at `config.url`.
    pub async fn connect(config: SourceConfig) -> Result<Self> {
        let client = OnlineClient::<ChainConfig>::from_url(&config.url)
            .await
            .map_err(|e| SubdexError::Source(format!("connect {}: {e}", config.url)))?;
        Ok(Self {
            client,
            config,
            finalized_stream: Mutex::new(None),
        })
    }

    /// Fetch and map a single block by height, retrying transient RPC failures
    /// per the configured [`RetryConfig`](crate::RetryConfig). `finalized` records
    /// whether the caller considers this height final.
    async fn fetch_one(&self, height: BlockNumber, finalized: bool) -> Result<subdex_core::Block> {
        retry_async(self.config.retry, "fetch_block", || async move {
            let at = self
                .client
                .at_block(height)
                .await
                .map_err(|e| SubdexError::Source(format!("at_block {height}: {e}")))?;
            map_block(&at, finalized, self.config.selection).await
        })
        .await
    }

    /// One attempt at pulling the next finalized block: lazily subscribe the
    /// stream if needed, then take one block. Any error here is surfaced to the
    /// retry wrapper in `next_finalized`, which resets the subscription and
    /// retries. Returns an empty batch when the stream is (temporarily) drained.
    async fn next_finalized_once(&self) -> Result<BlockBatch> {
        let mut guard = self.finalized_stream.lock().await;
        if guard.is_none() {
            // `stream_blocks` yields finalized blocks (per subxt docs).
            let stream = self
                .client
                .stream_blocks()
                .await
                .map_err(|e| SubdexError::Source(format!("subscribe finalized: {e}")))?;
            *guard = Some(stream);
        }
        let stream = guard.as_mut().expect("just set");

        match stream.next().await {
            Some(Ok(block)) => {
                let at = block
                    .at()
                    .await
                    .map_err(|e| SubdexError::Source(format!("at finalized block: {e}")))?;
                let mapped = map_block(&at, true, self.config.selection).await?;
                Ok(BlockBatch {
                    blocks: vec![mapped],
                })
            }
            Some(Err(e)) => Err(SubdexError::Source(format!("finalized stream: {e}"))),
            None => Ok(BlockBatch { blocks: vec![] }),
        }
    }
}

#[async_trait]
impl DataSource for SubxtSource {
    async fn finalized_head(&self) -> Result<BlockNumber> {
        retry_async(self.config.retry, "finalized_head", || async {
            // `at_current_block` instantiates a client at the current *finalized*
            // block at the time of the call (per subxt docs).
            let at = self
                .client
                .at_current_block()
                .await
                .map_err(|e| SubdexError::Source(format!("finalized head: {e}")))?;
            Ok(at.block_number() as u32)
        })
        .await
    }

    async fn fetch_batch(&self, from: BlockNumber, to: BlockNumber) -> Result<BlockBatch> {
        if from > to {
            return Ok(BlockBatch { blocks: vec![] });
        }
        // Cap the returned span to the configured batch size.
        let end = to.min(from.saturating_add(self.config.batch_size.saturating_sub(1)));

        // Direct RPC is latency-bound, so fetch the span's blocks CONCURRENTLY
        // (up to `concurrency` in flight) rather than one-at-a-time. `buffered`
        // preserves input order, so the returned blocks stay in height order.
        // Anything in [from, to] requested as a batch is treated as finalized
        // (backfill range); live unfinalized blocks come via `next_finalized`.
        use futures::stream::{self, StreamExt, TryStreamExt};
        let blocks: Vec<subdex_core::Block> = stream::iter(from..=end)
            .map(|h| self.fetch_one(h, true))
            .buffered(self.config.concurrency)
            .try_collect()
            .await?;

        Ok(BlockBatch { blocks })
    }

    async fn next_finalized(&self) -> Result<BlockBatch> {
        // Retry transient failures. Crucially, on any error we drop the stored
        // subscription (`take()` below) so the next attempt re-subscribes — this
        // is the reconnect path when the finalized-block stream is dropped.
        retry_async(self.config.retry, "next_finalized", || async {
            match self.next_finalized_once().await {
                Ok(batch) => Ok(batch),
                Err(e) => {
                    // Reset the subscription so the retry reconnects from scratch.
                    self.finalized_stream.lock().await.take();
                    Err(e)
                }
            }
        })
        .await
    }

    fn name(&self) -> &str {
        "subxt-rpc"
    }
}
