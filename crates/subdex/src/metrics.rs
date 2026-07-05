//! Prometheus metrics observer (behind the `metrics` feature).
//!
//! [`PrometheusObserver`] implements [`ProcessorObserver`] by recording the
//! engine's run-loop events into the [`metrics`] facade — counters, gauges, and
//! histograms an operator can scrape and alert on. [`install_prometheus`] wires
//! up the Prometheus exporter and starts an HTTP `/metrics` listener.
//!
//! Enable with `--features metrics`. When the feature is off, none of this is
//! compiled and the engine's default [`NoopObserver`](subdex_core::NoopObserver)
//! costs nothing.
//!
//! ## Exported series
//!
//! | Metric | Type | Meaning |
//! |---|---|---|
//! | `subdex_cursor_height` | gauge | Highest committed block height |
//! | `subdex_finalized_head` | gauge | Latest finalized head seen |
//! | `subdex_head_lag` | gauge | `head - cursor` (how far behind the tip) |
//! | `subdex_blocks_processed_total` | counter | Blocks committed |
//! | `subdex_events_decoded_total` | counter | Events decoded + committed |
//! | `subdex_reorgs_total` | counter | Reorgs detected + rolled back |
//! | `subdex_reorg_depth` | histogram | Blocks rolled back per reorg |
//! | `subdex_batch_commit_seconds` | histogram | Commit-transaction duration |
//! | `subdex_fetch_seconds` | histogram | Batch fetch duration |
//! | `subdex_errors_total` | counter | Run-loop errors (labelled by `context`) |

use metrics::{counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram};
use metrics_exporter_prometheus::PrometheusBuilder;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use subdex_core::{BlockNumber, ProcessorObserver};

/// A [`ProcessorObserver`] that records engine events as Prometheus metrics.
///
/// Attach it with [`Processor::with_observer`](crate::Processor::with_observer)
/// after calling [`install_prometheus`] (which starts the scrape endpoint).
/// Tracks cursor + head internally so it can publish the derived `head_lag`.
#[derive(Debug, Default)]
pub struct PrometheusObserver {
    cursor: AtomicU64,
    head: AtomicU64,
}

impl PrometheusObserver {
    /// A fresh observer. Call [`install_prometheus`] once (globally) to expose
    /// the `/metrics` endpoint; construct one of these per `Processor`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Recompute and publish `subdex_head_lag = max(head - cursor, 0)`.
    fn publish_lag(&self) {
        let cursor = self.cursor.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Relaxed);
        gauge!("subdex_head_lag").set(head.saturating_sub(cursor) as f64);
    }
}

impl ProcessorObserver for PrometheusObserver {
    fn on_batch_committed(
        &self,
        cursor: BlockNumber,
        count: usize,
        events: usize,
        commit: Duration,
    ) {
        self.cursor.store(cursor as u64, Ordering::Relaxed);
        gauge!("subdex_cursor_height").set(cursor as f64);
        counter!("subdex_blocks_processed_total").increment(count as u64);
        counter!("subdex_events_decoded_total").increment(events as u64);
        histogram!("subdex_batch_commit_seconds").record(commit.as_secs_f64());
        self.publish_lag();
    }

    fn on_reorg(&self, _fork_height: BlockNumber, depth: u32) {
        counter!("subdex_reorgs_total").increment(1);
        histogram!("subdex_reorg_depth").record(depth as f64);
    }

    fn on_head(&self, head: BlockNumber) {
        self.head.store(head as u64, Ordering::Relaxed);
        gauge!("subdex_finalized_head").set(head as f64);
        self.publish_lag();
    }

    fn on_fetch(&self, _count: usize, elapsed: Duration) {
        histogram!("subdex_fetch_seconds").record(elapsed.as_secs_f64());
    }

    fn on_error(&self, context: &str, _error: &str) {
        counter!("subdex_errors_total", "context" => context.to_string()).increment(1);
    }
}

/// Install the Prometheus recorder and start an HTTP `/metrics` listener on
/// `addr` (e.g. `"0.0.0.0:9000".parse()`). Call this **once** at startup, before
/// constructing observers. Also registers metric descriptions (help text).
///
/// Returns an error if a recorder is already installed or the listener can't bind.
pub fn install_prometheus(addr: SocketAddr) -> Result<(), String> {
    PrometheusBuilder::new()
        .with_http_listener(addr)
        .install()
        .map_err(|e| format!("install prometheus exporter on {addr}: {e}"))?;
    describe_metrics();
    Ok(())
}

/// Register help text for each series (shown in the `/metrics` output).
fn describe_metrics() {
    describe_gauge!("subdex_cursor_height", "Highest committed block height");
    describe_gauge!("subdex_finalized_head", "Latest finalized head seen");
    describe_gauge!(
        "subdex_head_lag",
        "Blocks behind the finalized tip (head - cursor)"
    );
    describe_counter!("subdex_blocks_processed_total", "Blocks committed");
    describe_counter!(
        "subdex_events_decoded_total",
        "Events decoded and committed"
    );
    describe_counter!("subdex_reorgs_total", "Reorgs detected and rolled back");
    describe_histogram!("subdex_reorg_depth", "Blocks rolled back per reorg");
    describe_histogram!(
        "subdex_batch_commit_seconds",
        "Commit-transaction duration (s)"
    );
    describe_histogram!("subdex_fetch_seconds", "Batch fetch duration (s)");
    describe_counter!(
        "subdex_errors_total",
        "Run-loop errors, labelled by context"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observer_records_without_a_recorder_installed() {
        // The `metrics` macros are no-ops when no recorder is installed, so the
        // observer's hooks must not panic even outside a real exporter. This also
        // exercises the internal cursor/head/lag bookkeeping.
        let o = PrometheusObserver::new();
        o.on_head(100);
        o.on_batch_committed(40, 10, 55, Duration::from_millis(5));
        o.on_fetch(10, Duration::from_millis(20));
        o.on_reorg(30, 3);
        o.on_error("commit", "boom");

        // Internal state tracks head and cursor for the lag gauge.
        assert_eq!(o.head.load(Ordering::Relaxed), 100);
        assert_eq!(o.cursor.load(Ordering::Relaxed), 40);
    }
}
