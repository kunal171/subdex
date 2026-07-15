//! Runnable multi-handler example.
//!
//! Wires **two** handlers ([`BalancesHandler`] + [`AssetsHandler`]) onto one
//! `Processor`, backfills then follows the tip, and (by default) serves a GraphQL
//! API exposing both handlers' tables. All framework config is loaded via
//! [`subdex_config`] (`.env` / `subdex.toml` / env vars — see that crate).
//!
//! Example-app knobs (not framework config): `FOLLOW` (default `1`), `SERVE`
//! (default `1`), `GRAPHQL_PORT` (default `4350`). `START_HEIGHT` defaults to
//! `head - 20` when unset so a fresh run does something immediately.
//!
//! ```bash
//! WS_URL=wss://your-node:9944 DATABASE_URL=postgres://…/subdex \
//!     cargo run -p subdex-example-multi-pallet
//! ```

use async_graphql::{EmptyMutation, EmptySubscription, Schema};
use std::sync::Arc;
use subdex::{DataSource, Processor};
use subdex_config::IndexerConfig;
use subdex_example_multi_pallet::{AssetsHandler, BalancesHandler, QueryRoot};
use subdex_graphql::{serve as serve_graphql, GraphqlConfig};
use subdex_source::SubxtSource;
use subdex_store::PgStore;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = IndexerConfig::load()?;
    let follow = env_or("FOLLOW", "1") != "0";
    let serve = env_or("SERVE", "1") != "0";
    let gql_port: u16 = env_or("GRAPHQL_PORT", "4350").parse().unwrap_or(4350);

    tracing::info!(url = %cfg.source.url.as_deref().unwrap_or(""), "connecting to chain");
    let source = SubxtSource::connect(cfg.source_config()).await?;
    let store = PgStore::connect(cfg.store_config()).await?;
    let pool = store.pool().clone();

    let head = source.finalized_head().await?;
    let mut proc_cfg = cfg.processor_config();
    if cfg.processor.start_height.is_none() {
        proc_cfg = proc_cfg.with_start_height(head.saturating_sub(20));
    }
    let start = proc_cfg.start_height;

    // BOTH handlers on one Processor: their writes + the cursor advance commit on
    // the same transaction per batch, so a block is either fully indexed across
    // both tables or not at all.
    let processor = Processor::new(
        source,
        store,
        vec![Arc::new(BalancesHandler), Arc::new(AssetsHandler)],
        proc_cfg,
    );
    processor.init().await?;

    if serve {
        let schema = Schema::build(QueryRoot::default(), EmptyMutation, EmptySubscription)
            .data(pool)
            .finish();
        let gql = GraphqlConfig::on_port(gql_port);
        tracing::info!(
            port = gql_port,
            "GraphQL at http://localhost:{gql_port}/graphql"
        );
        tokio::spawn(async move {
            if let Err(e) = serve_graphql(schema, gql).await {
                tracing::error!("graphql server error: {e}");
            }
        });
    }

    if follow {
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
        let next = processor.backfill().await?;
        tracing::info!(next, "backfill complete");
        if serve {
            tracing::info!("serving GraphQL (Ctrl-C to stop)");
            let _ = tokio::signal::ctrl_c().await;
        }
    }

    Ok(())
}
