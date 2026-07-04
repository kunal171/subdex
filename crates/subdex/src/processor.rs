//! The [`Processor`]: drives a source through handlers into a store.
//!
//! It implements the full run loop — resume, backfill, and live-follow — with
//! per-block atomic commits and reorg handling. [`run_until`](Processor::run_until)
//! is the one-call entry point (init → backfill → follow with graceful shutdown).

use crate::config::ProcessorConfig;
use std::sync::Arc;
use std::time::Instant;
use subdex_core::{
    Block, BlockNumber, DataSource, Handler, NoopObserver, ProcessorObserver, Result, Store,
};

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
    /// Observability hook, called at key run-loop points. Defaults to the
    /// zero-cost [`NoopObserver`]; set via [`with_observer`](Processor::with_observer).
    observer: Arc<dyn ProcessorObserver>,
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
            observer: Arc::new(NoopObserver),
        }
    }

    /// Attach an observability hook. The engine calls it at key run-loop points
    /// (batch committed, reorg, new head, fetch, error) — for metrics, a progress
    /// reporter, or a test spy. Without this the observer is a no-op.
    pub fn with_observer(mut self, observer: Arc<dyn ProcessorObserver>) -> Self {
        self.observer = observer;
        self
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
        self.observer.on_head(head);

        while next <= head {
            let to = next
                .saturating_add(self.config.batch_size.saturating_sub(1))
                .min(head);
            let fetch_start = Instant::now();
            let batch = self.source.fetch_batch(next, to).await.inspect_err(|e| {
                self.observer.on_error("fetch", &e.to_string());
            })?;
            self.observer
                .on_fetch(batch.blocks.len(), fetch_start.elapsed());
            if batch.blocks.is_empty() {
                // Source returned nothing for this window; avoid a busy spin.
                break;
            }

            // Commit the whole batch in one transaction (DB-side throughput).
            match self.process_batch_blocks(&batch.blocks).await? {
                None => {
                    next = batch.blocks.last().unwrap().id.number.saturating_add(1);
                }
                Some(refetch_from) => {
                    // Reorg handled: jump back and re-fetch from there.
                    next = refetch_from;
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
            // A new finalized tip: report it so observers can track head-lag.
            if let Some(last) = batch.blocks.last() {
                self.observer.on_head(last.id.number);
            }
            // Commit the tip batch in one transaction (same path as backfill).
            // Reorgs are handled inside; on a reorg we simply continue — the next
            // stream batches re-deliver the corrected chain (the source drives
            // ordering at the tip).
            let _ = self.process_batch_blocks(&batch.blocks).await?;
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
            // A new finalized tip: report it so observers can track head-lag.
            if let Some(last) = batch.blocks.last() {
                self.observer.on_head(last.id.number);
            }
            // Commit the tip batch in one transaction (reorg-aware).
            let _ = self.process_batch_blocks(&batch.blocks).await?;
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

    /// Process a contiguous batch of blocks, handling a reorg at the batch
    /// boundary and committing the whole batch in **one** transaction.
    ///
    /// The reorg check is done once, against the batch's first block (the blocks
    /// within a batch are already known-contiguous from the source). On a reorg
    /// the diverged tail is rolled back and `Ok(Some(refetch_from))` is returned
    /// so the caller re-fetches the corrected chain; otherwise the batch is
    /// committed and `Ok(None)` is returned.
    ///
    /// Committing per-batch (not per-block) is the DB-side throughput lever: N
    /// blocks' handler writes + the cursor advance land in a single transaction.
    pub async fn process_batch_blocks(&self, blocks: &[Block]) -> Result<Option<BlockNumber>> {
        let Some(first) = blocks.first() else {
            return Ok(None);
        };

        // Reorg check against the first block's parent.
        if first.id.number > 0 {
            let parent_height = first.id.number - 1;
            if let Some(stored_parent_hash) = self.store.hash_at(parent_height).await? {
                if stored_parent_hash != first.parent_hash {
                    let fork = parent_height.saturating_sub(1);
                    // Depth rolled back: from the current cursor down to the fork.
                    let depth = match self.store.cursor().await? {
                        Some(c) => c.number.saturating_sub(fork),
                        None => 0,
                    };
                    self.store.rollback_to(fork).await?;
                    self.observer.on_reorg(fork, depth);
                    return Ok(Some(parent_height));
                }
            }
        }

        self.commit_batch(blocks).await?;
        Ok(None)
    }

    /// Commit a batch of blocks atomically: open ONE transaction, run every
    /// handler's `process_batch` over all the blocks, advance the cursor to the
    /// last block, then commit once. A handler error drops the transaction
    /// (rolling back the entire batch) — no partial batch is ever persisted.
    pub async fn commit_batch(&self, blocks: &[Block]) -> Result<()> {
        if blocks.is_empty() {
            return Ok(());
        }
        let commit_start = Instant::now();
        let mut tx = self.store.begin().await?;

        for h in &self.handlers {
            h.process_batch(blocks, &mut tx)
                .await
                .inspect_err(|e| self.observer.on_error("handler", &e.to_string()))?;
        }

        // Record every block's hash (needed for reorg detection); the cursor
        // thus ends at the batch's last block. These are cheap upserts on the
        // same transaction — the whole batch still commits once.
        for block in blocks {
            self.store.set_cursor(&mut tx, block).await?;
        }
        self.store
            .commit(tx)
            .await
            .inspect_err(|e| self.observer.on_error("commit", &e.to_string()))?;

        // Notify: batch committed. Cursor is the last block; sum decoded events.
        let cursor = blocks.last().map(|b| b.id.number).unwrap_or(0);
        let events: usize = blocks.iter().map(|b| b.events.len()).sum();
        self.observer
            .on_batch_committed(cursor, blocks.len(), events, commit_start.elapsed());
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

    #[tokio::test]
    async fn commit_batch_commits_whole_batch_in_one_call() {
        let h = Arc::new(RecordingHandler::new());
        let p = processor_with(vec![h.clone()]);

        let batch = test_chain(0, 5);
        assert_eq!(p.process_batch_blocks(&batch).await.unwrap(), None);

        // Every block processed, in order.
        assert_eq!(h.heights(), vec![0, 1, 2, 3, 4]);
        // ONE process_batch call for the whole batch (not 5).
        assert_eq!(
            h.batch_call_count(),
            1,
            "the batch commits in a single call"
        );
        // Cursor ends at the last block; all hashes recorded for reorg detection.
        assert_eq!(p.store().cursor().await.unwrap().unwrap().number, 4);
        assert_eq!(p.store().len(), 5);
    }

    #[tokio::test]
    async fn commit_batch_rolls_back_entire_batch_on_handler_error() {
        // Handler fails at height 3 (mid-batch): the whole batch transaction is
        // dropped — nothing persisted, cursor not advanced.
        let h = Arc::new(RecordingHandler::failing_at(3));
        let p = processor_with(vec![h.clone()]);

        let err = p.process_batch_blocks(&test_chain(0, 5)).await;
        assert!(err.is_err(), "batch should surface the handler error");
        assert_eq!(p.store().len(), 0, "no partial batch persisted");
        assert!(p.store().cursor().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn process_batch_blocks_detects_reorg_at_batch_boundary() {
        let h = Arc::new(RecordingHandler::new());
        let p = processor_with(vec![h.clone()]);

        // Index chain 1,2,3 on fork A.
        assert_eq!(
            p.process_batch_blocks(&[
                test_block(1, "0x1a", "0x0"),
                test_block(2, "0x2a", "0x1a"),
                test_block(3, "0x3a", "0x2a"),
            ])
            .await
            .unwrap(),
            None
        );
        assert_eq!(p.store().len(), 3);

        // A new batch starting at height 3 on fork B: parent (0x2b) != stored
        // 0x2a → reorg. Roll back to fork (height 1), refetch from 2.
        let refetch = p
            .process_batch_blocks(&[test_block(3, "0x3b", "0x2b")])
            .await
            .unwrap();
        assert_eq!(refetch, Some(2));
        assert_eq!(p.store().cursor().await.unwrap().unwrap().number, 1);
    }

    #[tokio::test]
    async fn backfill_uses_the_batch_path() {
        // 10-block chain, batch size 3 → 4 batches (3+3+3+1) → 4 process_batch calls.
        let h = Arc::new(RecordingHandler::new());
        let p = processor_over(
            test_chain(0, 10),
            vec![h.clone()],
            ProcessorConfig::default().with_batch_size(3),
        );

        let next = p.backfill().await.unwrap();
        assert_eq!(h.heights(), (0..10).collect::<Vec<_>>());
        assert_eq!(next, 10);
        assert_eq!(
            h.batch_call_count(),
            4,
            "10 blocks / batch_size 3 = 4 batches (one txn each)"
        );
    }

    // --- Observer wiring ---

    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    /// A spy observer recording what the engine reports.
    #[derive(Default)]
    struct SpyObserver {
        batches: AtomicU32,
        blocks: AtomicU64,
        events: AtomicU64,
        reorgs: AtomicU32,
        last_reorg_depth: AtomicU32,
        heads: AtomicU32,
        last_head: AtomicU64,
        fetches: AtomicU32,
    }

    impl subdex_core::ProcessorObserver for SpyObserver {
        fn on_batch_committed(
            &self,
            cursor: BlockNumber,
            count: usize,
            events: usize,
            _: std::time::Duration,
        ) {
            self.batches.fetch_add(1, Ordering::SeqCst);
            self.blocks.fetch_add(count as u64, Ordering::SeqCst);
            self.events.fetch_add(events as u64, Ordering::SeqCst);
            self.last_head.fetch_max(cursor as u64, Ordering::SeqCst);
        }
        fn on_reorg(&self, _fork: BlockNumber, depth: u32) {
            self.reorgs.fetch_add(1, Ordering::SeqCst);
            self.last_reorg_depth.store(depth, Ordering::SeqCst);
        }
        fn on_head(&self, head: BlockNumber) {
            self.heads.fetch_add(1, Ordering::SeqCst);
            self.last_head.fetch_max(head as u64, Ordering::SeqCst);
        }
        fn on_fetch(&self, _count: usize, _elapsed: std::time::Duration) {
            self.fetches.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn observer_sees_backfill_batches_head_and_fetches() {
        let spy = Arc::new(SpyObserver::default());
        let p = processor_over(
            test_chain(0, 10),
            vec![Arc::new(RecordingHandler::new())],
            ProcessorConfig::default().with_batch_size(3),
        )
        .with_observer(spy.clone());

        p.backfill().await.unwrap();

        // 10 blocks / batch 3 → 4 batches, 10 blocks total.
        assert_eq!(
            spy.batches.load(Ordering::SeqCst),
            4,
            "one per committed batch"
        );
        assert_eq!(spy.blocks.load(Ordering::SeqCst), 10, "all blocks reported");
        assert_eq!(spy.fetches.load(Ordering::SeqCst), 4, "one fetch per batch");
        assert!(
            spy.heads.load(Ordering::SeqCst) >= 1,
            "head reported at least once"
        );
        assert_eq!(
            spy.last_head.load(Ordering::SeqCst),
            9,
            "head/cursor reached 9"
        );
    }

    #[tokio::test]
    async fn observer_sees_reorg_with_depth() {
        let spy = Arc::new(SpyObserver::default());
        let p = processor_with_observer(spy.clone());

        // Index 1,2,3 on fork A.
        for b in [
            test_block(1, "0x1a", "0x0"),
            test_block(2, "0x2a", "0x1a"),
            test_block(3, "0x3a", "0x2a"),
        ] {
            p.process_batch_blocks(&[b]).await.unwrap();
        }
        // Fork B at height 3 whose parent (0x2b) != stored 0x2a → reorg.
        // Cursor is at 3, fork point is height 1, so depth = 3 - 1 = 2.
        let refetch = p
            .process_batch_blocks(&[test_block(3, "0x3b", "0x2b")])
            .await
            .unwrap();
        assert_eq!(refetch, Some(2));
        assert_eq!(spy.reorgs.load(Ordering::SeqCst), 1, "one reorg observed");
        assert_eq!(
            spy.last_reorg_depth.load(Ordering::SeqCst),
            2,
            "rolled back 2 blocks"
        );
    }

    fn processor_with_observer(obs: Arc<SpyObserver>) -> Processor<ScriptedSource, MemStore> {
        Processor::new(
            ScriptedSource::new(vec![]),
            MemStore::new(),
            vec![Arc::new(RecordingHandler::new())],
            ProcessorConfig::default(),
        )
        .with_observer(obs)
    }
}
