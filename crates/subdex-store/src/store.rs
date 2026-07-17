//! [`PgStore`]: the Postgres-backed [`Store`] implementation.
//!
//! Owns the framework bookkeeping in the `subdex_block` table: the progress
//! cursor (the max-height row), reorg detection (stored hash per height), and
//! reorg rollback (delete rows above a fork height). Handler entity writes share
//! the [`PgStore::Tx`] transaction so they commit atomically with the cursor.

use crate::config::StoreConfig;
use crate::schema::MIGRATOR;
use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Postgres, Transaction};
use subdex_core::{Block, BlockId, BlockNumber, Result, Store, SubdexError};

/// A Postgres-backed store. Cheap to clone (holds a connection pool handle).
#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
    /// Retained reorg window: rows more than this many blocks below the cursor
    /// are pruned on commit. `0` disables pruning. See [`StoreConfig::reorg_retention`].
    reorg_retention: u32,
}

impl PgStore {
    /// Connect to Postgres using `config`, building a bounded connection pool.
    pub async fn connect(config: StoreConfig) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .connect(&config.url)
            .await
            .map_err(|e| SubdexError::Store(format!("connect: {e}")))?;
        Ok(Self {
            pool,
            reorg_retention: config.reorg_retention,
        })
    }

    /// Build a store from an already-constructed pool (useful for tests that
    /// manage their own pool/lifecycle). Pruning is disabled (retention 0); use
    /// [`with_reorg_retention`](PgStore::with_reorg_retention) to set a window.
    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            pool,
            reorg_retention: 0,
        }
    }

    /// Set the retained reorg window on a store built via [`from_pool`](PgStore::from_pool).
    pub fn with_reorg_retention(mut self, blocks: u32) -> Self {
        self.reorg_retention = blocks;
        self
    }

    /// Access the underlying pool (e.g. for a handler's own `init` to create its
    /// entity tables against the same database).
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Run a **handler's own** versioned migrations, tracked separately from the
    /// framework's bookkeeping.
    ///
    /// A handler embeds its migrations with [`sqlx::migrate!`] (a `migrations/`
    /// directory of `NNNN_name.sql` files) and calls this from its
    /// [`init`](subdex_core::Handler::init). The migrations apply **once, in
    /// order**, and their applied versions are recorded in a per-handler table
    /// `_sqlx_migrations_<name>` — isolated from the framework's own
    /// `_sqlx_migrations` (which tracks `subdex_block`) so the two never collide.
    /// Re-running is idempotent: already-applied versions are skipped, so a fresh
    /// DB and an upgraded DB converge.
    ///
    /// `name` identifies the handler's migration set; it is sanitized to a safe
    /// SQL identifier (see [`handler_migrations_table`]).
    ///
    /// ```no_run
    /// # async fn ex(store: &subdex_store::PgStore) -> subdex_core::Result<()> {
    /// static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
    /// store.run_handler_migrations(&MIGRATOR, "transfers").await?;
    /// # Ok(()) }
    /// ```
    pub async fn run_handler_migrations(
        &self,
        migrator: &sqlx::migrate::Migrator,
        name: &str,
    ) -> Result<()> {
        let table = handler_migrations_table(name);
        // `Migrator` isn't `Clone`, and the caller's is a `&'static` — so build an
        // owned copy (all fields are Cow/bool) and point *that* at the handler's
        // own tracking table, leaving the caller's untouched.
        let mut m = sqlx::migrate::Migrator {
            migrations: migrator.migrations.clone(),
            ignore_missing: migrator.ignore_missing,
            locking: migrator.locking,
            no_tx: migrator.no_tx,
            table_name: migrator.table_name.clone(),
            create_schemas: migrator.create_schemas.clone(),
        };
        m.dangerous_set_table_name(table);
        m.run(&self.pool)
            .await
            .map_err(|e| store_err(&format!("handler migrations `{name}`"), e.into()))?;
        Ok(())
    }
}

/// The per-handler migration-tracking table name for `name`, isolated from the
/// framework's `_sqlx_migrations`. `name` is reduced to `[a-z0-9_]` (other
/// characters become `_`) so it's always a safe, unquoted SQL identifier; an
/// empty/failing name falls back to `handler`.
pub fn handler_migrations_table(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let base = if sanitized.is_empty() {
        "handler"
    } else {
        &sanitized
    };
    format!("_sqlx_migrations_{base}")
}

/// Map any sqlx error into the framework's store error.
fn store_err(context: &str, e: sqlx::Error) -> SubdexError {
    SubdexError::Store(format!("{context}: {e}"))
}

#[async_trait]
impl Store for PgStore {
    /// A live Postgres transaction. Handlers receive `&mut Self::Tx` and run
    /// their entity writes on it; the same transaction carries the cursor
    /// advance, so the whole block commits atomically.
    type Tx<'a> = Transaction<'a, Postgres>;

    async fn init(&self) -> Result<()> {
        MIGRATOR
            .run(&self.pool)
            .await
            .map_err(|e| SubdexError::Store(format!("migrate: {e}")))?;
        Ok(())
    }

    async fn cursor(&self) -> Result<Option<BlockId>> {
        // The cursor is the highest indexed block.
        let row: Option<(i64, String)> =
            sqlx::query_as("SELECT height, hash FROM subdex_block ORDER BY height DESC LIMIT 1")
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| store_err("cursor", e))?;
        Ok(row.map(|(height, hash)| BlockId {
            number: height as BlockNumber,
            hash,
        }))
    }

    async fn hash_at(&self, height: BlockNumber) -> Result<Option<String>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT hash FROM subdex_block WHERE height = $1")
                .bind(height as i64)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| store_err("hash_at", e))?;
        Ok(row.map(|(hash,)| hash))
    }

    async fn begin<'a>(&'a self) -> Result<Self::Tx<'a>> {
        self.pool.begin().await.map_err(|e| store_err("begin", e))
    }

    async fn set_cursor<'a>(&self, tx: &mut Self::Tx<'a>, block: &Block) -> Result<()> {
        // Insert the block row that advances the cursor, recording the full
        // reorg/observability metadata. ON CONFLICT keeps it idempotent if the
        // same height is re-committed (e.g. after a rollback re-indexes it).
        sqlx::query(
            "INSERT INTO subdex_block (height, hash, parent_hash, timestamp, spec_version) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (height) DO UPDATE SET \
                 hash = EXCLUDED.hash, \
                 parent_hash = EXCLUDED.parent_hash, \
                 timestamp = EXCLUDED.timestamp, \
                 spec_version = EXCLUDED.spec_version, \
                 indexed_at = now()",
        )
        .bind(block.id.number as i64)
        .bind(&block.id.hash)
        .bind(&block.parent_hash)
        .bind(block.timestamp.map(|t| t as i64))
        .bind(block.spec_version as i64)
        .execute(&mut **tx)
        .await
        .map_err(|e| store_err("set_cursor", e))?;

        // Prune old bookkeeping rows that are below the retained reorg window.
        // These are never read again (reorg checks only look back a bounded
        // number of blocks), so dropping them keeps `subdex_block` bounded instead
        // of growing one row per block forever. Done on the SAME transaction, so
        // it commits atomically with the cursor advance and adds no round-trip.
        // `reorg_retention == 0` disables it. `saturating_sub` avoids pruning
        // anything until the cursor climbs past the window.
        if self.reorg_retention > 0 {
            let prune_below = block.id.number.saturating_sub(self.reorg_retention);
            if prune_below > 0 {
                sqlx::query("DELETE FROM subdex_block WHERE height < $1")
                    .bind(prune_below as i64)
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| store_err("prune", e))?;
            }
        }
        Ok(())
    }

    async fn commit<'a>(&self, tx: Self::Tx<'a>) -> Result<()> {
        tx.commit().await.map_err(|e| store_err("commit", e))
    }

    async fn rollback_to(&self, height: BlockNumber) -> Result<()> {
        // Delete bookkeeping rows strictly above `height`. Handler entity rows
        // are rolled back by the processor (which knows the user tables); this
        // method owns only the framework's own table. Done in its own
        // transaction so it is atomic.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| store_err("rollback begin", e))?;
        sqlx::query("DELETE FROM subdex_block WHERE height > $1")
            .bind(height as i64)
            .execute(&mut *tx)
            .await
            .map_err(|e| store_err("rollback delete", e))?;
        tx.commit()
            .await
            .map_err(|e| store_err("rollback commit", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::handler_migrations_table;

    #[test]
    fn migration_table_is_a_safe_identifier() {
        assert_eq!(
            handler_migrations_table("transfers"),
            "_sqlx_migrations_transfers"
        );
        // Uppercase + dashes/dots/spaces are normalized to a safe identifier.
        assert_eq!(
            handler_migrations_table("My-Handler.v2 x"),
            "_sqlx_migrations_my_handler_v2_x"
        );
    }

    #[test]
    fn empty_name_falls_back() {
        assert_eq!(handler_migrations_table(""), "_sqlx_migrations_handler");
        assert_eq!(handler_migrations_table("!!!"), "_sqlx_migrations____");
    }

    #[test]
    fn distinct_names_get_distinct_tables() {
        assert_ne!(
            handler_migrations_table("balances"),
            handler_migrations_table("assets")
        );
    }
}
