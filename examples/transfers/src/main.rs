//! Runnable example indexer.
//!
//! Wires a real `SubxtSource` + `PgStore` + [`TransfersHandler`] into a
//! `Processor`, backfills from a start height to the finalized head, then follows
//! the tip — recording `Assets.Deposited` / `Assets.Withdrawn` events into a
//! `transfers` table — and (by default) serves a GraphQL API over the indexed
//! data alongside indexing.
//!
//! Framework configuration (source / store / processor) is loaded by
//! [`subdex_config`] — a local `.env` and an optional `subdex.toml` are picked up
//! automatically, and env vars override the file. See that crate for the full
//! variable list (`WS_URL`, `DATABASE_URL`, `BATCH_SIZE`, `CONCURRENCY`,
//! `SS58_PREFIX`, `STRICT`, `REORG_RETENTION`, `MAX_REORG_DEPTH`, `START_HEIGHT`, …).
//!
//! This binary adds three **example-app** knobs on top (not framework config):
//!
//! | Var            | Default | Meaning                                    |
//! |----------------|---------|--------------------------------------------|
//! | `FOLLOW`       | `1`     | Follow the tip after backfill (`0` exits)  |
//! | `SERVE`        | `1`     | Serve the GraphQL API (`0` to disable)     |
//! | `GRAPHQL_PORT` | `4350`  | Port for the GraphQL server                |
//!
//! `START_HEIGHT` defaults here to `head - 20` (last ~20 finalized blocks) when
//! left unset, so a fresh run does something immediately.
//!
//! ```bash
//! cp .env.example .env   # then edit WS_URL / DATABASE_URL
//! cargo run -p subdex-example-transfers
//! ```

use async_graphql::{EmptyMutation, EmptySubscription, Schema};
use std::sync::Arc;
use subdex::{DataSource, Processor};
use subdex_config::IndexerConfig;
use subdex_example_transfers::{QueryRoot, TransfersHandler};
use subdex_graphql::{serve as serve_graphql, GraphqlConfig};
use subdex_source::SubxtSource;
use subdex_store::PgStore;

/// Read an optional example-app env var (not framework config) with a default.
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

    // Framework config (source/store/processor) via the shared loader: an
    // optional TOML file, overlaid by env vars (WS_URL, DATABASE_URL, …). A local
    // `.env` is auto-loaded. See `subdex-config` for the full var list.
    let cfg = IndexerConfig::load()?;

    // Example-app knobs (not framework config).
    let follow = env_or("FOLLOW", "1") != "0";
    let serve = env_or("SERVE", "1") != "0";
    let gql_port: u16 = env_or("GRAPHQL_PORT", "4350").parse().unwrap_or(4350);

    tracing::info!(url = %cfg.source.url.as_deref().unwrap_or(""), "connecting to chain");
    let source = SubxtSource::connect(cfg.source_config()).await?;
    let store = PgStore::connect(cfg.store_config()).await?;
    // Clone the pool before the store moves into the processor — the GraphQL
    // server reads from the same database the indexer writes to.
    let pool = store.pool().clone();

    // Start height: use the configured one if set, else default to the last ~20
    // finalized blocks so a fresh run does something immediately.
    let head = source.finalized_head().await?;
    let mut proc_cfg = cfg.processor_config();
    if cfg.processor.start_height.is_none() {
        proc_cfg = proc_cfg.with_start_height(head.saturating_sub(20));
    }
    let start = proc_cfg.start_height;

    let processor = Processor::new(source, store, vec![Arc::new(TransfersHandler)], proc_cfg);

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
