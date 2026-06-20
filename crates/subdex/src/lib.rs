//! # subdex
//!
//! The indexing **engine**: it drives a [`DataSource`](subdex_core::DataSource)
//! through one or more [`Handler`](subdex_core::Handler)s into a
//! [`Store`](subdex_core::Store), keeping a resumable cursor and handling chain
//! reorganizations.
//!
//! The run loop, in brief:
//!
//! 1. **Resume** from `store.cursor()` (or `config.start_height` on a fresh DB).
//! 2. **Backfill** in batches via `source.fetch_batch()` up to the finalized head.
//! 3. **Follow the tip** via `source.next_finalized()`.
//! 4. For every block: validate its `parent_hash` against the stored hash of the
//!    previous height (reorg check), then run the handlers inside a
//!    `store.begin()` transaction, `set_cursor`, and `commit` — atomically.
//! 5. On a reorg, `store.rollback_to(fork)` and re-index the corrected chain.
//!
//! Built incrementally on `feat/processor`: this commit adds the crate scaffold
//! and [`ProcessorConfig`]; the block-commit unit, reorg handling, and run loop
//! follow.

mod config;
mod processor;
#[cfg(test)]
mod testkit;

pub use config::ProcessorConfig;
pub use processor::Processor;

// Re-export the core contracts so users depend on a single crate.
pub use subdex_core::{
    Block, BlockBatch, BlockId, BlockNumber, DataSource, Event, Extrinsic, Handler, Result, Store,
    SubdexError,
};
