# Code Walkthrough

A detailed, file-by-file tour of the codebase. Read [Architecture](./architecture.md)
first for the high-level model; this document explains the actual implementation.

Crates, in dependency order:

- [`subdex-core`](#subdex-core) — traits + types (no deps)
- [`subdex-source`](#subdex-source) — subxt RPC `DataSource`
- [`subdex-store`](#subdex-store) — Postgres `Store`
- [`subdex`](#subdex) — the engine (`Processor`)
- [`subdex-graphql`](#subdex-graphql) — GraphQL serving
- [`examples/transfers`](#examplestransfers) — a complete example

---

## `subdex-core`

The foundation: contracts and types with **no async-runtime or database
dependencies**. Everything else implements these.

### `types.rs` — the data model

Defines the chain-agnostic shapes that flow through the pipeline: `Block`,
`Extrinsic`, `Event`, `BlockBatch`, and `BlockId`.

Two design decisions to note:

- **`BlockId { number, hash }`** carries the hash, not just the height. Reorg
  detection depends on this — on a fork, height `N` exists with two *different*
  hashes, so height alone can't tell you the chain changed.
- **`Block.spec_version`** is explicit. This is the field that enables
  upgrade-correct decoding: the source records which runtime version each block
  was decoded under, and downstream nothing has to care about upgrades.

Event/extrinsic payloads are `scale_value::Value` — a *dynamically* decoded value,
so the type works for any chain without compile-time knowledge of its types.

### `source.rs`, `store.rs`, `handler.rs` — the three traits

These are the contracts described in [Architecture](./architecture.md#the-three-core-abstractions).
Worth highlighting from the source:

- **`Handler<S: Store>`** is generic over the concrete store so `process_block`
  receives `S::Tx` — the *real* transaction handle. This is how your writes end
  up on the same transaction as the cursor advance. The default `init` is a no-op
  so simple handlers can omit it.
- **`Store::set_cursor`** takes the full `&Block` (not just a `BlockId`) precisely
  so a store can persist `parent_hash` — which it needs later to detect reorgs.
- **`Store::Tx<'a>`** is a GAT (generic associated type), letting each store
  define its own transaction type (`sqlx::Transaction` for Postgres).

### `error.rs` — `SubdexError`

A single error enum. The notable variant is the structured
`Reorg { height, expected, got }`, which carries the fork height so the rollback
knows how far to go.

---

## `subdex-source`

A `DataSource` backed by a direct WebSocket RPC connection via `subxt`. Concrete
to subxt's `PolkadotConfig` (H256 hashes, `u32` block numbers, `MultiAddress`),
which matches most Substrate chains (H256 hashes, u32 block numbers, MultiAddress).

### `source.rs` — `SubxtSource`

Implements the three `DataSource` methods:

- `finalized_head()` → `client.at_current_block()` (subxt's "current finalized
  block") and reads its number.
- `fetch_batch(from, to)` → fetches each block by height via `client.at_block(h)`
  and maps it; caps the returned span to `config.batch_size`.
- `next_finalized()` → lazily opens `client.stream_blocks()` (subxt's finalized
  stream) and yields the next block, mapped.

The finalized stream is stored behind a `futures::lock::Mutex` so
`next_finalized(&self)` can advance it without `&mut self`.

### `mapping.rs` — the decode core

This is where raw chain data becomes the framework's `Block`. The key function,
`map_block`, takes a subxt `ClientAtBlock` (a client *positioned at one block*,
which crucially carries the metadata for **that block's** spec version) and:

1. reads `number`, `hash`, `parent_hash` (from the concrete `SubstrateHeader`),
   and `spec_version` (from the per-block client — this is the upgrade-correctness
   mechanism);
2. decodes every **extrinsic** via `decode_call_data_fields_unchecked_as::<Value>`
   into a dynamic `scale_value::Value`, and derives per-extrinsic **success** by
   scanning `System.ExtrinsicSuccess` / `ExtrinsicFailed` events at the matching
   phase;
3. decodes every **event** via `decode_fields_unchecked_as::<Value>`, linking each
   to its originating extrinsic via the event `Phase::ApplyExtrinsic(index)`;
4. extracts the block **timestamp** from the `Timestamp.set` inherent's argument.

Because the decode is *dynamic* (`scale_value::Value`) and uses the per-block
metadata, it needs no generated types and is correct across upgrades. Decode
failures on an individual extrinsic/event are tolerated (recorded as an empty
value) rather than aborting the whole block.

**Data selection.** `SourceConfig.selection` (`DataSelection { events, extrinsics }`,
default both) controls what `map_block` fetches. An events-only indexer can set
`DataSelection::events_only()` to skip the extrinsics fetch+decode entirely (the
block `timestamp`, derived from the `Timestamp.set` extrinsic, is then `None`).
The header is always fetched — its `parent_hash` is required for reorg-safety.

**Retry + reconnect.** Direct RPC fails transiently — timeouts, dropped sockets,
node restarts, rate-limits. Each network op (`finalized_head`, per-block fetch,
`next_finalized`) is wrapped in `retry_async`, which retries **transient** errors
(`SubdexError::Source`) with exponential backoff + jitter per `SourceConfig.retry`
(`RetryConfig`, default 5 retries / 250ms→30s). Decode errors fail fast — retrying
a genuine data bug won't help. When the finalized stream errors, the subscription
is dropped so the retry **re-subscribes**, which is the reconnect path. So a single
blip no longer aborts the run; only an exhausted budget does.

### Tests

- Offline unit tests for the timestamp/value-walk helpers.
- Two `#[ignore]`d live tests against a live Substrate chain
  ([`tests/live_chain.rs`](../crates/subdex-source/tests/live_chain.rs)) that fetch
  real blocks and assert contiguity, parent-hash chaining, `Timestamp.set`
  presence, and non-empty decoded event names.

---

## `subdex-store`

The Postgres `Store`, via `sqlx`. Owns the framework's bookkeeping.

### `migrations/0001_bookkeeping.sql` — the schema

One table, `subdex_block`, namespaced with the `subdex_` prefix so it never
collides with your tables:

```sql
subdex_block(height PK, hash, parent_hash, timestamp, spec_version, indexed_at)
```

It is **both** the cursor (the max-height row is "where we are") and the
reorg-detection record (compare an incoming block's `parent_hash` to the stored
hash of its parent height).

### `schema.rs` — embedded migrator

`MIGRATOR` uses `sqlx::migrate!("./migrations")`, which embeds the SQL into the
binary at compile time. `init()` runs it idempotently.

### `store.rs` — `PgStore`

Implements `Store`:

- `Tx<'a> = sqlx::Transaction<'a, Postgres>` — the real transaction handlers write
  on.
- `cursor()` → `SELECT … ORDER BY height DESC LIMIT 1`.
- `hash_at(height)` → `SELECT hash WHERE height = $1`.
- `set_cursor(tx, block)` → an **idempotent upsert** (`INSERT … ON CONFLICT
  (height) DO UPDATE`) recording hash/parent_hash/timestamp/spec_version. Idempotent
  so re-indexing a height after a rollback just overwrites the row.
- `rollback_to(height)` → `DELETE FROM subdex_block WHERE height > $1`, in its own
  transaction.

`pool()` exposes the underlying `PgPool` so a handler's `init` can create its own
tables (and the GraphQL layer can query) against the same database.

### Tests

[`tests/pg_store.rs`](../crates/subdex-store/tests/pg_store.rs) — `#[ignore]`d
integration tests against a real Postgres (each in an isolated throwaway DB)
covering: idempotent init, cursor + metadata recording, upsert-after-rollback, and
reorg rollback removing exactly the rows above the fork.

---

## `subdex`

The engine. [`processor.rs`](../crates/subdex/src/processor.rs) is the heart of
the framework.

### `Processor`

Generic over `Src: DataSource` and `St: Store`, holding `Vec<Arc<dyn Handler<St>>>`
and a `ProcessorConfig`.

**`commit_batch(blocks)`** — the atomic unit (one transaction per batch):

```rust
let mut tx = self.store.begin().await?;
for h in &self.handlers {
    h.process_batch(blocks, &mut tx).await?;       // all the batch's INSERTs, on tx
}
for block in blocks {
    self.store.set_cursor(&mut tx, block).await?;  // cursor + per-block hashes, on tx
}
self.store.commit(tx).await?;                       // commit the whole batch together
```

If a handler errors, `tx` is dropped → rolled back → none of the batch is
persisted. (A single-block `commit_block` exists too, as a public helper.)

**`process_batch_blocks(blocks)`** — the reorg-aware wrapper (checks the batch's
first block):

```rust
let first = blocks.first()…;
if first.id.number > 0 {
    let parent_height = first.id.number - 1;
    if let Some(stored) = self.store.hash_at(parent_height).await? {
        if stored != first.parent_hash {
            // reorg: drop the diverged tail and re-fetch from the parent height
            self.store.rollback_to(parent_height.saturating_sub(1)).await?;
            return Ok(Some(parent_height));
        }
    }
}
self.commit_batch(blocks).await?;
Ok(None)
```

Returns `Ok(Some(refetch_from))` on a reorg (telling the caller where to resume)
or `Ok(None)` on a normal commit. (`process_block` is the single-block equivalent.)

**`backfill()`** — resumes from `cursor + 1` (or `start_height`), fetches
`[resume, finalized_head]` in `batch_size` windows, and feeds each batch to
`process_batch_blocks` (one transaction per batch). On a reorg it rewinds `next`
to the returned refetch height and continues. `follow()`/`follow_until()` use the
same batch path for the tip.

**`follow(max_batches)`** — pulls `next_finalized()` batches and processes each
block, running until the stream ends or the optional bound is hit (the bound exists
so tests terminate; production passes `None`).

### `testkit.rs` — in-memory test doubles

So the engine's logic can be unit-tested offline (no chain, no DB):

- **`MemStore`** — an in-memory `Store` with a real cursor and reorg rollback; its
  `Tx` is a staged write buffer flushed on commit, discarded on drop.
- **`RecordingHandler`** — records the heights it saw; can be told to fail at a
  given height (to test rollback).
- **`ScriptedSource`** — replays a fixed list of blocks.

The 12 unit tests in `processor.rs` use these to verify atomic commit,
handler-error rollback, multi-handler fan-out, reorg detection + rollback, batched
backfill, cursor-resume, and live follow — all deterministically and offline.

### `tests/e2e.rs`

The keystone: a `#[ignore]`d end-to-end test wiring a **real** `SubxtSource`
(mainnet) + `PgStore` (Postgres) + a handler, backfilling a small window and
asserting the cursor advanced and events were written on the same transaction.

---

## `subdex-graphql`

A GraphQL serving toolkit (`async-graphql` + `axum`). Because subdex is
code-first, this is a *toolkit*, not an auto-generated API.

### `status.rs` — the built-in status query

`StatusQuery` exposes an `indexerStatus` field returning `IndexerStatus`
(`height`, `hash`, `specVersion`, `blockTimestamp`, `indexedBlocks`), read from
`subdex_block` in one round-trip. Every indexer gets this for free; the `PgPool`
is pulled from the schema's context data.

### `server.rs` — the HTTP harness

- `router(schema, config)` → an axum `Router` serving `GET {path}` (the GraphiQL
  playground) and `POST {path}` (query execution via `async_graphql_axum::GraphQL`).
- `serve(schema, config)` → bind and run.
- `build_status_schema(pool)` → a ready schema serving just `StatusQuery`.

It's generic over any `(Query, Mutation, Subscription)`, so you compose your own
resolvers with `StatusQuery` and serve the combined schema.

### `config.rs`

`GraphqlConfig { addr, path }`, defaulting to `0.0.0.0:4350` `/graphql` (4350
matches the port Subsquid's GraphQL server conventionally uses).

### `tests/serve.rs`

A `#[ignore]`d test that boots the server on an ephemeral port against a real
Postgres and queries `indexerStatus` over real HTTP (via `reqwest`), asserting the
response reflects the seeded cursor.

---

## `examples/transfers`

A complete, runnable indexer — the canonical "how do I use this." It records
`Assets.Deposited` / `Assets.Withdrawn` events into a `transfers` table.

### `value_ext.rs` — reading event fields

Helpers to pull typed values out of an event's dynamic `scale_value::Value`:

- `field(value, name)` — look up a named field.
- `as_u128(value)` — coerce an integer primitive (incl. a `U256` that fits).
- `as_account_ss58(value)` — collect the 32 account bytes and render them as a
  Substrate **SS58** address (the `5…` form). This handles a real subtlety: an
  `AccountId32` decodes as a *newtype-wrapped* byte array
  (`Unnamed([ Unnamed([u8; 32]) ])`), so `collect_bytes` unwraps the single-element
  wrapper layer before reading bytes. (This was found by actually running the
  example — see the git history.)

### `handler.rs` — `TransfersHandler`

Implements `Handler<PgStore>`:

- `init` creates the `transfers` table (with `UNIQUE(block_height, event_index)`
  so re-indexing is idempotent).
- `process_block` maps the two event names to a `direction` label, extracts
  `asset_id` / `who` / `amount` via `value_ext`, and inserts one row per matching
  event on the processor's transaction (`ON CONFLICT DO NOTHING`). `amount` is
  bound as text cast to `numeric` to avoid i64 overflow on large balances.

### `main.rs` — wiring

Env-configured (`WS_URL`, `DATABASE_URL`, `START_HEIGHT`, `FOLLOW`, `RUST_LOG`):
connect `SubxtSource` + `PgStore`, build a `Processor` with `TransfersHandler`,
then `init → backfill → follow`. ~80 lines, and it's a real indexer.

---

Next: **[Data Flow →](./data-flow.md)** — a step-by-step trace of one block end to end.
