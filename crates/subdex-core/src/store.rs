//! The [`Store`] trait: where the indexer's progress cursor lives and how
//! handler writes are committed atomically with that cursor.
//!
//! The framework ships a Postgres implementation (`subdex-store`). The trait is
//! kept minimal: it owns the *indexer's own* bookkeeping (the height/hash
//! cursor and reorg rollback). User entity tables are written by handlers
//! through a transaction the store hands them, so a block's handler writes and
//! the cursor advance commit together (all-or-nothing).

use crate::error::Result;
use crate::types::{Block, BlockId, BlockNumber};
use async_trait::async_trait;

/// Persistent storage backend for indexer state.
///
/// The unit of atomicity is a single block: a transaction obtained from
/// [`begin`](Store::begin) carries both the handler's writes and the cursor
/// advance ([`set_cursor`](Store::set_cursor)), so [`commit`](Store::commit)
/// persists them together. On a reorg, [`rollback_to`](Store::rollback_to) must undo
/// everything strictly above a height.
#[async_trait]
pub trait Store: Send + Sync {
    /// The block-scoped transaction handle handed to handlers. Backend-specific
    /// (e.g. wraps a `sqlx::Transaction`). Handlers downcast it via the concrete
    /// store type they were built against.
    type Tx<'a>: Send
    where
        Self: 'a;

    /// Run framework migrations / ensure the bookkeeping schema exists.
    async fn init(&self) -> Result<()>;

    /// The last fully-committed block, or `None` if nothing indexed yet.
    /// Used to resume after restart and to detect reorgs (via the stored hash).
    async fn cursor(&self) -> Result<Option<BlockId>>;

    /// Look up the stored hash for a previously-indexed height, if we still hold
    /// it (recent/unfinalized heights). Used to validate an incoming block's
    /// `parent_hash` and locate the fork point on a reorg.
    async fn hash_at(&self, height: BlockNumber) -> Result<Option<String>>;

    /// Begin a transaction scoped to committing one block.
    async fn begin<'a>(&'a self) -> Result<Self::Tx<'a>>;

    /// Within `tx`, advance the cursor to `block`, recording its hash AND the
    /// metadata needed for reorg detection (`parent_hash`) and observability
    /// (`timestamp`, `spec_version`). Handlers will have already written their
    /// entity rows on the same `tx`, so this call is what makes the block's
    /// indexing atomic with the cursor advance.
    ///
    /// Takes the full [`Block`] rather than just a [`BlockId`] because the store
    /// must persist `parent_hash` to later validate an incoming block's parent
    /// and locate the fork point on a reorg (see [`Store::hash_at`]).
    async fn set_cursor<'a>(&self, tx: &mut Self::Tx<'a>, block: &Block) -> Result<()>;

    /// Commit the transaction (handler writes + cursor advance together).
    async fn commit<'a>(&self, tx: Self::Tx<'a>) -> Result<()>;

    /// Roll back ALL indexed data strictly above `height` (handler entity rows
    /// and bookkeeping), atomically. Invoked on reorg before re-indexing the
    /// corrected chain. Implementations rely on framework-managed `_block` columns
    /// to know what to delete.
    async fn rollback_to(&self, height: BlockNumber) -> Result<()>;
}
