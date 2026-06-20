//! The [`Processor`]: drives a source through handlers into a store.
//!
//! This module currently implements the per-block commit unit. The reorg check
//! and the backfill/live run loop are layered on in subsequent commits.

use crate::config::ProcessorConfig;
use std::sync::Arc;
use subdex_core::{Block, DataSource, Handler, Result, Store};

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
}
