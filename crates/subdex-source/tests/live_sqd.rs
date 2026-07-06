//! Live integration tests against the real SQD portal (behind the `sqd` feature).
//!
//! Network-dependent, so `#[ignore]`d. They hit the public Polkadot dataset by
//! default (override with `SQD_TEST_URL` / `SQD_TEST_DATASET`). Run with:
//!
//! ```bash
//! cargo test -p subdex-source --features sqd --test live_sqd -- --ignored --nocapture
//! ```
//!
//! These assert the same *structural* properties as the live RPC test
//! (`live_chain.rs`): contiguous heights, parent-hash chaining, decoded event
//! names. The decoded arg *values* differ from RPC (the portal is pre-decoded
//! JSON) — that's expected and documented; we don't assert on their shape.

#![cfg(feature = "sqd")]

use subdex_core::DataSource;
use subdex_source::{SqdConfig, SqdPortalSource};

fn portal_url() -> String {
    std::env::var("SQD_TEST_URL").unwrap_or_else(|_| "https://portal.sqd.dev".into())
}

fn dataset() -> String {
    std::env::var("SQD_TEST_DATASET").unwrap_or_else(|_| "polkadot".into())
}

fn source() -> SqdPortalSource {
    SqdPortalSource::connect(SqdConfig::new(portal_url(), dataset()).with_batch_size(5))
        .expect("connect to portal")
}

/// Reads the finalized head, fetches a small recent batch, and asserts the
/// decoded blocks look structurally correct.
#[tokio::test]
#[ignore = "network: hits the live SQD portal; run with --ignored"]
async fn fetches_and_decodes_recent_blocks() {
    let src = source();

    let head = src.finalized_head().await.expect("finalized head");
    assert!(head > 0, "finalized head should be > 0, got {head}");

    let from = head.saturating_sub(4);
    let batch = src.fetch_batch(from, head).await.expect("fetch batch");
    assert!(!batch.blocks.is_empty(), "expected at least one block");
    assert!(
        batch.blocks.len() <= 5,
        "batch_size cap should bound the result, got {}",
        batch.blocks.len()
    );

    // Contiguity + parent-hash chaining (the same checks as the RPC test).
    for window in batch.blocks.windows(2) {
        let (a, b) = (&window[0], &window[1]);
        assert_eq!(b.id.number, a.id.number + 1, "heights must be contiguous");
        assert_eq!(
            b.parent_hash, a.id.hash,
            "block {}'s parent_hash must equal block {}'s hash",
            b.id.number, a.id.number
        );
    }

    for block in &batch.blocks {
        assert!(block.finalized, "portal serves finalized blocks");
        assert!(block.id.hash.starts_with("0x"), "hash is 0x-prefixed");
        // Events should decode with non-empty pallet/name strings.
        for ev in &block.events {
            assert!(!ev.name.is_empty(), "event name empty");
        }
    }

    println!(
        "OK: decoded {} blocks ending at #{} via the SQD portal",
        batch.blocks.len(),
        head
    );
}

/// The portal has no live Substrate tip: `next_finalized` must error clearly.
#[tokio::test]
#[ignore = "network: hits the live SQD portal; run with --ignored"]
async fn next_finalized_errors_backfill_only() {
    let err = source().next_finalized().await.unwrap_err();
    assert!(
        err.to_string().contains("backfill-only"),
        "expected a backfill-only error, got: {err}"
    );
}
