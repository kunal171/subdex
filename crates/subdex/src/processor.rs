//! The [`Processor`]: drives a source through handlers into a store.
//!
//! It implements the full run loop — resume, backfill, and live-follow — with
//! per-block atomic commits and reorg handling. [`run_until`](Processor::run_until)
//! is the one-call entry point (init → backfill → follow with graceful shutdown).

use crate::config::ProcessorConfig;
use std::sync::Arc;
use subdex_core::{Block, BlockNumber, DataSource, Handler, Result, Store};

/// The indexing engine. Generic over a concrete [`DataSource`] `Src` and
/// [`Store`] `St`; holds a list of [`Handler`]s that share the store's
/// transaction type so their writes commit with the cursor.
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

    /// Run the indexer until the `shutdown` future resolves: `init` →
    /// `backfill` to the finalized head → `follow` the tip, stopping cleanly when
    /// `shutdown` completes (e.g. a Ctrl-C signal). Returns `Ok(())` on a clean
    /// shutdown, or the first error encountered.
    ///
    /// This is the recommended one-call entry point for a production indexer.
    /// Backfill runs to completion first (it isn't interrupted mid-history);
    /// shutdown takes effect during the live-follow phase.
    pub async fn run_until<F>(&self, shutdown: F) -> Result<()>
    where
        F: std::future::Future<Output = ()> + Send,
    {
        self.init().await?;
        self.backfill().await?;
        self.follow_until(shutdown).await
    }

    /// The height to resume indexing from: `cursor + 1` if we've indexed
    /// anything, else `config.start_height`.
    async fn resume_height(&self) -> Result<BlockNumber> {
        Ok(match self.store.cursor().await? {
            Some(c) => c.number.saturating_add(1),
            None => self.config.start_height,
        })
    }

    /// Backfill from the resume height up to (and including) the source's current
    /// finalized head, in batches. Returns the next height to process once the
    /// head is reached. Reorgs encountered during backfill rewind the cursor and
    /// fetching continues from the corrected height.
    pub async fn backfill(&self) -> Result<BlockNumber> {
        let mut next = self.resume_height().await?;
        let head = self.source.finalized_head().await?;

        while next <= head {
            let to = next
                .saturating_add(self.config.batch_size.saturating_sub(1))
                .min(head);
            let batch = self.source.fetch_batch(next, to).await?;
            if batch.blocks.is_empty() {
                // Source returned nothing for this window; avoid a busy spin.
                break;
            }

            for block in &batch.blocks {
                match self.process_block(block).await? {
                    None => {
                        next = block.id.number.saturating_add(1);
                    }
                    Some(refetch_from) => {
                        // Reorg handled: jump back and re-fetch from there.
                        next = refetch_from;
                        break;
                    }
                }
            }
        }
        Ok(next)
    }

    /// Follow the finalized tip: pull one batch at a time from the source's
    /// finalized stream and process each block. `max_batches` bounds the loop
    /// (`None` runs until the stream ends / errors) — primarily so tests and
    /// bounded runs terminate. Returns when the stream is exhausted or the bound
    /// is hit.
    pub async fn follow(&self, max_batches: Option<usize>) -> Result<()> {
        let mut count = 0usize;
        loop {
            if let Some(max) = max_batches {
                if count >= max {
                    break;
                }
            }
            let batch = self.source.next_finalized().await?;
            count += 1;
            if batch.blocks.is_empty() {
                // Nothing new at the tip yet.
                if max_batches.is_some() {
                    // Bounded mode (tests): an empty batch means the script is
                    // exhausted, so stop rather than loop.
                    break;
                }
                continue;
            }
            for block in &batch.blocks {
                // Reorgs at the tip are handled the same way; on a reorg we simply
                // continue — the next stream blocks will re-deliver the corrected
                // chain (the source drives ordering at the tip).
                let _ = self.process_block(block).await?;
            }
        }
        Ok(())
    }

    /// Like [`follow`](Processor::follow) but stops cleanly when `shutdown`
    /// resolves. Each wait on the source's tip is raced against `shutdown`; if
    /// shutdown wins, the loop returns `Ok(())`. A batch that is already being
    /// processed completes first (we only check shutdown while *waiting* for the
    /// next batch), so a block is never left half-processed.
    pub async fn follow_until<F>(&self, shutdown: F) -> Result<()>
    where
        F: std::future::Future<Output = ()> + Send,
    {
        // Pin the shutdown future so it can be polled repeatedly in the loop.
        tokio::pin!(shutdown);
        loop {
            let batch = tokio::select! {
                biased;
                _ = &mut shutdown => return Ok(()),
                next = self.source.next_finalized() => next?,
            };
            if batch.blocks.is_empty() {
                // Nothing new at the tip yet; loop and wait again (the select
                // above will still observe shutdown).
                continue;
            }
            for block in &batch.blocks {
                let _ = self.process_block(block).await?;
            }
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{test_block, test_chain, MemStore, RecordingHandler, ScriptedSource};
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

    /// A processor over a scripted source replaying `blocks`, with a config.
    fn processor_over(
        blocks: Vec<Block>,
        handlers: Vec<Arc<dyn Handler<MemStore>>>,
        config: ProcessorConfig,
    ) -> Processor<ScriptedSource, MemStore> {
        Processor::new(
            ScriptedSource::new(blocks),
            MemStore::new(),
            handlers,
            config,
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
        assert_eq!(
            p.process_block(&test_block(1, "0x1", "0x0")).await.unwrap(),
            None
        );
        assert_eq!(
            p.process_block(&test_block(2, "0x2", "0x1")).await.unwrap(),
            None
        );

        assert_eq!(h.heights(), vec![1, 2]);
        assert_eq!(p.store().cursor().await.unwrap().unwrap().number, 2);
    }

    #[tokio::test]
    async fn process_block_first_block_has_no_parent_check() {
        let h = Arc::new(RecordingHandler::new());
        let p = processor_with(vec![h.clone()]);
        // Height 0: no parent to validate; commits directly.
        assert_eq!(
            p.process_block(&test_block(0, "0x0", "0x00"))
                .await
                .unwrap(),
            None
        );
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
        assert_eq!(
            refetch,
            Some(2),
            "caller should re-fetch from the parent height"
        );

        // Heights 2 and 3 (the diverged tail) are dropped; height 1 retained.
        assert_eq!(p.store().cursor().await.unwrap().unwrap().number, 1);
        assert!(p.store().hash_at(2).await.unwrap().is_none());
        assert!(p.store().hash_at(3).await.unwrap().is_none());
        assert!(p.store().hash_at(1).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn backfill_indexes_full_range_in_batches() {
        // Chain 0..10; small batch size to exercise multiple batches.
        let h = Arc::new(RecordingHandler::new());
        let p = processor_over(
            test_chain(0, 10),
            vec![h.clone()],
            ProcessorConfig::default().with_batch_size(3),
        );

        let next = p.backfill().await.unwrap();

        assert_eq!(
            h.heights(),
            (0..10).collect::<Vec<_>>(),
            "all blocks indexed in order"
        );
        assert_eq!(next, 10, "resume height is one past the head");
        assert_eq!(p.store().cursor().await.unwrap().unwrap().number, 9);
    }

    #[tokio::test]
    async fn backfill_resumes_from_existing_cursor() {
        // Pre-seed the store at height 4, then backfill a 0..8 chain: only 5..8
        // should be (re)processed.
        let h = Arc::new(RecordingHandler::new());
        let p = processor_over(
            test_chain(0, 8),
            vec![h.clone()],
            ProcessorConfig::default(),
        );
        // Seed cursor at 4 by committing blocks 0..=4 first via the source.
        for b in test_chain(0, 5) {
            p.commit_block(&b).await.unwrap();
        }
        h.seen.lock().unwrap().clear();

        let next = p.backfill().await.unwrap();
        assert_eq!(h.heights(), vec![5, 6, 7], "resumes at cursor+1");
        assert_eq!(next, 8);
    }

    #[tokio::test]
    async fn follow_processes_streamed_blocks_until_exhausted() {
        let h = Arc::new(RecordingHandler::new());
        let p = processor_over(
            test_chain(0, 4),
            vec![h.clone()],
            ProcessorConfig::default(),
        );

        // Bounded follow: enough batches to drain the 4 scripted blocks + 1 empty.
        p.follow(Some(10)).await.unwrap();

        assert_eq!(h.heights(), vec![0, 1, 2, 3]);
        assert_eq!(p.store().cursor().await.unwrap().unwrap().number, 3);
    }

    #[tokio::test]
    async fn follow_until_stops_on_shutdown() {
        let h = Arc::new(RecordingHandler::new());
        let p = processor_over(
            test_chain(0, 4),
            vec![h.clone()],
            ProcessorConfig::default(),
        );

        // Shutdown fires after a short delay — long enough to drain the 4
        // scripted tip blocks, after which the source yields empty batches and
        // the loop is waiting (so the shutdown branch of the select wins).
        let shutdown = async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        p.follow_until(shutdown).await.unwrap();

        assert_eq!(
            h.heights(),
            vec![0, 1, 2, 3],
            "processed the streamed blocks"
        );
        assert_eq!(p.store().cursor().await.unwrap().unwrap().number, 3);
    }

    #[tokio::test]
    async fn follow_until_immediate_shutdown_processes_nothing() {
        let h = Arc::new(RecordingHandler::new());
        let p = processor_over(
            test_chain(0, 4),
            vec![h.clone()],
            ProcessorConfig::default(),
        );

        // Already-resolved shutdown: the `biased` select picks it on the first
        // iteration before any block is fetched.
        p.follow_until(std::future::ready(())).await.unwrap();
        assert!(
            h.heights().is_empty(),
            "no blocks processed when shutdown is immediate"
        );
    }

    #[tokio::test]
    async fn run_until_backfills_then_follows_then_stops() {
        // The scripted source serves the same chain for both backfill
        // (fetch_batch) and follow (next_finalized). After init+backfill indexes
        // 0..4 (cursor at 3), the follow phase re-streams from index 0; reorg
        // detection means blocks 0..3 match stored hashes and re-commit
        // idempotently, and the run stops on shutdown. We assert it terminates
        // cleanly and the cursor is at the head.
        let h = Arc::new(RecordingHandler::new());
        let p = processor_over(
            test_chain(0, 4),
            vec![h.clone()],
            ProcessorConfig::default(),
        );

        let shutdown = async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        p.run_until(shutdown).await.unwrap();

        // Backfill indexed the whole chain; cursor sits at the head.
        assert_eq!(p.store().cursor().await.unwrap().unwrap().number, 3);
    }
}
