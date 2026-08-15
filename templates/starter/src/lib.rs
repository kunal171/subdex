//! Your indexer crate: the generated code (from `schema.graphql`) and your
//! handler.

pub mod generated;
pub mod handler;

pub use handler::MyHandler;
