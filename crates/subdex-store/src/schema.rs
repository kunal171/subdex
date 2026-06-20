//! Embedded framework migrations.
//!
//! The bookkeeping schema (the `subdex_block` cursor/reorg table) is shipped
//! inside the binary via [`sqlx::migrate!`], which reads the `migrations/`
//! directory at compile time. Running [`MIGRATOR`] against a pool creates the
//! schema idempotently and records applied versions in `_sqlx_migrations`.
//!
//! User entity tables are NOT managed here — handlers own their own schema.

/// The framework's embedded migrator (the `subdex_` bookkeeping tables).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[cfg(test)]
mod tests {
    use super::MIGRATOR;

    /// The bookkeeping migration is embedded and discoverable. This runs offline
    /// (no database) — it only inspects the compile-time-embedded migration set,
    /// guarding against an accidentally-empty or mis-pathed `migrations/` dir.
    #[test]
    fn embeds_the_bookkeeping_migration() {
        let versions: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
        assert!(
            versions.contains(&1),
            "expected migration version 1 (0001_bookkeeping), got {versions:?}"
        );
        assert!(
            MIGRATOR.iter().any(|m| m.description.contains("bookkeeping")),
            "migration description should identify the bookkeeping schema"
        );
    }
}
