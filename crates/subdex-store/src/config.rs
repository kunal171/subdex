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
}

impl StoreConfig {
    /// Create a config for `url` with a sensible default pool size (5).
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            max_connections: 5,
        }
    }

    /// Override the maximum pool size.
    pub fn with_max_connections(mut self, n: u32) -> Self {
        self.max_connections = n.max(1);
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
        let c = c.with_max_connections(20);
        assert_eq!(c.max_connections, 20);
    }

    #[test]
    fn max_connections_floored_at_one() {
        let c = StoreConfig::new("postgres://localhost/subdex").with_max_connections(0);
        assert_eq!(c.max_connections, 1, "pool size must be at least 1");
    }
}
