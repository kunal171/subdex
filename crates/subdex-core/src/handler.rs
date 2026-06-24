//! The [`Handler`] trait: the user-facing, code-first SDK surface.
//!
//! A framework user implements `Handler` to turn decoded blocks into their own
//! database rows. They define their own tables (via migrations they own) and
//! write to them through the store transaction the processor hands them, so
//! their writes commit atomically with the indexer cursor.
//!
//! Handlers are generic over the concrete [`Store`] so they receive the real,
//! typed transaction handle (e.g. a `sqlx::Transaction`) and can run arbitrary
//! queries — no schema DSL, full Rust.

use crate::error::Result;
use crate::store::Store;
use crate::types::Block;
use async_trait::async_trait;

/// Processes decoded blocks into user-defined storage.
///
/// The processor drives a handler over **batches** of blocks via
/// [`process_batch`](Handler::process_batch), committing one transaction per
/// batch. The default `process_batch` calls [`process_block`](Handler::process_block)
/// for each block, so you can implement either:
/// - **`process_block`** — simplest; one method, called per block (writes still
///   commit per *batch*, not per block), or
/// - **`process_batch`** — highest throughput; accumulate across the batch and
///   bulk-write once (avoids the per-row-upsert anti-pattern).
///
/// All writes in a batch share one transaction and commit atomically with the
/// cursor advance; returning `Err` rolls the whole batch back. Any write a
/// handler makes should be tagged with the block height (a `_block` column
/// convention) so reorgs can roll it back.
#[async_trait]
pub trait Handler<S: Store>: Send + Sync {
    /// Optional one-time setup (e.g. create the handler's own tables). Called
    /// once at startup, before any blocks are processed. Default: no-op.
    async fn init(&self, _store: &S) -> Result<()> {
        Ok(())
    }

    /// Handle a single decoded block. Write entity rows via `tx`. Returning
    /// `Err` aborts the block (the transaction is rolled back and the indexer
    /// stops), so handlers should only error on genuinely unrecoverable issues.
    async fn process_block<'a>(&self, block: &Block, tx: &mut S::Tx<'a>) -> Result<()>;

    /// Handle a contiguous **batch** of decoded blocks on a single transaction.
    ///
    /// The processor commits one transaction per *batch* (not per block), so a
    /// handler that overrides this can collapse many per-block round-trips into
    /// one — e.g. accumulate rows across the whole batch in memory and bulk-write
    /// once. This is the high-throughput path; per-block upserts are the classic
    /// indexer anti-pattern.
    ///
    /// The default implementation simply calls [`process_block`](Handler::process_block)
    /// for each block in order, so existing handlers work unchanged (they just
    /// don't get the bulk-write benefit until they override this). All writes —
    /// from every block in the batch — share `tx` and commit atomically together;
    /// returning `Err` rolls back the entire batch.
    async fn process_batch<'a>(&self, blocks: &[Block], tx: &mut S::Tx<'a>) -> Result<()> {
        for block in blocks {
            self.process_block(block, tx).await?;
        }
        Ok(())
    }

    /// A name for logs/metrics.
    fn name(&self) -> &str;
}
