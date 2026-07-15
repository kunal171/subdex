//! Example subdex indexer with **two pallets, two handlers**.
//!
//! Demonstrates the common real-world shape the single-handler `transfers`
//! example doesn't cover:
//!
//! - two independent [`Handler`](subdex::Handler)s — [`BalancesHandler`] and
//!   [`AssetsHandler`] — each owning its own table,
//! - both registered on **one** [`Processor`](subdex::Processor), so their writes
//!   and the cursor advance all commit on the **same transaction** (atomic across
//!   handlers),
//! - two decode styles: `BalancesHandler` uses the simple per-block
//!   `process_block`, while `AssetsHandler` overrides `process_batch` to
//!   **bulk-write** the whole batch in one INSERT,
//! - a GraphQL API exposing **both** entity types plus the framework's
//!   `indexerStatus`.
//!
//! Runnable as the `multi-pallet` binary; also a library so the handlers' pure
//! logic is unit-testable offline.

pub mod assets;
pub mod balances;
pub mod graphql;
pub mod value_ext;

pub use assets::AssetsHandler;
pub use balances::BalancesHandler;
pub use graphql::QueryRoot;
