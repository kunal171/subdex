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
    // optional high-throughput path; defaults to process_block per block:
    async fn process_batch<'a>(&self, blocks: &[Block], tx: &mut S::Tx<'a>) -> Result<()> { … }
    fn name(&self) -> &str;
}
```

This is the **only** trait you implement. It is generic over the concrete `Store`
so you receive the store's real transaction handle (`S::Tx`) — for the Postgres
store that's a `sqlx::Transaction`, on which you run arbitrary SQL. There is no
schema DSL; you define your tables and write to them in plain Rust.

Implement **`process_block`** for the simplest case (called per block; writes
still commit per *batch*), or override **`process_batch`** for the highest
throughput — accumulate rows across the whole batch in memory and bulk-write
once, avoiding the per-row-upsert anti-pattern. The default `process_batch` just
calls `process_block` for each block.

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
`fetch_batch`, and commits **each fetched batch in a single transaction**
(`process_batch_blocks`). It returns once it reaches the head. This is the "catch
up to now" phase.

### Phase 3 — Follow

`follow()` pulls the source's finalized-block stream (`next_finalized`) one batch
at a time and commits each tip batch (same path as backfill). This is the "stay
live" phase; it runs until the process is stopped.

Both phases route batches through the same `process_batch_blocks`, which is where
the guarantees live.

### Observability

The engine holds an optional `Arc<dyn ProcessorObserver>` (a fourth, non-core
seam) that it calls at each run-loop event — batch committed, reorg, new head,
fetch, error. It defaults to a zero-cost `NoopObserver`, and is set via
`Processor::with_observer`. This decouples the engine from any particular metrics
or logging backend: the same hook drives a Prometheus exporter (the `metrics`
feature's `PrometheusObserver`), a progress/ETA reporter, or a test spy, without
the engine knowing which. Hooks are synchronous and expected to be cheap
(a counter bump or channel send), so they don't slow the hot batch path.

---

## The guarantees, and how they're enforced

### Atomicity — a batch is all-or-nothing

The engine commits **one transaction per batch** (not per block) — this is the
DB-side throughput lever, and it keeps the unit of atomicity a whole batch.
`commit_batch` runs in **two phases**:

1. **Prepare (concurrent, no transaction).** Every handler's `prepare(blocks)`
   runs at once (`try_join_all`) — this is pure compute (decoding the batch into
   rows), so a multi-handler indexer's decode work *overlaps* instead of summing.
   A handler that doesn't override `prepare` returns `None` and is handled by its
   `process_batch` in phase 2 (backwards-compatible).
2. **Write (serial, on one transaction).** `store.begin()` opens `tx`; then, in
   handler order, each prepared result is `write`n onto `tx` (or `process_batch`
   is run for a `None`). `store.set_cursor(&mut tx, block)` records the cursor +
   per-block hashes on the *same* `tx`; `store.commit(tx)` commits everything.

Writes stay **serial** on the one `tx` (that's what keeps the batch atomic); only
the compute overlaps. If any handler errors — in `prepare` or `write` — the
transaction is never committed (or is dropped), the cursor does **not** advance,
and **none** of the batch is persisted. A crash mid-batch leaves the database
exactly as it was before. There is no "half-indexed" state to recover from.

> The concurrency win in phase 1 needs a multi-threaded runtime, and — for heavy
> *synchronous* decoding — offloading via `spawn_blocking`. The API enables the
> overlap; the deployment realizes it.
>
> A single-block `process_block`/`commit_block` path also exists (public helper),
> but the run loop uses the batch path uniformly.

### Resumability — survive restarts

Because the cursor advance is committed atomically with the block's data, the
cursor is always consistent with what's been written. On startup, `resume_height`
reads it and continues. No checkpoint files, no replay-from-zero.

### Reorg-safety — survive forks

Before committing block `N`, `process_block` checks: does `block.parent_hash`
equal the hash we stored for height `N-1`?

- **Yes** (or we have no record of `N-1` — it's the first block, or below the
  retained window) → commit normally.
- **No** → a reorg replaced our parent. The engine finds the **true common
  ancestor** and rolls back everything above it in one pass, then re-fetches the
  corrected chain from there.

**Finding the fork point.** Rather than assume the fork is exactly one block back,
`find_fork_point` walks **down** from the divergent height, comparing the hash we
stored at each height against the source's canonical hash there, until they agree —
that height is the true ancestor. The first comparison is free (the incoming
block's `parent_hash` *is* the canonical hash at the parent height), so a 1-block
reorg needs no extra fetch; deeper divergence costs one single-block fetch per
level, done in a tight loop up front instead of one engine iteration per level.

**Bounded depth.** The walk is capped by `max_reorg_depth` (default 64; `0` =
unbounded). A fork deeper than that below the cursor is a hard error
(`ReorgTooDeep`) rather than an unbounded rewind — on a finalized-block indexer,
that much depth signals a misconfiguration (e.g. a non-finalized source), not a
real fork.

**The retained window.** The store keeps only the last `StoreConfig::reorg_retention`
block rows (default 5000; `0` = keep all): on each commit, `set_cursor` prunes
`subdex_block` rows below `cursor - retention` **on the same transaction**. Those
rows are never read again — reorg checks only look back a bounded number of
blocks — so pruning keeps the bookkeeping table bounded instead of growing one row
per block forever. Keep `reorg_retention` **≥ `max_reorg_depth`** so a reorg's fork
point is still in the table.

This keeps the database consistent with the canonical chain even when the chain
reorganizes under us. Because subdex indexes **finalized** blocks, deep reorgs
aren't expected — but the check is a correctness backstop, and on GRANDPA chains
the finalized cursor is unambiguous.

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
