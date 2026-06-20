//! The [`Processor`]: drives a source through handlers into a store.
//!
//! This module currently implements the per-block commit unit. The reorg check
//! and the backfill/live run loop are layered on in subsequent commits.

use crate::config::ProcessorConfig;
use std::sync::Arc;
use subdex_core::{Block, BlockNumber, DataSource, Handler, Result, Store};

/// The indexing engine. Generic over a concrete [`DataSource`] `Src` and
/// [`Store`] `St`; holds a list of [`Handler`]s that share the store's
/// transaction type so their writes commit with the cursor.
///
/// `source`/`config` and the accessor helpers below are consumed by the
/// backfill + live run loop, which is added in the following commits on this
/// branch — hence `#[allow(dead_code)]` until then.
#[allow(dead_code)]
pub struct Processor<Src, St>
where
    Src: DataSource,
    St: Store,
{
    source: Src,
    store: St,
    handlers: Vec<Arc<dyn Handler<St>>>,
    config: ProcessorConfig,
}

impl<Src, St> Processor<Src, St>
where
    Src: DataSource,
    St: Store,
{
    /// Construct a processor from a source, store, handlers, and config.
    pub fn new(
        source: Src,
        store: St,
        handlers: Vec<Arc<dyn Handler<St>>>,
        config: ProcessorConfig,
    ) -> Self {
        Self {
            source,
            store,
            handlers,
            config,
        }
    }

    /// Access the store (e.g. for tests / handler init).
    pub fn store(&self) -> &St {
        &self.store
    }

    /// Run every handler's one-time `init`, then ensure the store schema exists.
    /// Call once before processing begins.
    pub async fn init(&self) -> Result<()> {
        self.store.init().await?;
        for h in &self.handlers {
            h.init(&self.store).await?;
        }
        Ok(())
    }

    /// Process the next block in chain order, handling reorgs.
    ///
    /// Before committing, validate that `block.parent_hash` matches the hash we
    /// stored for `block.number - 1`:
    /// - **Match** (or we hold no record of the parent — first block, or pruned):
    ///   commit normally.
    /// - **Mismatch**: a reorg replaced the parent. Walk back to the fork point
    ///   (the highest height whose stored hash still agrees with the new chain is
    ///   unknown from a single block, so we roll back to the parent height − 1
    ///   conservatively, i.e. drop the divergent parent and above) and signal the
    ///   caller to re-fetch from there. We roll back to `block.number - 1` so the
    ///   diverged parent is removed and will be re-indexed from the new chain.
    ///
    /// Returns `Ok(Some(refetch_from))` when a reorg was handled and the caller
    /// should resume fetching at `refetch_from`; `Ok(None)` when the block was
    /// committed normally.
    pub async fn process_block(&self, block: &Block) -> Result<Option<BlockNumber>> {
        // Genesis / very first block has no parent to validate against.
        if block.id.number > 0 {
            let parent_height = block.id.number - 1;
            if let Some(stored_parent_hash) = self.store.hash_at(parent_height).await? {
                if stored_parent_hash != block.parent_hash {
                    // Reorg: our stored parent differs from this block's parent.
                    // Drop the diverged parent and everything above it, then ask
                    // the caller to re-index starting at the parent height.
                    let fork = parent_height.saturating_sub(1);
                    self.store.rollback_to(fork).await?;
                    return Ok(Some(parent_height));
                }
            }
        }
        self.commit_block(block).await?;
        Ok(None)
    }

    /// Process and commit a single block atomically:
    /// open a store transaction, run every handler on it, advance the cursor,
    /// then commit. If any handler errors, the transaction is dropped (rolled
    /// back) and the error is returned — nothing is half-written.
    pub async fn commit_block(&self, block: &Block) -> Result<()> {
        let mut tx = self.store.begin().await?;

        for h in &self.handlers {
            h.process_block(block, &mut tx).await?;
        }

        self.store.set_cursor(&mut tx, block).await?;
        self.store.commit(tx).await?;
        Ok(())
    }

    /// The configured backfill batch size. (Used by the run loop, next commit.)
    #[allow(dead_code)]
    pub(crate) fn batch_size(&self) -> u32 {
        self.config.batch_size
    }

    /// The configured start height (used only when the store has no cursor).
    #[allow(dead_code)]
    pub(crate) fn start_height(&self) -> subdex_core::BlockNumber {
        self.config.start_height
    }

    /// Access the source (used by the run loop, added later).
    #[allow(dead_code)]
    pub(crate) fn source(&self) -> &Src {
        &self.source
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{test_block, MemStore, RecordingHandler, ScriptedSource};
    use crate::ProcessorConfig;

    fn processor_with(
        handlers: Vec<Arc<dyn Handler<MemStore>>>,
    ) -> Processor<ScriptedSource, MemStore> {
        Processor::new(
            ScriptedSource::new(vec![]),
            MemStore::new(),
            handlers,
            ProcessorConfig::default(),
        )
    }

    #[tokio::test]
    async fn init_runs_store_and_handler_init() {
        let h = Arc::new(RecordingHandler::new());
        let p = processor_with(vec![h.clone()]);
        p.init().await.unwrap();
        assert!(p.store().was_inited(), "store init should have run");
    }

    #[tokio::test]
    async fn commit_block_advances_cursor_and_runs_handler() {
        let h = Arc::new(RecordingHandler::new());
        let p = processor_with(vec![h.clone()]);

        let b = test_block(7, "0x7", "0x6");
        p.commit_block(&b).await.unwrap();

        // Handler saw the block.
        assert_eq!(h.heights(), vec![7]);
        // Cursor advanced and the block is persisted.
        let cursor = p.store().cursor().await.unwrap().unwrap();
        assert_eq!(cursor.number, 7);
        assert_eq!(cursor.hash, "0x7");
        assert_eq!(p.store().len(), 1);
    }

    #[tokio::test]
    async fn handler_error_rolls_back_the_block() {
        // Handler fails at height 5: the transaction must be discarded, leaving
        // no cursor advance and no persisted row.
        let h = Arc::new(RecordingHandler::failing_at(5));
        let p = processor_with(vec![h.clone()]);

        let err = p.commit_block(&test_block(5, "0x5", "0x4")).await;
        assert!(err.is_err(), "commit should surface the handler error");
        assert_eq!(p.store().len(), 0, "nothing persisted on failure");
        assert!(p.store().cursor().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn multiple_handlers_all_run_in_order() {
        let h1 = Arc::new(RecordingHandler::new());
        let h2 = Arc::new(RecordingHandler::new());
        let p = processor_with(vec![h1.clone(), h2.clone()]);

        p.commit_block(&test_block(1, "0x1", "0x0")).await.unwrap();

        assert_eq!(h1.heights(), vec![1]);
        assert_eq!(h2.heights(), vec![1], "every handler runs for the block");
    }

    #[tokio::test]
    async fn process_block_commits_when_parent_matches() {
        let h = Arc::new(RecordingHandler::new());
        let p = processor_with(vec![h.clone()]);

        // Index 1 then 2; block 2's parent_hash (0x1) matches block 1's hash.
        assert_eq!(p.process_block(&test_block(1, "0x1", "0x0")).await.unwrap(), None);
        assert_eq!(p.process_block(&test_block(2, "0x2", "0x1")).await.unwrap(), None);

        assert_eq!(h.heights(), vec![1, 2]);
        assert_eq!(p.store().cursor().await.unwrap().unwrap().number, 2);
    }

    #[tokio::test]
    async fn process_block_first_block_has_no_parent_check() {
        let h = Arc::new(RecordingHandler::new());
        let p = processor_with(vec![h.clone()]);
        // Height 0: no parent to validate; commits directly.
        assert_eq!(p.process_block(&test_block(0, "0x0", "0x00")).await.unwrap(), None);
        assert_eq!(p.store().cursor().await.unwrap().unwrap().number, 0);
    }

    #[tokio::test]
    async fn process_block_detects_reorg_and_rolls_back() {
        let h = Arc::new(RecordingHandler::new());
        let p = processor_with(vec![h.clone()]);

        // Index a chain 1,2,3 on fork A.
        for b in [
            test_block(1, "0x1a", "0x0"),
            test_block(2, "0x2a", "0x1a"),
            test_block(3, "0x3a", "0x2a"),
        ] {
            assert_eq!(p.process_block(&b).await.unwrap(), None);
        }
        assert_eq!(p.store().len(), 3);

        // Now a block 3 arrives on fork B whose parent (0x2b) != stored 0x2a.
        // Reorg: roll back to fork point (height 1) and ask to refetch from 2.
        let refetch = p
            .process_block(&test_block(3, "0x3b", "0x2b"))
            .await
            .unwrap();
        assert_eq!(refetch, Some(2), "caller should re-fetch from the parent height");

        // Heights 2 and 3 (the diverged tail) are dropped; height 1 retained.
        assert_eq!(p.store().cursor().await.unwrap().unwrap().number, 1);
        assert!(p.store().hash_at(2).await.unwrap().is_none());
        assert!(p.store().hash_at(3).await.unwrap().is_none());
        assert!(p.store().hash_at(1).await.unwrap().is_some());
    }
}
