//! Runnable example indexer.
//!
//! Wires a real `SubxtSource` + `PgStore` + [`TransfersHandler`] into a
//! `Processor`, backfills from a start height to the finalized head, then follows
//! the tip — recording `Assets.Deposited` / `Assets.Withdrawn` events into a
//! `transfers` table — and (by default) serves a GraphQL API over the indexed
//! data alongside indexing.
//!
//! Configuration is read from the environment (a local `.env` is auto-loaded if
//! present — see `.env.example`).
//!
//! | Var            | Required | Default | Meaning                              |
//! |----------------|----------|---------|--------------------------------------|
//! | `WS_URL`       | **yes**  | —       | Chain RPC endpoint                   |
//! | `DATABASE_URL` | **yes**  | —       | Postgres connection                  |
//! | `START_HEIGHT` | no       | `head-20` | Backfill start (fresh DB only)     |
//! | `FOLLOW`       | no       | `1`     | Follow the tip after backfill (`0` exits) |
//! | `SERVE`        | no       | `1`     | Serve the GraphQL API (`0` to disable) |
//! | `GRAPHQL_PORT` | no       | `4350`  | Port for the GraphQL server          |
//!
//! ```bash
//! cp .env.example .env   # then edit WS_URL / DATABASE_URL
//! cargo run -p subdex-example-transfers
//! ```

use async_graphql::{EmptyMutation, EmptySubscription, Schema};
use std::sync::Arc;
use subdex::{DataSource, Processor, ProcessorConfig};
use subdex_example_transfers::{QueryRoot, TransfersHandler};
use subdex_graphql::{serve as serve_graphql, GraphqlConfig};
use subdex_source::{SourceConfig, SubxtSource};
use subdex_store::{PgStore, StoreConfig};

/// Read an optional env var (tuning knob) with a default.
fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Read a required env var, erroring with a clear message if it's missing.
fn require_env(key: &str) -> anyhow::Result<String> {
    std::env::var(key).map_err(|_| {
        anyhow::anyhow!(
            "missing required env var `{key}` (set it in .env or the environment — see .env.example)"
        )
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load a local .env if present (ignored if absent; real env vars win).
    let _ = dotenvy::dotenv();

    // Logs: set RUST_LOG=info for progress output.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Required configuration — no hardcoded endpoints/credentials.
    let ws = require_env("WS_URL")?;
    let db = require_env("DATABASE_URL")?;

    let follow = env_or("FOLLOW", "1") != "0";
    let serve = env_or("SERVE", "1") != "0";
    let gql_port: u16 = env_or("GRAPHQL_PORT", "4350").parse().unwrap_or(4350);

    tracing::info!(%ws, "connecting to chain");
    let source = SubxtSource::connect(SourceConfig::new(&ws)).await?;
    let store = PgStore::connect(StoreConfig::new(&db)).await?;
    // Clone the pool before the store moves into the processor — the GraphQL
    // server reads from the same database the indexer writes to.
    let pool = store.pool().clone();

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

    // Ensure the schema (subdex_block + transfers) exists before serving queries.
    processor.init().await?;

    // Optionally serve the GraphQL API alongside indexing, on a background task.
    if serve {
        let schema = Schema::build(QueryRoot::default(), EmptyMutation, EmptySubscription)
            .data(pool)
            .finish();
        let cfg = GraphqlConfig::on_port(gql_port);
        tracing::info!(
            port = gql_port,
            "GraphQL at http://localhost:{gql_port}/graphql"
        );
        tokio::spawn(async move {
            if let Err(e) = serve_graphql(schema, cfg).await {
                tracing::error!("graphql server error: {e}");
            }
        });
    }

    if follow {
        // Backfill -> follow the tip until Ctrl-C (init already ran above).
        tracing::info!(
            start,
            head,
            "indexing — backfill then follow (Ctrl-C to stop)"
        );
        processor.backfill().await?;
        let shutdown = async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutdown signal received; stopping after the current block");
        };
        processor.follow_until(shutdown).await?;
        tracing::info!("stopped cleanly");
    } else {
        // Backfill only. If serving, keep the process up so the API stays
        // reachable; otherwise exit.
        let next = processor.backfill().await?;
        tracing::info!(next, "backfill complete");
        if serve {
            tracing::info!("serving GraphQL (Ctrl-C to stop)");
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("stopped cleanly");
        }
    }

    Ok(())
}
