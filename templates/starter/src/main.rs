//! Wires the source + store + your handler into a `Processor`, backfills to the
//! finalized head, then follows the tip — and serves the generated GraphQL API
//! over the same database.
//!
//! Config comes from the environment / a local `.env` / `subdex.toml` via
//! `subdex_config` (WS_URL and DATABASE_URL are required). See docs/GUIDE.md.

use async_graphql::{EmptyMutation, EmptySubscription, Schema};
use std::sync::Arc;
use subdex::{DataSource, Processor};
use subdex_config::IndexerConfig;
use subdex_graphql::{serve as serve_graphql, GraphqlConfig};
use subdex_source::SubxtSource;
use subdex_store::PgStore;

// Your handler + the generated GraphQL query root.
use my_indexer::generated::graphql::QueryRoot;
use my_indexer::MyHandler;

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
    let serve = env_or("SERVE", "1") != "0";
    let follow = env_or("FOLLOW", "1") != "0";
    let gql_port: u16 = env_or("GRAPHQL_PORT", "4350").parse().unwrap_or(4350);

    tracing::info!(url = %cfg.source.url.as_deref().unwrap_or(""), "connecting to chain");
    let source = SubxtSource::connect(cfg.source_config()).await?;
    let store = PgStore::connect(cfg.store_config()).await?;
    // Clone the pool before the store moves into the processor — the GraphQL API
    // reads from the same database the indexer writes to.
    let pool = store.pool().clone();

    // Start height: the configured one, else the last ~20 finalized blocks so a
    // fresh run does something immediately.
    let head = source.finalized_head().await?;
    let mut proc_cfg = cfg.processor_config();
    if cfg.processor.start_height.is_none() {
        proc_cfg = proc_cfg.with_start_height(head.saturating_sub(20));
    }

    let processor = Processor::new(source, store, vec![Arc::new(MyHandler)], proc_cfg);
    processor.init().await?;

    if serve {
        let schema = Schema::build(QueryRoot::default(), EmptyMutation, EmptySubscription)
            .data(pool)
            .finish();
        tracing::info!(port = gql_port, "GraphQL at http://localhost:{gql_port}/graphql");
        tokio::spawn(async move {
            if let Err(e) = serve_graphql(schema, GraphqlConfig::on_port(gql_port)).await {
                tracing::error!("graphql server error: {e}");
            }
        });
    }

    if follow {
        tracing::info!("indexing — backfill then follow (Ctrl-C to stop)");
        processor.backfill().await?;
        let shutdown = async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down after the current block");
        };
        processor.follow_until(shutdown).await?;
    } else {
        let next = processor.backfill().await?;
        tracing::info!(next, "backfill complete");
        if serve {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
    Ok(())
}
