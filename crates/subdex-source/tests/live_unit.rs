//! Live integration tests against the Unit Network mainnet archive.
//!
//! These are **network-dependent** and therefore `#[ignore]`d by default so they
//! never break offline/CI runs. Run them explicitly:
//!
//! ```bash
//! cargo test -p subdex-source --test live_unit -- --ignored --nocapture
//! ```
//!
//! Override the endpoint with `SUBDEX_TEST_WS` if the default is unavailable.

use subdex_core::DataSource;
use subdex_source::{SourceConfig, SubxtSource};

fn ws_url() -> String {
    std::env::var("SUBDEX_TEST_WS")
        .unwrap_or_else(|_| "wss://archive2.mainnet-unit.com".to_string())
}

/// Connects, reads the finalized head, fetches a small recent batch, and asserts
/// the decoded blocks look structurally correct:
/// - contiguous heights,
/// - parent-hash chaining within the batch (block N+1's parent == block N's hash),
/// - a non-zero spec version,
/// - the `Timestamp.set` inherent present with a plausible timestamp,
/// - at least some decoded events with non-empty pallet/name.
#[tokio::test]
#[ignore = "network: hits Unit mainnet RPC; run with --ignored"]
async fn fetches_and_decodes_recent_blocks() {
    let source = SubxtSource::connect(SourceConfig::new(ws_url()).with_batch_size(5))
        .await
        .expect("connect to chain");

    let head = source.finalized_head().await.expect("finalized head");
    assert!(head > 0, "finalized head should be > 0, got {head}");

    let from = head.saturating_sub(4);
    let batch = source.fetch_batch(from, head).await.expect("fetch batch");
    assert!(!batch.blocks.is_empty(), "expected at least one block");
    assert!(
        batch.blocks.len() <= 5,
        "batch_size cap should bound the result, got {}",
        batch.blocks.len()
    );

    // Contiguity + parent-hash chaining.
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
        assert!(block.spec_version > 0, "spec_version should be set");
        assert!(block.finalized, "backfilled blocks are marked finalized");

        // Every Substrate block carries the Timestamp.set inherent.
        let has_timestamp = block
            .extrinsics
            .iter()
            .any(|e| e.pallet == "Timestamp" && e.call == "set");
        assert!(
            has_timestamp,
            "block {} missing Timestamp.set",
            block.id.number
        );
        assert!(
            block.timestamp.unwrap_or(0) > 1_500_000_000_000,
            "decoded timestamp should be a plausible ms epoch"
        );

        // Events should decode with non-empty pallet/name strings.
        for ev in &block.events {
            assert!(!ev.pallet.is_empty(), "event pallet name empty");
            assert!(!ev.name.is_empty(), "event name empty");
        }
    }

    println!(
        "OK: decoded {} blocks ending at #{} (spec {})",
        batch.blocks.len(),
        head,
        batch.blocks.last().unwrap().spec_version
    );
}

/// Asserts the live finalized-block stream yields at least one decoded block.
#[tokio::test]
#[ignore = "network: subscribes to Unit mainnet finalized stream; run with --ignored"]
async fn streams_one_finalized_block() {
    let source = SubxtSource::connect(SourceConfig::new(ws_url()))
        .await
        .expect("connect");

    let batch = source.next_finalized().await.expect("next finalized");
    assert_eq!(batch.blocks.len(), 1, "stream delivers one block per call");
    let b = &batch.blocks[0];
    assert!(b.spec_version > 0);
    assert!(b.id.hash.starts_with("0x"));
    println!(
        "OK: streamed finalized block #{} ({})",
        b.id.number, b.id.hash
    );
}
