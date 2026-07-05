//! The [`ProcessorObserver`] trait: an optional, backend-agnostic hook the engine
//! calls at key points in its run loop.
//!
//! The engine emits `tracing` logs, but logs aren't machine-readable — you can't
//! alert on "indexing stalled", "reorg rate spiked", or "head lag is growing"
//! without scraping text. An observer receives those events as structured data,
//! so it can drive **any** sink: Prometheus metrics, a progress/ETA reporter, a
//! test spy, a custom dashboard feed.
//!
//! The trait is deliberately **synchronous and cheap** — hooks fire on the hot
//! batch path, so an implementation should do bounded, non-blocking work
//! (increment a counter, send on a channel). The default methods are no-ops, so
//! the engine's default observer costs nothing.

use crate::types::BlockNumber;
use std::time::Duration;

/// Receives structured events from the [`Processor`](crate) run loop.
///
/// Every method has a no-op default; implement only the ones you need. Hooks are
/// called synchronously from the engine, so keep them fast and non-blocking.
pub trait ProcessorObserver: Send + Sync {
    /// A batch of `count` blocks committed successfully in one transaction,
    /// advancing the cursor to `cursor` (the batch's last height). `events` is
    /// the number of decoded events across the batch; `commit` is how long the
    /// commit transaction took.
    fn on_batch_committed(
        &self,
        _cursor: BlockNumber,
        _count: usize,
        _events: usize,
        _commit: Duration,
    ) {
    }

    /// A reorg was detected and rolled back: the chain diverged at `fork_height`,
    /// so everything above it was dropped. `depth` is how many blocks were rolled
    /// back (0 if the fork was at the current cursor).
    fn on_reorg(&self, _fork_height: BlockNumber, _depth: u32) {}

    /// The source reported a new finalized `head`. Emitted at the start of
    /// backfill and as the tip advances, so an observer can track head-lag
    /// (`head - cursor`).
    fn on_head(&self, _head: BlockNumber) {}

    /// A fetch of `count` blocks completed, taking `elapsed` (the network-bound
    /// cost). Lets an observer track fetch latency / throughput separately from
    /// commit time.
    fn on_fetch(&self, _count: usize, _elapsed: Duration) {}

    /// The run loop surfaced an error (from the source, a handler, or the store).
    /// `context` names where it came from (e.g. `"fetch"`, `"commit"`). The engine
    /// still returns the error to its caller; this is purely for observation.
    fn on_error(&self, _context: &str, _error: &str) {}
}

/// The default observer: does nothing. Used when no observer is attached, so the
/// engine's hook calls compile away to no-ops.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopObserver;

impl ProcessorObserver for NoopObserver {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A spy observer that counts hook calls — proves the default methods are
    /// overridable and that a shared observer accumulates across calls.
    #[derive(Default)]
    struct Spy {
        batches: AtomicU32,
        reorgs: AtomicU32,
    }

    impl ProcessorObserver for Spy {
        fn on_batch_committed(&self, _c: BlockNumber, _n: usize, _e: usize, _d: Duration) {
            self.batches.fetch_add(1, Ordering::SeqCst);
        }
        fn on_reorg(&self, _f: BlockNumber, _d: u32) {
            self.reorgs.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn noop_observer_does_nothing_and_is_callable() {
        let o = NoopObserver;
        // All hooks callable, no panic, no state.
        o.on_batch_committed(10, 5, 20, Duration::from_millis(3));
        o.on_reorg(4, 2);
        o.on_head(100);
        o.on_fetch(5, Duration::from_millis(1));
        o.on_error("commit", "boom");
    }

    #[test]
    fn spy_records_overridden_hooks_only() {
        let s = Spy::default();
        s.on_batch_committed(1, 1, 1, Duration::ZERO);
        s.on_batch_committed(2, 1, 1, Duration::ZERO);
        s.on_reorg(0, 1);
        // Non-overridden hooks (on_head/on_fetch) default to no-op and don't panic.
        s.on_head(50);
        assert_eq!(s.batches.load(Ordering::SeqCst), 2);
        assert_eq!(s.reorgs.load(Ordering::SeqCst), 1);
    }
}
