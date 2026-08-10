//! Backfill throughput benchmark for the SQD portal [`DataSource`].
//!
//! Measures how fast [`SqdPortalSource`] fetches + decodes a fixed block range
//! into the framework `Block` model (no DB writes) — the source-side ceiling
//! behind subdex's "portal backfill is far faster than per-block RPC" claim.
//!
//! Throughput is dominated by two levers, both exposed here:
//! - **`SELECTION`** — the portal is columnar, so it only ships the fields you
//!   ask for. `events` (name+args of events only) is a fraction of the payload of
//!   `all` (events + calls + args), and backfills far faster.
//! - **`CONCURRENCY`** — each `fetch_batch` is one HTTP request; a real backfill
//!   pipelines several in flight. This runs `CONCURRENCY` batches at once.
//!
//! ```bash
//! # realistic selective backfill: events only, 8 batches of 1000 in flight
//! SELECTION=events BLOCKS=40000 BATCH=1000 CONCURRENCY=8 \
//!   cargo run --release -p subdex-source --features sqd --example portal_backfill_bench
//!
//! # worst case: full block data (events + calls + args)
//! SELECTION=all BLOCKS=5000 BATCH=1000 CONCURRENCY=4 \
//!   cargo run --release -p subdex-source --features sqd --example portal_backfill_bench
//! ```

use futures::stream::{self, StreamExt};
use std::time::Instant;
use subdex_core::DataSource;
use subdex_source::{DataSelection, SqdConfig, SqdPortalSource};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let portal = env_or("PORTAL_URL", "https://portal.sqd.dev");
    let dataset = env_or("DATASET", "polkadot");
    let blocks: u32 = env_or("BLOCKS", "20000").parse()?;
    let batch: u32 = env_or("BATCH", "1000").parse()?;
    let concurrency: usize = env_or("CONCURRENCY", "8").parse()?;
    let selection = match env_or("SELECTION", "events").as_str() {
        "all" | "full" => DataSelection::default(),
        _ => DataSelection::events_only(),
    };
    let sel_label = if selection.events && env_or("SELECTION", "events") != "events" {
        "all (events+calls)"
    } else {
        "events-only"
    };

    let cfg = SqdConfig::new(&portal, &dataset)
        .with_batch_size(batch)
        .with_selection(selection);
    // One shared source; DataSource is Send + Sync so we can hit it concurrently.
    let source = std::sync::Arc::new(SqdPortalSource::connect(cfg)?);

    // Backfill a fixed window ending a little below the portal head (so the
    // whole range is available and finalized).
    let head = source.finalized_head().await?;
    let to = head.saturating_sub(10);
    let from = to.saturating_sub(blocks.saturating_sub(1));

    println!(
        "portal={portal} dataset={dataset}  selection={sel_label}  batch={batch}  concurrency={concurrency}\n\
         backfilling {blocks} blocks [{from}..={to}]\n"
    );

    // Build the list of [batch_from, batch_to] ranges, then run up to
    // `concurrency` of them at once — the shape a real backfill uses.
    let mut ranges = Vec::new();
    let mut cursor = from;
    while cursor <= to {
        let batch_to = (cursor + batch - 1).min(to);
        ranges.push((cursor, batch_to));
        cursor = batch_to + 1;
    }
    let total_batches = ranges.len();

    let start = Instant::now();
    let got = stream::iter(ranges)
        .map(|(f, t)| {
            let src = source.clone();
            async move {
                let b = src.fetch_batch(f, t).await?;
                Ok::<u64, Box<dyn std::error::Error + Send + Sync>>(b.blocks.len() as u64)
            }
        })
        .buffer_unordered(concurrency)
        .fold(0u64, |acc, r| async move {
            match r {
                Ok(n) => acc + n,
                Err(e) => {
                    eprintln!("  batch error: {e}");
                    acc
                }
            }
        })
        .await;

    let el = start.elapsed().as_secs_f64();
    println!(
        "─────────────────────────────────────────────\n\
         fetched+decoded {got} blocks in {el:.2}s\n\
         throughput: {:.0} blocks/sec  ({total_batches} batches, {concurrency}-way)\n\
         ─────────────────────────────────────────────",
        got as f64 / el
    );
    Ok(())
}
