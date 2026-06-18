//! Framework-wide error type.

use thiserror::Error;

/// Errors surfaced by data sources, stores, and the processor.
#[derive(Debug, Error)]
pub enum SubdexError {
    #[error("data source error: {0}")]
    Source(String),

    #[error("store error: {0}")]
    Store(String),

    #[error("decode error: {0}")]
    Decode(String),

    /// A reorg was detected: the parent hash of an incoming block did not match
    /// the hash we recorded for its parent height. Carries the height at which
    /// the chains diverge so the store can roll back above it.
    #[error("reorg detected at height {height}: expected parent {expected}, got {got}")]
    Reorg {
        height: crate::types::BlockNumber,
        expected: String,
        got: String,
    },

    #[error("handler error: {0}")]
    Handler(String),

    #[error("configuration error: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, SubdexError>;
