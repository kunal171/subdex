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
/// at a configured height (to test transaction rollback).
#[derive(Clone, Default)]
pub struct RecordingHandler {
    pub seen: Arc<Mutex<Vec<BlockNumber>>>,
    pub fail_at: Option<BlockNumber>,
}

impl RecordingHandler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn failing_at(height: BlockNumber) -> Self {
        Self {
            seen: Arc::new(Mutex::new(Vec::new())),
            fail_at: Some(height),
        }
    }

    pub fn heights(&self) -> Vec<BlockNumber> {
        self.seen.lock().unwrap().clone()
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
        let mut idx = self.next_idx.lock().unwrap();
        if *idx < self.blocks.len() {
            let b = self.blocks[*idx].clone();
            *idx += 1;
            Ok(BlockBatch { blocks: vec![b] })
        } else {
            // No more scripted blocks; return empty (the run loop treats an empty
            // tip batch as "nothing new yet").
            Ok(BlockBatch { blocks: vec![] })
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
