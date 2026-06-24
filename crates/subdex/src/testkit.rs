//! In-memory test doubles for unit-testing the [`Processor`](crate::Processor)
//! without a real chain or database.
//!
//! Only compiled under `cfg(test)`. Provides:
//! - [`MemStore`]: an in-memory [`Store`] with a real cursor + reorg rollback.
//! - [`RecordingHandler`]: a [`Handler`] that records the heights it saw.
//! - [`ScriptedSource`]: a [`DataSource`] that replays a fixed list of blocks.

use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use subdex_core::{
    Block, BlockBatch, BlockId, BlockNumber, DataSource, Handler, Result, Store, SubdexError,
};

/// An in-memory store. The "transaction" is a buffer of staged block-rows that
/// are applied to the shared map on commit and discarded on drop (rollback).
#[derive(Clone, Default)]
pub struct MemStore {
    /// height -> (hash, parent_hash)
    blocks: Arc<Mutex<BTreeMap<BlockNumber, (String, String)>>>,
    /// Set true once `init` has run, to assert it is called.
    inited: Arc<Mutex<bool>>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test helper: current number of stored blocks.
    pub fn len(&self) -> usize {
        self.blocks.lock().unwrap().len()
    }

    #[allow(dead_code)] // used by run-loop tests in following commits
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn was_inited(&self) -> bool {
        *self.inited.lock().unwrap()
    }
}

/// A staged write buffer acting as the store transaction. On `commit` the
/// processor calls [`Store::commit`], which flushes `staged` into the shared map.
pub struct MemTx {
    blocks: Arc<Mutex<BTreeMap<BlockNumber, (String, String)>>>,
    staged: Vec<(BlockNumber, String, String)>,
}

#[async_trait]
impl Store for MemStore {
    type Tx<'a> = MemTx;

    async fn init(&self) -> Result<()> {
        *self.inited.lock().unwrap() = true;
        Ok(())
    }

    async fn cursor(&self) -> Result<Option<BlockId>> {
        let map = self.blocks.lock().unwrap();
        Ok(map.iter().next_back().map(|(n, (hash, _))| BlockId {
            number: *n,
            hash: hash.clone(),
        }))
    }

    async fn hash_at(&self, height: BlockNumber) -> Result<Option<String>> {
        Ok(self
            .blocks
            .lock()
            .unwrap()
            .get(&height)
            .map(|(h, _)| h.clone()))
    }

    async fn begin<'a>(&'a self) -> Result<Self::Tx<'a>> {
        Ok(MemTx {
            blocks: self.blocks.clone(),
            staged: Vec::new(),
        })
    }

    async fn set_cursor<'a>(&self, tx: &mut Self::Tx<'a>, block: &Block) -> Result<()> {
        tx.staged.push((
            block.id.number,
            block.id.hash.clone(),
            block.parent_hash.clone(),
        ));
        Ok(())
    }

    async fn commit<'a>(&self, tx: Self::Tx<'a>) -> Result<()> {
        let mut map = tx.blocks.lock().unwrap();
        for (n, hash, parent) in tx.staged {
            map.insert(n, (hash, parent));
        }
        Ok(())
    }

    async fn rollback_to(&self, height: BlockNumber) -> Result<()> {
        let mut map = self.blocks.lock().unwrap();
        let above: Vec<BlockNumber> = map.range((height + 1)..).map(|(n, _)| *n).collect();
        for n in above {
            map.remove(&n);
        }
        Ok(())
    }
}

/// A handler that records every block height it processed, and optionally fails
/// at a configured height (to test transaction rollback). It also counts how
/// many times `process_batch` is invoked, so tests can assert the batch path is
/// used (one call per committed batch).
#[derive(Clone, Default)]
pub struct RecordingHandler {
    pub seen: Arc<Mutex<Vec<BlockNumber>>>,
    pub fail_at: Option<BlockNumber>,
    pub batch_calls: Arc<Mutex<usize>>,
}

impl RecordingHandler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn failing_at(height: BlockNumber) -> Self {
        Self {
            fail_at: Some(height),
            ..Self::default()
        }
    }

    pub fn heights(&self) -> Vec<BlockNumber> {
        self.seen.lock().unwrap().clone()
    }

    /// How many times `process_batch` ran (i.e. how many batches were committed).
    pub fn batch_call_count(&self) -> usize {
        *self.batch_calls.lock().unwrap()
    }
}

#[async_trait]
impl Handler<MemStore> for RecordingHandler {
    async fn process_block<'a>(
        &self,
        block: &Block,
        _tx: &mut <MemStore as Store>::Tx<'a>,
    ) -> Result<()> {
        if Some(block.id.number) == self.fail_at {
            return Err(SubdexError::Handler(format!(
                "intentional failure at height {}",
                block.id.number
            )));
        }
        self.seen.lock().unwrap().push(block.id.number);
        Ok(())
    }

    /// Override the default to count batch invocations, then delegate to
    /// `process_block` for each block (so `seen`/`fail_at` behaviour is shared).
    async fn process_batch<'a>(
        &self,
        blocks: &[Block],
        tx: &mut <MemStore as Store>::Tx<'a>,
    ) -> Result<()> {
        *self.batch_calls.lock().unwrap() += 1;
        for block in blocks {
            self.process_block(block, tx).await?;
        }
        Ok(())
    }

    fn name(&self) -> &str {
        "recording"
    }
}

/// A data source that replays a fixed, pre-built list of blocks. `fetch_batch`
/// returns the requested window; `next_finalized` yields the next block beyond
/// the highest one already requested, then blocks forever (simulating the tip).
pub struct ScriptedSource {
    blocks: Vec<Block>,
    /// Cursor for `next_finalized`.
    next_idx: Arc<Mutex<usize>>,
}

impl ScriptedSource {
    pub fn new(blocks: Vec<Block>) -> Self {
        Self {
            blocks,
            next_idx: Arc::new(Mutex::new(0)),
        }
    }
}

#[async_trait]
impl DataSource for ScriptedSource {
    async fn finalized_head(&self) -> Result<BlockNumber> {
        Ok(self.blocks.last().map(|b| b.id.number).unwrap_or(0))
    }

    async fn fetch_batch(&self, from: BlockNumber, to: BlockNumber) -> Result<BlockBatch> {
        let blocks: Vec<Block> = self
            .blocks
            .iter()
            .filter(|b| b.id.number >= from && b.id.number <= to)
            .cloned()
            .collect();
        Ok(BlockBatch { blocks })
    }

    async fn next_finalized(&self) -> Result<BlockBatch> {
        let next = {
            let mut idx = self.next_idx.lock().unwrap();
            if *idx < self.blocks.len() {
                let b = self.blocks[*idx].clone();
                *idx += 1;
                Some(b)
            } else {
                None
            }
        };
        match next {
            Some(b) => Ok(BlockBatch { blocks: vec![b] }),
            None => {
                // No more scripted blocks. A *real* finalized-tip stream blocks
                // here until the node finalizes a new block; we model that with a
                // short async sleep (rather than returning empty immediately),
                // which both reflects reality and lets a racing shutdown future's
                // timer make progress instead of being starved by a busy loop.
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                Ok(BlockBatch { blocks: vec![] })
            }
        }
    }

    fn name(&self) -> &str {
        "scripted"
    }
}

/// Build a test block with a given height, hash and parent hash.
pub fn test_block(number: BlockNumber, hash: &str, parent: &str) -> Block {
    Block {
        id: BlockId {
            number,
            hash: hash.into(),
        },
        parent_hash: parent.into(),
        timestamp: Some(1_700_000_000_000 + number as u64),
        spec_version: 1,
        finalized: true,
        extrinsics: vec![],
        events: vec![],
    }
}

/// Build a contiguous chain `[start, start+count)` with deterministic hashes
/// `0x<height>` and correct parent links.
pub fn test_chain(start: BlockNumber, count: u32) -> Vec<Block> {
    (start..start + count)
        .map(|n| {
            let parent = if n == 0 {
                "0x00".to_string()
            } else {
                format!("0x{:x}", n - 1)
            };
            test_block(n, &format!("0x{n:x}"), &parent)
        })
        .collect()
}
