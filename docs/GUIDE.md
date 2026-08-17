# Build an indexer on your chain

A top-to-bottom guide to building a [subdex](https://github.com/kunal171/subdex/blob/main/README.md) indexer for any
Substrate chain — then a reference to return to. It covers the choices at each
step, so you can start minimal and grow.

**The shortest path:** clone the [starter template](https://github.com/kunal171/subdex/tree/main/templates/starter), point
`WS_URL` at your chain, `cargo run`. Everything below explains what that template
does and how to take it further.

```bash
# from templates/starter/
docker compose up -d
WS_URL=wss://rpc.polkadot.io cargo run     # backfill + follow + GraphQL on :4350
```

## The mental model

An indexer is one `Handler` you write, plus wiring the framework provides:

```
 DataSource ──decoded Block──► Handler(s) ──rows──► Store ──► Postgres ──► GraphQL
 (RPC/portal)                  (your code)          (cursor + reorg + txn)
                        └──────── the Processor engine runs the loop ───────┘
```

- **You write:** a `schema.graphql` (your tables) and a `Handler` (event → rows).
- **The framework does:** fetch + decode blocks, drive backfill/follow, detect
  reorgs, commit each block **atomically** with the cursor, and serve GraphQL.
- **`subdex-codegen`** turns your schema into the entity structs, migration, and
  GraphQL API, so you only write the event→entity mapping.

---

## 1. Choose a data source

A `DataSource` produces decoded blocks. Three ship with subdex — pick by whether
you need speed, live-follow, or both.

| Source | Backfill | Live tip | Works on | Use when |
|---|---|---|---|---|
| **`SubxtSource`** (default) | slow (RPC) | ✅ | **any** Substrate chain | Getting started; live indexing; a chain with no portal dataset |
| **`SqdPortalSource`** (`sqd` feature) | **fast** (columnar) | ❌ | chains with an SQD dataset | Large historical backfill |
| **`HybridSource`** (`sqd` feature) | **fast** | ✅ | chains with an SQD dataset | Production: fast catch-up *and* live-follow |

- **`SubxtSource`** talks RPC over WebSocket. It decodes each block against **its
  own** spec-version metadata, so a backfill across a runtime upgrade stays
  correct with no per-chain codegen. It's latency-bound — throughput is capped by
  the node (tens of blocks/sec on a public endpoint). Start here.
- **`SqdPortalSource`** pulls pre-decoded, columnar, batched history from the SQD
  portal — far faster for backfill than per-block RPC (how much faster depends on
  the field selection — see [step 8](#8-backfill-fast)), but it **can't stream a
  live Substrate tip**.
- **`HybridSource::new(portal, rpc)`** composes them: portal for the historical
  sweep, RPC to follow the tip. This is the production shape.

```rust
// Default: direct RPC (any chain).
let source = SubxtSource::connect(cfg.source_config()).await?;

// Fast backfill + live follow (needs the `sqd` feature):
// let source = HybridSource::new(
//     SqdPortalSource::connect(SqdConfig::new("https://portal.sqd.dev", "polkadot"))?,
//     SubxtSource::connect(cfg.source_config()).await?,
// );
```

See [README § Data sources](https://github.com/kunal171/subdex/blob/main/README.md#data-sources) for the full trade-offs.

---

## 2. Define your schema

Describe your tables in `schema.graphql` with the `@entity` dialect, then generate
the Rust + SQL + GraphQL:

```graphql
type Transfer @entity {
    id: ID!                    # required primary key (TEXT)
    blockHeight: Int! @index   # @index → a non-unique index
    from: String!
    to: String                 # nullable → Option / NULL
    amount: BigInt!            # decimal string / NUMERIC (balances exceed i64)
}

enum Direction { DEPOSIT WITHDRAW }
```

Supported: `@entity` types (a required `id: ID!`), scalars (`ID`, `String`, `Int`,
`BigInt`, `Float`, `Boolean`, `DateTime`, `Bytes`, `JSON`), scalar list fields
(→ Postgres arrays), `@index` / `@unique`, `enum`s, and a field typed as another
`@entity` (stored as that entity's id string — no joins in v1).

Generate:

```bash
subdex-codegen generate schema.graphql --out src/generated   # or: just codegen
```

This writes, all carrying a DO-NOT-EDIT header:
- `src/generated/entities.rs` — one struct per entity **+ a typed `.upsert()`**;
- `migrations/0001_schema.sql` — `CREATE TABLE` + indexes;
- `src/generated/graphql.rs` — an async-graphql read API (list + count per entity).

Commit the generated files. Edit the schema and re-run — never hand-edit output.
For a big schema, split it across `schema/*.graphql` and pass the directory.

> **Prefer hand-written tables?** You don't have to use codegen. Create tables in
> your handler's `init` and write rows with plain `sqlx`. The
> [`transfers`](https://github.com/kunal171/subdex/tree/main/examples/transfers) example does exactly that. Codegen just
> removes the boilerplate.

---

## 3. Write a handler

The handler is the one piece that's yours. The pattern is
**load → extract → process → persist**:

```rust
async fn process_block<'a>(&self, block: &Block, tx: &mut <PgStore as Store>::Tx<'a>)
    -> Result<()>
{
    for ev in &block.events {
        if (ev.pallet.as_str(), ev.name.as_str()) != ("Balances", "Transfer") { continue; }
        let row = Transfer {                                   // process
            id: format!("{}-{}", block.id.number, ev.index),
            block_height: block.id.number as i32,
            from: field_account_ss58(&ev.fields, "from", 42).unwrap_or_default(),  // extract
            to: field_account_ss58(&ev.fields, "to", 42),
            amount: field_bigint(&ev.fields, "amount").unwrap_or_else(|| "0".into()),
        };
        row.upsert(&mut **tx).await?;                          // persist (atomic w/ cursor)
    }
    Ok(())
}
```

### Reading event fields (the value helpers)

A decoded event's `fields` is a dynamic `scale_value::Value`. `subdex_source::value`
gives you total (never-panic) readers:

| Helper | Reads |
|---|---|
| `field_u128` / `field_bigint` | integer / decimal-string balance |
| `field_bool` / `field_str` | primitive bool / string |
| `field_account_ss58(v, name, prefix)` | account → SS58 (`42` Substrate, `0` Polkadot) |
| `field_hex(v, name)` | opaque bytes → `0x…` (**NUL-safe** — use for tx hashes) |
| `require_fields(v, &[…])` | error if a field is missing (vs silently NULL) |

### Which write path? (three options)

The `Handler` trait has three methods; implement the one that fits.

| Method | Use when | How it writes |
|---|---|---|
| **`process_block`** | Simple: a few rows per block | One call per block; write rows on `tx` |
| **`process_batch`** | You want one multi-row INSERT per batch | One call per batch of blocks |
| **`prepare` + `Prepared::write`** | Heavy compute per block | `prepare` runs **concurrently** with other handlers (no tx); `write` does the serial bulk insert |

- Start with **`process_block`** — it's the simplest and the generated `.upsert()`
  is already efficient.
- Reach for **`process_batch`** to collapse many rows into a single multi-row
  `INSERT` (fewer round-trips on a heavy backfill).
- Use the **two-phase `prepare`/`write`** when per-block work is CPU-heavy: the
  engine runs every handler's `prepare` concurrently, then writes serially on the
  shared transaction. See the [`multi-pallet`](https://github.com/kunal171/subdex/tree/main/examples/multi-pallet) example,
  which shows a simple handler and a two-phase handler committing **together**.

Whichever you pick, all writes ride the transaction the store hands you, so a
block's rows and the cursor advance commit **all-or-nothing**.

---

## 4. Migrations

Codegen writes `migrations/0001_schema.sql`. Apply it once at startup from your
handler's `init`:

```rust
static MIGRATOR: subdex_store::Migrator = sqlx::migrate!("./migrations");

async fn init(&self, store: &PgStore) -> Result<()> {
    store.run_handler_migrations(&MIGRATOR, self.name()).await
}
```

Migrations apply **once, in order**, tracked in a per-handler table
(`_sqlx_migrations_<name>`) isolated from the framework's own. Re-running is
idempotent, so a fresh DB and an upgraded DB converge.

**Evolving the schema:** edit `schema.graphql`, re-run codegen (it rewrites
`0001_schema.sql`), and for an *already-deployed* DB add a new
`migrations/0002_*.sql` with the delta (e.g. `ALTER TABLE … ADD COLUMN …`) rather
than editing `0001`. New migrations reach existing deployments exactly once.

---

## 5. Configuration

Config layers: an optional `subdex.toml` (`[source]/[store]/[processor]` tables),
overlaid by environment variables, with a local `.env` auto-loaded. `WS_URL` and
`DATABASE_URL` are the only required values.

| Env var | TOML | Meaning |
|---|---|---|
| `WS_URL` | `source.url` | **Required.** Chain WS endpoint. |
| `DATABASE_URL` | `store.url` | **Required.** Postgres. |
| `BATCH_SIZE` | `source.batch_size` + `processor.batch_size` | Blocks per fetch / per commit. |
| `CONCURRENCY` | `source.concurrency` | In-flight block fetches (hides RPC latency). |
| `SS58_PREFIX` | `source.ss58_prefix` | Address format (42 Substrate, 0 Polkadot). |
| `STRICT` | `source.strict` | `1` = a decode failure aborts the block (vs tolerate). |
| `REORG_RETENTION` | `store.reorg_retention` | `(height,hash)` rows kept for reorg checks. |
| `START_HEIGHT` | `processor.start_height` | Backfill start on a fresh DB. |
| `MAX_REORG_DEPTH` | `processor.max_reorg_depth` | Bound on reorg rewind depth. |

Load it with one call:

```rust
let cfg = IndexerConfig::load()?;   // TOML + env + .env
```

---

## 6. Serve GraphQL

Codegen's `graphql.rs` gives you a `QueryRoot` (a list + count per entity, plus
the framework's `indexerStatus`). Serve it over the same pool the indexer writes
to:

```rust
let schema = Schema::build(QueryRoot::default(), EmptyMutation, EmptySubscription)
    .data(pool)                              // the PgStore pool
    .finish();
tokio::spawn(serve_graphql(schema, GraphqlConfig::on_port(4350)));
```

Then query `http://localhost:4350/graphql`:

```graphql
{ transfers(limit: 10) { id blockHeight from amount }
  transfersCount
  indexerStatus { height indexedBlocks specVersion } }
```

You can hand-add resolvers by merging your own `#[Object]` into the root — the
generated types are ordinary async-graphql objects.

---

## 7. Observe

Enable the `metrics` feature to get a Prometheus observer and a `/metrics`
endpoint:

```toml
subdex = { git = "…", features = ["metrics"] }
```

```rust
let processor = Processor::new(source, store, handlers, cfg)
    .with_observer(install_prometheus(9000)?);   // PrometheusObserver + /metrics on :9000
```

It exposes head-lag (`head − cursor`), blocks/events indexed, commit durations,
reorg counts, and fetch timings. Without the feature, the `ProcessorObserver`
trait is still there — implement it for custom logging/alerting (it's the hook
behind head-lag detection and the DEFER_INDEXES head transition).

---

## 8. Backfill fast

Public RPC is the usual bottleneck. In order of impact:

1. **Use the portal for backfill** — `SqdPortalSource` / `HybridSource` are much
   faster than per-block RPC when a dataset exists for your chain (step 1), since
   the portal serves columnar, batched history instead of one round trip per block.
2. **Fetch only what you use** — `DataSelection::events_only()` skips the
   extrinsics fetch; on the portal, a narrow field selection is a fraction of the
   payload (the single biggest lever on portal throughput).
3. **Raise `CONCURRENCY`** — more in-flight fetches hide RPC latency (respect the
   endpoint's limits).
4. **Defer indexes** — drop a table's non-PK indexes during backfill and recreate
   them at head, so bulk inserts skip index maintenance. Opt in from `init`:

   ```rust
   store.defer_index(
       "transfers_block_height_idx",
       "CREATE INDEX IF NOT EXISTS transfers_block_height_idx ON transfers (block_height)",
   ).await?;
   ```

   It only drops on a **fresh** DB (a no-op on resume), records the pending set in
   the database (so a crash mid-backfill still recreates), and the framework
   recreates them when backfill reaches head. **Never defer the primary key or a
   `UNIQUE` index you upsert on** — those must exist during backfill for
   correctness.
5. **Batch commits** — `BATCH_SIZE` controls blocks per transaction; larger
   batches amortize commit overhead (at the cost of a larger unit of retry).

---

## 9. Operate

- **Resumable.** The `(height, hash)` cursor survives restarts; the indexer picks
  up exactly where it left off, and the stored hash detects a reorg that happened
  while it was down.
- **Reorg-safe.** On a fork it walks to the true common ancestor and rolls back
  once (bounded by `MAX_REORG_DEPTH`), then re-indexes the canonical chain.
- **Atomic.** A block's rows and the cursor commit together — a crash never leaves
  a half-indexed block.
- **Strict vs tolerant decode.** By default a per-item decode failure logs +
  counts + writes an empty value; set `STRICT=1` to make it a hard error (good for
  CI / correctness runs).
- **Docker.** The template's `docker-compose.yml` runs Postgres; the repo's
  top-level `Dockerfile` packages an indexer as a slim image (see
  [README § Docker](https://github.com/kunal171/subdex/blob/main/README.md#or-run-it-all-in-docker)).

---

## Where to go next

- The runnable [starter template](https://github.com/kunal171/subdex/tree/main/templates/starter) — clone and edit.
- [`examples/transfers`](https://github.com/kunal171/subdex/tree/main/examples/transfers) — a single hand-written handler.
- [`examples/multi-pallet`](https://github.com/kunal171/subdex/tree/main/examples/multi-pallet) — two handlers, two write
  patterns, one atomic commit.
- [README](https://github.com/kunal171/subdex/blob/main/README.md) — architecture, data-source trade-offs, reorg handling.
- [RFC 034](rfcs/034-schema-first-codegen.md) — the design behind the codegen and
  builder DX.
