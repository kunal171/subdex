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
/// The processor calls [`process_block`](Handler::process_block) once per block,
/// in order, passing a transaction that is committed (with the cursor advance)
/// when the handler returns `Ok`, or rolled back if it returns `Err`. Any write
/// a handler makes MUST be tagged with the block height in a way the store can
/// roll back (the framework provides helpers / a `_block` column convention) so
/// reorgs stay consistent.
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

    /// A name for logs/metrics.
    fn name(&self) -> &str;
}
