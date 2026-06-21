# Architecture

This document explains how subdex is put together end to end: the core
abstractions, how data flows through them, and the guarantees the design provides.

For the *why*, see [Purpose & Motivation](./README.md#purpose--motivation). For a
line-by-line code tour, see the [Code Walkthrough](./code-walkthrough.md).

---

## The big picture

subdex is a **pipeline** built from three traits. You implement one of them
(`Handler`); the framework provides production implementations of the other two
and the engine that drives them.

```
   ┌────────────────────────────────────────────────────────────────────┐
   │                          Substrate chain                          │
   └───────────────────────────────┬────────────────────────────────────┘
                                    │ raw blocks (SCALE-encoded)
                ╔═══════════════════▼═══════════════════╗
                ║              DataSource               ║   trait
                ║   "give me decoded blocks N..M and    ║   impl: subdex-source
                ║    the live finalized tip"            ║   (subxt RPC)
                ╚═══════════════════╤═══════════════════╝
                                    │ Block { id, parent_hash, spec_version,
                                    │         events[], extrinsics[], … }
                ╔═══════════════════▼═══════════════════╗
                ║              Processor                ║   the engine
                ║   resume → backfill → follow tip      ║   crate: subdex
                ║   reorg detect → rollback             ║
                ╚═══════════════════╤═══════════════════╝
                                    │ one Block, inside a store transaction
                ╔═══════════════════▼═══════════════════╗
   YOU WRITE ──▶║               Handler                 ║   trait
                ║   block → INSERT into your tables     ║   impl: yours
                ╚═══════════════════╤═══════════════════╝
                                    │ writes share the transaction
                ╔═══════════════════▼═══════════════════╗
                ║                Store                  ║   trait
                ║   cursor · stored hashes · reorg      ║   impl: subdex-store
                ║   rollback · atomic commit            ║   (Postgres / sqlx)
                ╚═══════════════════╤═══════════════════╝
                                    │
                          ┌─────────▼─────────┐
                          │     Postgres      │
                          └─────────┬─────────┘
                                    │ (optional) read API
                          ┌─────────▼─────────┐
                          │   GraphQL server  │   crate: subdex-graphql
                          │ async-graphql+axum│
                          └───────────────────┘
```

---

## The three core abstractions

All three live in [`subdex-core`](../crates/subdex-core), which has **no async
runtime or database dependencies** — it is pure contracts and types, so anything
can implement it.

### 1. `DataSource` — where decoded blocks come from

```rust
#[async_trait]
pub trait DataSource: Send + Sync {
    async fn finalized_head(&self) -> Result<BlockNumber>;
    async fn fetch_batch(&self, from: BlockNumber, to: BlockNumber) -> Result<BlockBatch>;
    async fn next_finalized(&self) -> Result<BlockBatch>;
    fn name(&self) -> &str;
}
```

- `fetch_batch` is used for **backfill** (catching up history).
- `next_finalized` is used for **following the tip** (live indexing).
- A `DataSource` is responsible for **decoding** — turning raw SCALE bytes into
  the framework's `Block` model, against the correct per-block metadata.

The default implementation, [`SubxtSource`](../crates/subdex-source), talks to a
node over WebSocket RPC using [`subxt`](https://docs.rs/subxt). The trait exists
so other sources (an SQD-portal stream, a columnar archive) can be added without
changing anything downstream.

### 2. `Handler` — your code

```rust
#[async_trait]
pub trait Handler<S: Store>: Send + Sync {
    async fn init(&self, store: &S) -> Result<()> { Ok(()) }                  // create your tables
    async fn process_block<'a>(&self, block: &Block, tx: &mut S::Tx<'a>) -> Result<()>;  // write rows
    fn name(&self) -> &str;
}
```

This is the **only** trait you implement. It is generic over the concrete `Store`
so you receive the store's real transaction handle (`S::Tx`) — for the Postgres
store that's a `sqlx::Transaction`, on which you run arbitrary SQL. There is no
schema DSL; you define your tables and write to them in plain Rust.

The critical contract: **anything you write in `process_block` goes on the
transaction `tx`**, which the engine commits together with the cursor advance.
That is what makes indexing atomic.

### 3. `Store` — cursor, hashes, and reorg rollback

```rust
#[async_trait]
pub trait Store: Send + Sync {
    type Tx<'a>: Send where Self: 'a;
    async fn init(&self) -> Result<()>;
    async fn cursor(&self) -> Result<Option<BlockId>>;
    async fn hash_at(&self, height: BlockNumber) -> Result<Option<String>>;
    async fn begin<'a>(&'a self) -> Result<Self::Tx<'a>>;
    async fn set_cursor<'a>(&self, tx: &mut Self::Tx<'a>, block: &Block) -> Result<()>;
    async fn commit<'a>(&self, tx: Self::Tx<'a>) -> Result<()>;
    async fn rollback_to(&self, height: BlockNumber) -> Result<()>;
}
```

The store owns the framework's **own** bookkeeping — not your tables. It tracks:

- the **cursor** (`cursor()` → highest indexed block; the resume point), and
- the **hash of each recent height** (`hash_at()` → used to detect reorgs).

The default implementation, [`PgStore`](../crates/subdex-store), keeps this in a
`subdex_block` table and exposes a `sqlx::Transaction` as `Tx`, so your handler
writes and the cursor advance are one atomic unit.

---

## The engine: `Processor`

The [`Processor`](../crates/subdex) ties the three together. It is generic over a
concrete `DataSource` and `Store` and holds a list of `Arc<dyn Handler>`. Its run
loop has three phases.

### Phase 1 — Resume

`resume_height()` reads `store.cursor()`. If the store has a cursor (we've indexed
before), it resumes from `cursor + 1`. On a fresh database it starts from
`config.start_height`. **This is what makes restarts safe** — the engine always
picks up exactly where it left off.

### Phase 2 — Backfill

`backfill()` fetches `[resume, finalized_head]` in `batch_size` windows via
`fetch_batch`, processing each block. It returns once it reaches the head. This is
the "catch up to now" phase.

### Phase 3 — Follow

`follow()` pulls the source's finalized-block stream (`next_finalized`) one batch
at a time and processes each block as it's finalized. This is the "stay live"
phase; it runs until the process is stopped.

Both phases route every block through the same `process_block`, which is where the
guarantees live.

---

## The guarantees, and how they're enforced

### Atomicity — a block is all-or-nothing

`commit_block` does, in order:

1. `store.begin()` → open a transaction `tx`.
2. for each handler: `handler.process_block(block, &mut tx)` → your INSERTs go on `tx`.
3. `store.set_cursor(&mut tx, block)` → the cursor advance goes on the *same* `tx`.
4. `store.commit(tx)` → commit everything together.

If any handler returns `Err`, `tx` is dropped — Postgres rolls it back, the cursor
does **not** advance, and **nothing** is persisted. A crash mid-block leaves the
database exactly as it was before the block. There is no "half-indexed block"
state to recover from.

### Resumability — survive restarts

Because the cursor advance is committed atomically with the block's data, the
cursor is always consistent with what's been written. On startup, `resume_height`
reads it and continues. No checkpoint files, no replay-from-zero.

### Reorg-safety — survive forks

Before committing block `N`, `process_block` checks: does `block.parent_hash`
equal the hash we stored for height `N-1`?

- **Yes** (or we have no record of `N-1` — it's the first block, or below the
  retained window) → commit normally.
- **No** → a reorg replaced our parent. The store **rolls back** everything above
  the fork point, and the engine re-fetches the corrected chain from there.

This keeps the database consistent with the canonical chain even when the chain
reorganizes under us. Because subdex indexes **finalized** blocks, deep reorgs
aren't expected — but the check is a correctness backstop, and on GRANDPA chains
(like Unit) the finalized cursor is unambiguous.

### Upgrade-correctness — survive runtime upgrades

Each `Block` carries the `spec_version` it was authored under, and the
`DataSource` decodes each block against the metadata for *that* spec. A backfill
that spans a runtime upgrade decodes the old blocks under the old metadata and the
new blocks under the new metadata, transparently. This is the property that
distinguishes subdex from pinned-metadata indexers — it's correct across upgrades
**by construction**, not by remembering to regenerate types.

---

## The data model

The chain-agnostic types that flow through the pipeline (in
[`subdex-core/types.rs`](../crates/subdex-core/src/types.rs)):

- **`Block`** — `id: BlockId`, `parent_hash`, `timestamp`, `spec_version`,
  `finalized`, `extrinsics: Vec<Extrinsic>`, `events: Vec<Event>`.
- **`BlockId`** — `{ number, hash }`. Both fields matter: the *hash* is what makes
  reorg detection possible (the same height can appear with different hashes
  across a fork).
- **`Extrinsic`** — `index`, `pallet`, `call`, `args` (a dynamic
  `scale_value::Value`), `signed`, `signer`, `success`.
- **`Event`** — `index`, `pallet`, `name`, `fields` (dynamic `Value`),
  `extrinsic_index`.

Decoded values are dynamic `scale_value::Value`s, which is what lets the framework
work for **any** chain without compile-time knowledge of its types. A handler
reads the fields it cares about by name (see the example's
[`value_ext`](../examples/transfers/src/value_ext.rs)).

---

## Why traits everywhere?

Every seam is a trait so the pieces are independently replaceable:

- Swap `SubxtSource` for an SQD-portal `DataSource` → faster backfill, same handlers.
- Swap `PgStore` for a different `Store` → different database, same engine.
- Add/remove `Handler`s → change what you index, nothing else moves.

The engine and your handlers never depend on a concrete source or store beyond the
trait, so the framework grows without churn.

---

Next: **[Code Walkthrough →](./code-walkthrough.md)** · **[Data Flow →](./data-flow.md)**
