//! # subdex-core
//!
//! Core traits and chain-agnostic types for the **subdex** Substrate indexer
//! framework.
//!
//! The framework is built around three traits that compose into an indexing
//! pipeline:
//!
//! - [`DataSource`] — produces decoded [`Block`]s for a range and
//!   the live tip (default impl: direct RPC via `subxt`).
//! - [`Handler`] — user-implemented, code-first; turns blocks into their own
//!   database rows (default store: Postgres).
//! - [`Store`] — owns the indexer cursor and reorg rollback, and hands handlers
//!   a transaction so their writes commit atomically with the cursor.
//!
//! An optional [`ProcessorObserver`] hook lets consumers observe the run loop
//! (batches, reorgs, head-lag, errors) for metrics or progress reporting, with a
//! zero-cost [`NoopObserver`] default.
//!
//! This crate has no async-runtime or database dependencies; it only defines the
//! contracts the other crates implement.

pub mod error;
pub mod handler;
pub mod observer;
pub mod source;
pub mod store;
pub mod types;

pub use error::{Result, SubdexError};
pub use handler::{Handler, Prepared};
pub use observer::{NoopObserver, ProcessorObserver};
pub use source::DataSource;
pub use store::Store;
pub use types::{Block, BlockBatch, BlockHash, BlockId, BlockNumber, Event, Extrinsic};

#[cfg(test)]
mod tests {
    use super::types::*;

    fn sample_block(number: BlockNumber, hash: &str, parent: &str) -> Block {
        Block {
            id: BlockId {
                number,
                hash: hash.into(),
            },
            parent_hash: parent.into(),
            timestamp: Some(1_700_000_000_000),
            spec_version: 145,
            finalized: true,
            extrinsics: vec![],
            events: vec![],
        }
    }

    #[test]
    fn block_batch_first_last_empty() {
        let empty = BlockBatch { blocks: vec![] };
        assert!(empty.is_empty());
        assert!(empty.first().is_none());
        assert!(empty.last().is_none());

        let batch = BlockBatch {
            blocks: vec![
                sample_block(1, "0xaa", "0x00"),
                sample_block(2, "0xbb", "0xaa"),
            ],
        };
        assert!(!batch.is_empty());
        assert_eq!(batch.first().unwrap().id.number, 1);
        assert_eq!(batch.last().unwrap().id.number, 2);
    }

    #[test]
    fn block_id_equality_is_number_and_hash() {
        let a = BlockId {
            number: 5,
            hash: "0xabc".into(),
        };
        let b = BlockId {
            number: 5,
            hash: "0xabc".into(),
        };
        let c = BlockId {
            number: 5,
            hash: "0xdef".into(),
        };
        assert_eq!(a, b);
        assert_ne!(
            a, c,
            "same height but different hash must not be equal (reorg case)"
        );
    }

    #[test]
    fn reorg_error_carries_fork_height() {
        let e = crate::SubdexError::Reorg {
            height: 42,
            expected: "0xparent".into(),
            got: "0xother".into(),
        };
        let msg = e.to_string();
        assert!(msg.contains("42"));
        assert!(msg.contains("0xparent"));
        assert!(msg.contains("0xother"));
    }
}
