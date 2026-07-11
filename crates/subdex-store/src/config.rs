//! Configuration for the Postgres-backed [`PgStore`](crate::PgStore).

/// Connection settings for [`PgStore`](crate::PgStore).
#[derive(Clone, Debug)]
pub struct StoreConfig {
    /// Postgres connection URL, e.g.
    /// `postgres://user:pass@localhost:5432/subdex`.
    pub url: String,
    /// Maximum number of pooled connections. Defaults to 5 via
    /// [`StoreConfig::new`].
    pub max_connections: u32,
    /// How many recent block rows to retain in the `subdex_block` bookkeeping
    /// table for reorg detection. On each commit, rows more than this many blocks
    /// below the cursor are pruned — they're never read again (reorg checks only
    /// look back a bounded window, and subdex indexes finalized blocks). This
    /// keeps the table bounded on a multi-million-block chain instead of growing
    /// forever.
    ///
    /// **Must be ≥ your `max_reorg_depth`** (default 64) so a reorg's fork point
    /// is still in the table; the default (5000) is comfortably above that.
    /// `0` disables pruning (keep the full audit trail). Defaults to 5000.
    pub reorg_retention: u32,
}

impl StoreConfig {
    /// Create a config for `url` with sensible defaults (pool size 5, reorg
    /// retention 5000 blocks).
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            max_connections: 5,
            reorg_retention: 5000,
        }
    }

    /// Override the maximum pool size.
    pub fn with_max_connections(mut self, n: u32) -> Self {
        self.max_connections = n.max(1);
        self
    }

    /// Override the retained reorg window (`0` disables pruning — keep all rows).
    pub fn with_reorg_retention(mut self, blocks: u32) -> Self {
        self.reorg_retention = blocks;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let c = StoreConfig::new("postgres://localhost/subdex");
        assert_eq!(c.max_connections, 5);
        assert_eq!(c.reorg_retention, 5000);
        let c = c.with_max_connections(20).with_reorg_retention(1000);
        assert_eq!(c.max_connections, 20);
        assert_eq!(c.reorg_retention, 1000);
    }

    #[test]
    fn max_connections_floored_at_one() {
        let c = StoreConfig::new("postgres://localhost/subdex").with_max_connections(0);
        assert_eq!(c.max_connections, 1, "pool size must be at least 1");
    }
}
