//! Example subdex indexer — records `Assets.Deposited` / `Assets.Withdrawn`
//! events (the most common token-movement events on Unit) into Postgres.
//!
//! This crate doubles as a library so its pure logic (decoding event fields)
//! is unit-testable offline, and as a binary (`transfers`) that runs the
//! indexer against a live chain + database.

pub mod graphql;
pub mod handler;
pub mod value_ext;

pub use graphql::QueryRoot;
pub use handler::TransfersHandler;
