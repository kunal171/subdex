//! Runnable example indexer.
//!
//! Wires a real `SubxtSource` + `PgStore` + [`TransfersHandler`] into a
//! `Processor`, backfills from a start height to the finalized head, then follows
//! the tip — recording `Assets.Deposited` / `Assets.Withdrawn` events into a
//! `transfers` table.
//!
//! Configuration via environment variables:
//!
//! | Var            | Default                                   | Meaning                         |
//! |----------------|-------------------------------------------|---------------------------------|
//! | `WS_URL`       | `wss://archive2.mainnet-unit.com`         | Chain RPC endpoint              |
//! | `DATABASE_URL` | `postgres://postgres:postgres@localhost:55432/subdex` | Postgres connection |
//! | `START_HEIGHT` | `finalized_head - 20`                     | Backfill start (fresh DB only)  |
//! | `FOLLOW`       | `1`                                       | Follow the tip after backfill (`0` to exit) |
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:55432/subdex \
//! WS_URL=wss://archive2.mainnet-unit.com \
//!     cargo run -p subdex-example-transfers
//! ```

use std::sync::Arc;
use subdex::{DataSource, Processor, ProcessorConfig};
use subdex_example_transfers::TransfersHandler;
use subdex_source::{SourceConfig, SubxtSource};
use subdex_store::{PgStore, StoreConfig};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs: set RUST_LOG=info for progress output.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let ws = env_or("WS_URL", "wss://archive2.mainnet-unit.com");
    let db = env_or(
        "DATABASE_URL",
        "postgres://postgres:postgres@localhost:55432/subdex",
    );
    let follow = env_or("FOLLOW", "1") != "0";

    tracing::info!(%ws, "connecting to chain");
    let source = SubxtSource::connect(SourceConfig::new(&ws)).await?;
    let store = PgStore::connect(StoreConfig::new(&db)).await?;

    // Default start: the last ~20 finalized blocks, so a fresh run does something
    // immediately. Overridable via START_HEIGHT.
    let head = source.finalized_head().await?;
    let start = match std::env::var("START_HEIGHT") {
        Ok(s) => s.parse().unwrap_or_else(|_| head.saturating_sub(20)),
        Err(_) => head.saturating_sub(20),
    };

    let processor = Processor::new(
        source,
        store,
        vec![Arc::new(TransfersHandler)],
        ProcessorConfig::from_height(start),
    );

    tracing::info!(start, head, "initializing + backfilling");
    processor.init().await?;
    let next = processor.backfill().await?;
    tracing::info!(next, "backfill complete");

    if follow {
        tracing::info!("following the finalized tip (Ctrl-C to stop)");
        // Unbounded follow: runs until the process is stopped.
        processor.follow(None).await?;
    }

    Ok(())
}
