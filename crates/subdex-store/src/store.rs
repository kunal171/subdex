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
