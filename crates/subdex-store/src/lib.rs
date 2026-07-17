//! # subdex-store
//!
//! A Postgres-backed [`Store`](subdex_core::Store) for the subdex framework.
//!
//! The store owns the framework's *own* bookkeeping — the `(height, hash)`
//! cursor and the reorg rollback — and hands handlers a `sqlx` transaction so
//! their entity writes commit atomically with the cursor advance.
//!
//! Implemented incrementally on the `feat/store-postgres` branch: this commit
//! adds the crate scaffold and connection config; the schema, read paths,
//! transaction lifecycle, and reorg rollback follow.

mod config;
mod schema;
mod store;

pub use config::StoreConfig;
pub use schema::MIGRATOR;
pub use store::{handler_migrations_table, PgStore};

/// Re-export of sqlx's `Migrator` so handlers can run their own embedded
/// migrations (via `sqlx::migrate!`) through [`PgStore::run_handler_migrations`]
/// without depending on `sqlx` directly.
pub use sqlx::migrate::Migrator;
