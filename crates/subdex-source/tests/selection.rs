use subdex_core::DataSource;
use subdex_source::{DataSelection, SourceConfig, SubxtSource};

#[tokio::test]
#[ignore = "network: hits a live Substrate chain; run with --ignored"]
async fn events_only_skips_extrinsics() {
    let url = std::env::var("SUBDEX_TEST_WS").expect(
        "set SUBDEX_TEST_WS to a Substrate RPC endpoint, e.g. wss://your-substrate-node:9944",
    );

    // Full selection: events AND extrinsics present.
    let full = SubxtSource::connect(SourceConfig::new(&url)).await.unwrap();
    let head = full.finalized_head().await.unwrap();
    let fb = full.fetch_batch(head - 2, head).await.unwrap();
    let f = &fb.blocks[0];
    assert!(!f.extrinsics.is_empty(), "full: extrinsics present");
    assert!(
        f.timestamp.is_some(),
        "full: timestamp present (from Timestamp.set)"
    );

    // events_only: events present, extrinsics empty, timestamp None.
    let eo =
        SubxtSource::connect(SourceConfig::new(&url).with_selection(DataSelection::events_only()))
            .await
            .unwrap();
    let eb = eo.fetch_batch(head - 2, head).await.unwrap();
    let e = &eb.blocks[0];
    assert!(!e.events.is_empty(), "events_only: events still present");
    assert!(e.extrinsics.is_empty(), "events_only: extrinsics skipped");
    assert!(
        e.timestamp.is_none(),
        "events_only: timestamp None (no extrinsics)"
    );
    // Same events decoded either way.
    assert_eq!(
        e.events.len(),
        f.events.len(),
        "event count matches full selection"
    );
    println!(
        "OK: events_only fetched {} events, 0 extrinsics",
        e.events.len()
    );
}
