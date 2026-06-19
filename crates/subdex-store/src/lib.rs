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

pub use config::StoreConfig;
