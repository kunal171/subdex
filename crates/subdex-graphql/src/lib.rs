//! # subdex-graphql
//!
//! A small GraphQL serving toolkit (`async-graphql` + `axum`) for subdex
//! indexers.
//!
//! subdex is **code-first**: users define their own tables in their handlers, so
//! there is no central entity schema to auto-generate an API from (unlike
//! schema-first frameworks). Instead this crate gives you:
//!
//! - a ready **server harness** that serves any `async-graphql` `Schema` over
//!   HTTP with a GraphiQL playground (you write your query resolvers in Rust,
//!   backed by the same Postgres pool the [`PgStore`](subdex_store::PgStore)
//!   uses), and
//! - a built-in **indexer-status** query over the framework's own `subdex_block`
//!   bookkeeping, useful out of the box for every indexer.
//!
//! Built incrementally on `feat/graphql`: this commit adds the crate scaffold and
//! [`GraphqlConfig`]; the server harness and the status query follow.

mod config;

pub use config::GraphqlConfig;
