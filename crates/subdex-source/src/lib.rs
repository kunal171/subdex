//! # subdex-source
//!
//! A [`DataSource`](subdex_core::DataSource) implementation backed by `subxt`:
//! it connects to a Substrate node over WebSocket RPC and decodes blocks,
//! events, and extrinsics into the framework's chain-agnostic model.
//!
//! It decodes every block against the metadata for **that block's** spec
//! version (subxt's per-block client carries the right metadata), so it stays
//! correct across runtime upgrades without per-chain codegen.
//!
//! ```no_run
//! use subdex_source::{SourceConfig, SubxtSource};
//! use subdex_core::DataSource;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let source = SubxtSource::connect(
//!     SourceConfig::new("wss://your-substrate-node:9944")
//! ).await?;
//! let head = source.finalized_head().await?;
//! let batch = source.fetch_batch(head.saturating_sub(4), head).await?;
//! println!("fetched {} blocks", batch.blocks.len());
//! # Ok(())
//! # }
//! ```

mod config;
mod mapping;
mod retry;
mod source;
#[cfg(feature = "sqd")]
mod sqd;
mod ss58;

pub use config::{DataSelection, RetryConfig, SourceConfig};
pub use source::{ChainConfig, SubxtSource};

#[cfg(feature = "sqd")]
pub use sqd::{SqdConfig, SqdPortalSource};
