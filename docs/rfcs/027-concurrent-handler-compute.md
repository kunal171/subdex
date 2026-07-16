# RFC: Concurrent handler compute (#27)

## Goal
When an indexer registers several independent handlers (one per pallet, as in the
`multi-pallet` example), let their **compute** (decoding a batch into rows) run
concurrently, while their **writes** stay serialized on the one shared
transaction — so atomicity is unchanged but wall-clock batch latency drops from
the *sum* of the handlers' work toward the *max*.

## Where we are today
`commit_batch` runs handlers strictly in sequence, each on `&mut tx`:

```rust
for h in &self.handlers {
    h.process_batch(blocks, &mut tx).await?;   // decode + write, interleaved
}
```

Two reasons it's sequential:
1. **Shared `&mut tx`.** Only one handler can hold the transaction mutably at a
   time — this is *required* for atomicity (one tx = all-or-nothing).
2. **Compute and write are fused.** `process_batch` both decodes rows *and* writes
   them, so there's no pure-compute phase to parallelize independently of the tx.

The compute (walking `blocks`, reading `scale_value::Value` fields, building row
structs) is pure CPU and touches no shared state — it's embarrassingly
parallelizable. Only the DB writes need the serial tx.

## The core problem: heterogeneous row types
The issue suggests `prepare(&[Block]) -> Rows` then `write(rows, &mut tx)`. The
snag: **each handler's `Rows` is a different type** (the `multi-pallet` example's
`BalancesHandler` yields balance rows, `AssetsHandler` yields asset rows). A
`Vec<Arc<dyn Handler>>` needs a *uniform* trait-object interface, but a trait
method can't return each impl's own concrete `Rows` type through `dyn`.

So the two-phase split has to **keep the prepared rows inside the handler** (not
hand a typed `Rows` back to the engine). Three ways to do that:

### Option A — associated `Prepared` type (rejected for `dyn`)
```rust
trait Handler { type Prepared; async fn prepare(&self, &[Block]) -> Self::Prepared; ... }
```
Clean and fully typed, but an associated type makes `Handler` **not object-safe**
— `Vec<Arc<dyn Handler>>` no longer compiles. The whole engine is built on
`dyn Handler`. Rejected.

### Option B — the handler carries its own prepared state via interior mutability (rejected)
`prepare(&self, &[Block])` stashes rows in a `Mutex<Option<Prepared>>` on the
handler, `write(&self, &mut tx)` drains it. Object-safe, but: handlers become
stateful/non-reentrant, the `Mutex` is a footgun across batches, and it's easy to
misuse (prepare/write ordering, one batch at a time). Rejected — too subtle.

### Option C — two object-safe methods, engine parallelizes `prepare` (chosen)
Add to the trait, both object-safe (no associated types, no return of handler-owned
types across the boundary):

```rust
/// Phase 1 — PURE compute. Decode the batch into whatever the handler will write,
/// stored inside a boxed, type-erased carrier the handler alone understands.
/// No `tx`, no `&mut self`: safe to run for all handlers concurrently.
async fn prepare<'a>(&self, blocks: &[Block]) -> Result<Box<dyn Prepared<S>>>;

/// Phase 2 — SERIAL write. Consume the phase-1 output on the shared tx.
async fn write<'a>(&self, prepared: Box<dyn Prepared<S>>, tx: &mut S::Tx<'a>) -> Result<()>;
```

where `Prepared<S>` is a tiny object-safe trait the handler downcasts (via
`Any`) — or, simpler, `write` is a method **on the prepared object itself**:

```rust
#[async_trait]
pub trait Prepared<S: Store>: Send {
    /// Write the pre-computed rows onto the shared transaction (serial phase).
    async fn write<'a>(self: Box<Self>, tx: &mut S::Tx<'a>) -> Result<()>;
}

#[async_trait]
pub trait Handler<S: Store>: Send + Sync {
    // ... existing init / process_block / process_batch (unchanged) ...

    /// Two-phase entry point. Default: run the existing `process_batch` in the
    /// write phase (no concurrency) — so **existing handlers keep working**.
    async fn prepare<'a>(&self, blocks: &[Block]) -> Result<Box<dyn Prepared<S>>> {
        // Fallback carrier that just replays process_batch under the tx.
        Ok(Box::new(DeferToProcessBatch { blocks: blocks.to_vec() /* Arc */ }))
    }
}
```

This makes `write` live on the `Prepared` object (which *is* the handler's typed
rows, boxed) — so there's no downcast and no associated type on `Handler`. The
engine holds `Vec<Box<dyn Prepared<S>>>` between the phases, uniformly.

## Engine change (`commit_batch`)
```rust
let mut tx = store.begin().await?;

// Phase 1: prepare ALL handlers concurrently (pure compute, no tx).
let prepared: Vec<Box<dyn Prepared<S>>> =
    futures::future::try_join_all(self.handlers.iter().map(|h| h.prepare(blocks))).await?;

// Phase 2: write each in order on the shared tx (serial — atomicity preserved).
for p in prepared {
    p.write(&mut tx).await?;   // first Err drops tx => whole batch rolls back
}

for block in blocks { store.set_cursor(&mut tx, block).await?; }
store.commit(tx).await?;
```

- **Concurrency** is real for the compute phase (`try_join_all`). On a multi-core
  runtime the pure decoding overlaps; the writes are still serial (they must be).
- **Atomicity unchanged**: a `prepare` error short-circuits before any write; a
  `write` error drops `tx` → the whole batch rolls back. Same guarantee as today.
- **Backwards-compatible**: `Handler::prepare` has a default that defers to the
  existing `process_batch`, so **every current handler works untouched** — it just
  doesn't overlap until it opts into a real `prepare`.

> Note: `try_join_all` on a single-threaded runtime (or CPU-bound sync decode)
> won't magically parallelize — the win needs a multi-threaded tokio runtime and
> `prepare` bodies that are actually async/yield or are offloaded (e.g.
> `spawn_blocking` for heavy sync decode). We document that; the API enables it,
> the deployment realizes it.

## Acceptance criteria (from the issue) → how this meets them
- [x] Independent handlers' compute overlaps → `try_join_all` over `prepare`.
- [x] Atomicity unchanged → writes still serial on one tx; any error rolls back all.
- [x] Backwards-compatible default → `prepare` defaults to the existing path;
  single-phase handlers keep working.

## Scope for the first PR
- Add the `Prepared<S>` trait + `Handler::prepare` default to `subdex-core`.
- Switch `commit_batch` to prepare-concurrently / write-serially.
- Convert **one** example handler (the `multi-pallet` `AssetsHandler`, which
  already accumulates rows) to the two-phase API as the dogfood + a test proving
  overlap (e.g. two handlers that each sleep in `prepare` finish in ~max, not sum).
- Docs: architecture "Atomicity" section + the `Handler` trait docs.

## Open questions for review
1. **Is `Prepared<S>` worth the extra trait**, or is the simpler
   "return `Box<dyn FnOnce(&mut Tx)>`"-style closure cleaner? (Lifetime of `&mut
   Tx` across an async boundary makes the closure form awkward — leaning trait.)
2. **`spawn_blocking` for sync decode**: do we offer a helper, or leave it to the
   handler author? (Lean: leave it, document it — keeps the core runtime-agnostic.)
3. Do we deprecate `process_batch` eventually, or keep both paths indefinitely?
   (Lean: keep both — `process_batch` is the simple path, `prepare`/`write` the
   concurrent one.)
