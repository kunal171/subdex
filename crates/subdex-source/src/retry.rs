//! Bounded retry-with-backoff for the source's network operations.
//!
//! Direct RPC calls fail transiently all the time (timeouts, dropped sockets,
//! node restarts, rate-limits). [`retry_async`] re-runs a fallible async op with
//! exponential backoff + jitter until it succeeds or the [`RetryConfig`] budget
//! is exhausted, so one blip doesn't abort the whole run. Only **transient**
//! errors are retried — see [`is_transient`].

use crate::config::RetryConfig;
use std::future::Future;
use std::time::Duration;
use subdex_core::{Result, SubdexError};

/// Is this error worth retrying? Network/connection failures (`Source`) are
/// transient — the node may recover. A `Decode` error is a genuine data/logic
/// problem that retrying won't fix, so it fails fast. Everything else is treated
/// as non-transient by default (conservative: don't loop on a real bug).
pub(crate) fn is_transient(err: &SubdexError) -> bool {
    matches!(err, SubdexError::Source(_))
}

/// Add up to ~25% pseudo-random jitter to a delay, so many concurrent retries
/// don't hammer the node in lockstep. Uses a cheap time-seeded value — we don't
/// need cryptographic randomness, just de-synchronization.
fn jitter(delay: Duration) -> Duration {
    if delay.is_zero() {
        return delay;
    }
    let base = delay.as_millis() as u64;
    // Nanosecond fraction of "now" gives a spread-out, dependency-free source.
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let extra = (base / 4).saturating_mul(now_nanos % 1000) / 1000; // 0..=25%
    Duration::from_millis(base.saturating_add(extra))
}

/// Run `op`, retrying transient failures per `cfg`. `label` names the operation
/// for log context (e.g. `"fetch_batch"`). On a transient error it logs a `warn`
/// with the attempt number and backoff, sleeps, and tries again; on a
/// non-transient error, or once retries are exhausted, it returns the last error.
pub(crate) async fn retry_async<T, F, Fut>(cfg: RetryConfig, label: &str, mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut attempt: u32 = 0;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                // Give up on non-transient errors or once the budget is spent.
                if !is_transient(&e) || attempt >= cfg.max_retries {
                    return Err(e);
                }
                let delay = jitter(cfg.backoff(attempt));
                tracing::warn!(
                    op = label,
                    attempt = attempt + 1,
                    max = cfg.max_retries,
                    delay_ms = delay.as_millis() as u64,
                    error = %e,
                    "transient source error; retrying after backoff"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// A config with near-zero delays so tests don't actually wait.
    fn fast_cfg(max_retries: u32) -> RetryConfig {
        RetryConfig {
            max_retries,
            base_delay: Duration::from_millis(0),
            max_delay: Duration::from_millis(0),
        }
    }

    #[tokio::test]
    async fn succeeds_first_try_without_retrying() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let out: Result<u32> = retry_async(fast_cfg(5), "op", || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(42)
            }
        })
        .await;
        assert_eq!(out.unwrap(), 42);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no retries when it works");
    }

    #[tokio::test]
    async fn retries_transient_then_succeeds() {
        // Fail transiently twice, then succeed on the third attempt.
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let out: Result<u32> = retry_async(fast_cfg(5), "op", || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(SubdexError::Source("timeout".into()))
                } else {
                    Ok(7)
                }
            }
        })
        .await;
        assert_eq!(out.unwrap(), 7);
        assert_eq!(calls.load(Ordering::SeqCst), 3, "2 failures + 1 success");
    }

    #[tokio::test]
    async fn gives_up_after_max_retries() {
        // Always transient: 1 initial + max_retries attempts, then error out.
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let out: Result<u32> = retry_async(fast_cfg(3), "op", || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(SubdexError::Source("down".into()))
            }
        })
        .await;
        assert!(out.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "1 initial attempt + 3 retries"
        );
    }

    #[tokio::test]
    async fn does_not_retry_non_transient() {
        // A decode error is permanent — fail immediately, no retries.
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let out: Result<u32> = retry_async(fast_cfg(5), "op", || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(SubdexError::Decode("bad".into()))
            }
        })
        .await;
        assert!(out.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no retry on decode error");
    }

    #[test]
    fn backoff_grows_and_caps() {
        let cfg = RetryConfig {
            max_retries: 10,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(1000),
        };
        assert_eq!(cfg.backoff(0), Duration::from_millis(100));
        assert_eq!(cfg.backoff(1), Duration::from_millis(200));
        assert_eq!(cfg.backoff(2), Duration::from_millis(400));
        assert_eq!(cfg.backoff(3), Duration::from_millis(800));
        // 1600 would exceed the 1000 cap.
        assert_eq!(cfg.backoff(4), Duration::from_millis(1000));
        assert_eq!(
            cfg.backoff(20),
            Duration::from_millis(1000),
            "saturates, no overflow"
        );
    }
}
