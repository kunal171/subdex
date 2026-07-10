<div align="center">

# subdex

**A general-purpose, code-first blockchain indexer framework for [Substrate](https://substrate.io) chains — written in Rust.**

[![CI](https://github.com/kunal171/subdex/actions/workflows/ci.yml/badge.svg)](https://github.com/kunal171/subdex/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-1.96%2B-orange)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](./LICENSE)
[![Status](https://img.shields.io/badge/status-alpha-yellow)](#project-status)

*subdex is to Substrate what [Subsquid/SQD](https://sqd.dev) is — but Rust-native end to end: you implement a `Handler` trait in plain Rust, define your own tables, and the framework drives a resumable, reorg-safe pipeline from the chain into Postgres, with an optional GraphQL API.*

</div>

---

## Table of contents

- [Why subdex](#why-subdex)
- [Architecture](#architecture)
- [Quickstart — run the example](#quickstart--run-the-example)
- [Write your own indexer](#write-your-own-indexer)
- [Serve a GraphQL API](#serve-a-graphql-api)
- [Crates](#crates)
- [Configuration](#configuration)
- [Reorgs & finality](#reorgs--finality)
- [Testing](#testing)
- [Documentation](#documentation)
- [Project status](#project-status)
- [License](#license)

---

## Why subdex

Indexers that decode against a **single pinned runtime metadata** silently break
when a chain upgrades — storage layouts, event shapes, and call encodings drift,
and your indexer keeps "working" while writing wrong data. subdex avoids this by:

- **Decoding each block against the metadata for _its own_ spec version** — so it
  stays correct across runtime upgrades automatically, with no per-chain codegen.
- **Being written in the same language as Substrate itself** — chain types can be
  shared rather than re-derived, eliminating an entire class of indexer/runtime
  drift bugs.
- **Code-first ergonomics** — you write a small Rust `Handler` and define your own
  tables. No schema DSL, no codegen step, full type safety and the whole Rust
  ecosystem at your disposal.

It is **resumable** (a `(height, hash)` cursor survives restarts), **reorg-safe**
(it validates parent hashes and rolls back on forks), and **atomic** (your writes
commit on the same transaction as the cursor advance — never half-applied).

---

## Architecture

Three composable traits (defined in `subdex-core`) form the pipeline. You
implement **`Handler`**; the framework provides the rest.

```
        ┌──────────────────────────────────────────────────────────────┐
        │                       Substrate chain                        │
        │                       (RPC / WSS)                           │
        └───────────────────────────┬──────────────────────────────────┘
                                     │
                       ┌─────────────▼──────────────┐
                       │        DataSource          │   subdex-source
                       │  (subxt RPC, per-spec      │   → decodes any chain
                       │   metadata decoding)       │
                       └─────────────┬──────────────┘
                                     │  Block { events, extrinsics, spec_version, … }
                       ┌─────────────▼──────────────┐
                       │        Processor           │   subdex
                       │  resume · backfill · follow │   the engine
                       │  reorg detect + rollback   │
                       └─────────────┬──────────────┘
                                     │  one Block at a time, in a txn
                       ┌─────────────▼──────────────┐
        YOU WRITE ───▶ │         Handler(s)         │   your code
                       │  block → your table rows   │
                       └─────────────┬──────────────┘
                                     │  writes on the store txn
                       ┌─────────────▼──────────────┐
                       │           Store            │   subdex-store
                       │  cursor · hashes · reorg   │   → Postgres (sqlx)
                       │  rollback (atomic commit)  │
                       └─────────────┬──────────────┘
                                     │
                       ┌─────────────▼──────────────┐
                       │          Postgres          │
                       └─────────────┬──────────────┘
                                     │
                       ┌─────────────▼──────────────┐
                       │       GraphQL API          │   subdex-graphql
                       │  async-graphql + axum      │   (optional)
                       │  + GraphiQL playground     │
                       └────────────────────────────┘
```

| Trait | Role | Default implementation |
|---|---|---|
| `DataSource` | Produces decoded blocks for a range + the live finalized tip | direct RPC via `subxt` (`subdex-source`) |
| `Handler` | **You implement this** — turn a block into your rows | — |
| `Store` | Owns the cursor + reorg rollback; hands handlers a txn | Postgres via `sqlx` (`subdex-store`) |

Because each is a trait, the pieces are swappable — e.g. a future SQD-portal
`DataSource` for faster backfill plugs in without touching your handlers.

---

## Quickstart — run the example

The fastest way to see subdex work is the bundled [`transfers`](./examples/transfers)
example, which indexes `Assets.Deposited` / `Assets.Withdrawn` events into Postgres.

**Prerequisites:** Rust ≥ 1.96, Docker (for Postgres).

```bash
# 1. A Postgres to index into
docker run -d --name subdex-db \
    -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=subdex \
    -p 55432:5432 postgres:16-alpine

# 2. Configure (WS_URL + DATABASE_URL are required; a local .env is auto-loaded
#    from the directory you run cargo in — the repo root here)
cp examples/transfers/.env.example .env

# 3. Run the indexer (backfills ~20 recent blocks, then follows the tip).
#    Ctrl-C to stop.
cargo run -p subdex-example-transfers
```

Then query what it indexed (e.g. in `psql` or DBeaver →
`postgres://postgres:postgres@localhost:55432/subdex`):

```sql
SELECT direction, count(*) FROM transfers GROUP BY direction;

SELECT block_height, direction, asset_id, account, amount
FROM transfers ORDER BY block_height DESC LIMIT 10;
```

```
 block_height | direction |               account                | amount
--------------+-----------+--------------------------------------+---------
      8668945 | withdraw  | 5DAbqA9t7TpVZuetzwSzGk9kdqGCRN3qw…    | 1715514
      8668945 | deposit   | 5G3tmhfoaaTwEBNGuspZ3scWr1BWUSW7V…   |       0
```

Accounts are rendered as **SS58** (`5…`) addresses, just like block explorers.

---

## Write your own indexer

An indexer is: **one `Handler`** + **wiring** (source + store + processor). Here is
a complete, minimal example end to end.

### 1. Add dependencies

```toml
[dependencies]
subdex = { git = "https://github.com/kunal171/subdex" }
subdex-source = { git = "https://github.com/kunal171/subdex" }
subdex-store = { git = "https://github.com/kunal171/subdex" }
async-trait = "0.1"
sqlx = { version = "0.9", features = ["runtime-tokio", "postgres"] }
tokio = { version = "1", features = ["full"] }
anyhow = "1"
```

### 2. Implement a `Handler`

You own your tables; create them in `init`, write rows in `process_block` using
the transaction the processor hands you (so your writes commit atomically with
the indexer cursor).

```rust
use async_trait::async_trait;
use subdex::{Block, Handler, Result, Store, SubdexError};
use subdex_store::PgStore;

struct EventCounter;

#[async_trait]
impl Handler<PgStore> for EventCounter {
    // One-time setup: create your own table.
    async fn init(&self, store: &PgStore) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS event_count (
                block_height BIGINT NOT NULL,
                pallet TEXT NOT NULL,
                events BIGINT NOT NULL,
                PRIMARY KEY (block_height, pallet))",
        )
        .execute(store.pool())
        .await
        .map_err(|e| SubdexError::Handler(e.to_string()))?;
        Ok(())
    }

    // Per block: write rows on the processor's transaction `tx`.
    async fn process_block<'a>(
        &self,
        block: &Block,
        tx: &mut <PgStore as Store>::Tx<'a>,
    ) -> Result<()> {
        use std::collections::HashMap;
        let mut by_pallet: HashMap<&str, i64> = HashMap::new();
        for ev in &block.events {
            *by_pallet.entry(ev.pallet.as_str()).or_default() += 1;
        }
        for (pallet, count) in by_pallet {
            sqlx::query(
                "INSERT INTO event_count (block_height, pallet, events)
                 VALUES ($1, $2, $3)
                 ON CONFLICT (block_height, pallet) DO UPDATE SET events = EXCLUDED.events",
            )
            .bind(block.id.number as i64)
            .bind(pallet)
            .bind(count)
            .execute(&mut **tx)
            .await
            .map_err(|e| SubdexError::Handler(e.to_string()))?;
        }
        Ok(())
    }

    fn name(&self) -> &str { "event-counter" }
}
```

> The engine commits **one transaction per batch**, so `process_block` is already
> efficient. For maximum throughput on heavy indexers, override `process_batch`
> instead — accumulate rows across the whole batch in memory and bulk-insert once
> (avoids per-row upserts). The default `process_batch` just calls `process_block`.

### 3. Wire it up and run

```rust
use std::sync::Arc;
use subdex::{DataSource, Processor, ProcessorConfig};
use subdex_source::{SourceConfig, SubxtSource};
use subdex_store::{PgStore, StoreConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let source = SubxtSource::connect(
        SourceConfig::new("wss://your-substrate-node:9944"),
    ).await?;
    let store = PgStore::connect(
        StoreConfig::new("postgres://postgres:postgres@localhost:55432/subdex"),
    ).await?;

    // Start ~50 blocks back; you'd pick a real start height for your use case.
    let head = source.finalized_head().await?;
    let start = head.saturating_sub(50);

    let processor = Processor::new(
        source,
        store,
        vec![Arc::new(EventCounter)],
        ProcessorConfig::from_height(start),
    );

    // One call: init -> backfill to the head -> follow the tip, stopping
    // cleanly on Ctrl-C.
    processor.run_until(async { let _ = tokio::signal::ctrl_c().await; }).await?;
    Ok(())
}
```

> Prefer fine-grained control? Call the phases yourself:
> `processor.init().await?` → `processor.backfill().await?` → `processor.follow(None).await?`.

That's a complete indexer. The processor resumes from the stored cursor on
restart, rolls back on reorgs, and commits each block atomically.

---

## Serve a GraphQL API

`subdex-graphql` serves any [`async-graphql`](https://docs.rs/async-graphql)
schema over HTTP (with a [GraphiQL](https://github.com/graphql/graphiql)
playground), and ships a built-in **indexer-status** query every indexer gets for
free:

```rust
use subdex_graphql::{build_status_schema, serve, GraphqlConfig};

// `pool` is the same PgPool your PgStore uses: `store.pool().clone()`.
let schema = build_status_schema(pool);
serve(schema, GraphqlConfig::default()).await?; // http://0.0.0.0:4350/graphql
```

Open `http://localhost:4350/graphql` for the playground, or query it:

```graphql
{
  indexerStatus {
    height
    hash
    specVersion
    blockTimestamp
    indexedBlocks
  }
}
```

To expose **your own** tables, write your query resolvers in plain Rust (backed by
the same pool) and compose them with `StatusQuery` (via `async_graphql`'s
`MergedObject`) into a schema, then pass it to `serve` / `router`. The
[`transfers` example](./examples/transfers) does exactly this — it indexes **and**
serves a `transfers` query alongside `indexerStatus` from a single binary.

---

## Crates

| Crate | Purpose | Status |
|---|---|---|
| [`subdex-core`](./crates/subdex-core) | Traits (`DataSource`/`Handler`/`Store`) + chain-agnostic types. No runtime/db deps. | ✅ |
| [`subdex-source`](./crates/subdex-source) | `DataSource`s: direct RPC via `subxt` (any chain), plus an SQD-portal backfill source (`sqd` feature). | ✅ |
| [`subdex-store`](./crates/subdex-store) | Postgres `Store` via `sqlx` — cursor, hashes, atomic commit, reorg rollback. | ✅ |
| [`subdex`](./crates/subdex) | The engine: backfill + live-follow run loop, reorg handling. Re-exports the core API. | ✅ |
| [`subdex-graphql`](./crates/subdex-graphql) | GraphQL serving toolkit (`async-graphql` + `axum`) + built-in status query. | ✅ |
| [`examples/transfers`](./examples/transfers) | Runnable example: index Assets deposits/withdrawals into Postgres. | ✅ |

---

## Configuration

| Component | Type | Key options |
|---|---|---|
| Source | `SourceConfig` | `url` (WSS endpoint), `batch_size`, `concurrency`, `selection` (`DataSelection` — fetch only events/extrinsics you need), `retry` (`RetryConfig` — transient-failure backoff), `ss58_prefix` (signer address prefix, default 42), `strict` (make per-item decode failures hard errors; default off) |
| Store | `StoreConfig` | `url` (Postgres), `max_connections` |
| Processor | `ProcessorConfig` | `start_height`, `batch_size`, `reorg_retention`, `max_reorg_depth` (bound the rewind on a reorg; `0` = unbounded) |
| GraphQL | `GraphqlConfig` | `addr` (default `0.0.0.0:4350`), `path` (default `/graphql`) |

The `transfers` example reads `WS_URL`, `DATABASE_URL`, `START_HEIGHT`, `FOLLOW`,
and `RUST_LOG` from the environment — see its [README](./examples/transfers/README.md).

---

## Data sources

The engine talks to any `DataSource`. Two ship in `subdex-source`:

**`SubxtSource` (default)** — direct RPC over WebSocket via `subxt`, works against
any Substrate chain, decodes each block against its own spec-version metadata, and
does both backfill and live-follow. It's latency-bound: throughput is capped by
the node (~tens of blocks/sec against a public endpoint).

**`SqdPortalSource` (`sqd` feature)** — a **backfill** source over the
[SQD (Subsquid) portal](https://docs.sqd.dev), which serves pre-decoded, columnar,
batched history far faster than per-block RPC (measured **15–50× faster** than RPC
on Polkadot; ~1,600 blk/s on a 5k-block range). Two caveats, both inherent to the
portal:

- **Backfill-only.** The portal has no live Substrate tip, so `next_finalized`
  errors — pair it with an `SubxtSource` for the live phase (a `HybridSource`
  is planned). Ideal for the historical catch-up, then hand off to RPC.
- **Decoded values are equivalent, not identical, to RPC.** The portal pre-decodes
  args to JSON; subdex bridges that to `scale_value::Value` so handlers keep the
  same type. Structural fields (heights, hashes, event names/indices, timestamps)
  match RPC exactly; complex arg shapes (enums, byte arrays) may differ.

```rust
use subdex_source::{SqdConfig, SqdPortalSource};

let source = SqdPortalSource::connect(
    SqdConfig::new("https://portal.sqd.dev", "polkadot")
        .with_batch_size(5000), // the portal rewards large ranges
)?;
```

Because both are `DataSource`s, **handlers and the engine don't change** when you
switch — that's the point of the trait seam.

---

## Reorgs & finality

The processor anchors on the chain's **finalized** head. Before committing a
block it validates that the block's `parent_hash` matches the hash stored for the
previous height:

- **Match** → commit normally (handler writes + cursor advance, atomically).
- **Mismatch** → a reorg replaced the parent; the processor walks down to the
  **true common ancestor** (comparing stored hashes against the source's canonical
  hashes), rolls back the diverged tail in one pass, and re-fetches from the fork
  point. The rewind is bounded by `max_reorg_depth` (default 64; `0` = unbounded) —
  a deeper fork errors rather than rewinding unboundedly.

Because subdex indexes finalized blocks, deep reorgs are not expected; the
parent-hash check protects against any divergence within the retained window.
On GRANDPA chains the finalized cursor is clean and unambiguous.

---

## Observability

The engine exposes its run loop through a lightweight
[`ProcessorObserver`](https://docs.rs/subdex) hook — a synchronous, backend-agnostic
trait it calls at key points: `on_batch_committed` (cursor, block/event counts,
commit time), `on_reorg` (fork height + depth), `on_head` (new finalized tip),
`on_fetch` (fetch latency), and `on_error`. Every method has a no-op default, so
the default [`NoopObserver`] costs nothing. Attach one with `.with_observer(...)`:

```rust
let processor = Processor::new(source, store, handlers, config)
    .with_observer(my_observer); // Arc<dyn ProcessorObserver>
```

Use it for a progress/ETA reporter (the [`transfers`](./examples/transfers) and
profile-indexer examples drive their progress logs this way), a test spy, or your
own dashboard feed.

### Prometheus metrics

Enable the `metrics` feature for a ready-made Prometheus observer and a `/metrics`
endpoint:

```toml
subdex = { version = "...", features = ["metrics"] }
```

```rust
use subdex::{install_prometheus, PrometheusObserver};
use std::sync::Arc;

install_prometheus("0.0.0.0:9000".parse()?)?; // serves /metrics
let processor = Processor::new(source, store, handlers, config)
    .with_observer(Arc::new(PrometheusObserver::new()));
```

Exported series: `subdex_cursor_height`, `subdex_finalized_head`,
`subdex_head_lag` (gauges); `subdex_blocks_processed_total`,
`subdex_events_decoded_total`, `subdex_reorgs_total`, `subdex_errors_total`,
`subdex_decode_failures_total` (counters); `subdex_reorg_depth`,
`subdex_batch_commit_seconds`, `subdex_fetch_seconds` (histograms). The feature is
off by default — no metrics dependencies are compiled unless you ask for them.

> The source emits `subdex_decode_failures_total` (labelled by `kind`/`pallet`)
> whenever an event's fields or an extrinsic's args fail to decode. By default the
> item is recorded with an empty value and indexing continues; set
> `SourceConfig.strict` to make such a failure a hard error instead (useful in CI
> to catch metadata drift rather than silently write empty data).

---

## Testing

```bash
# Fast, offline unit tests (no chain, no database):
cargo test

# Lint:
cargo clippy --workspace --all-targets

# Network/DB integration tests are #[ignore]d so offline/CI runs stay green.
# Run them explicitly with a chain + Postgres available:
docker run -d --name pg -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=subdex \
    -p 55432:5432 postgres:16-alpine

SUBDEX_TEST_WS=wss://your-substrate-node:9944 \
SUBDEX_TEST_DB=postgres://postgres:postgres@localhost:55432/subdex \
    cargo test --workspace -- --ignored
```

Integration tests cover: decoding real mainnet blocks (`subdex-source`), the full
store lifecycle incl. reorg rollback (`subdex-store`), an end-to-end
mainnet→Postgres run (`subdex`), and serving GraphQL over HTTP (`subdex-graphql`).

---

## Documentation

In-depth docs live in [`docs/`](./docs):

- **[Purpose & Motivation](./docs/README.md#purpose--motivation)** — why this project
  exists (runtime-upgrade drift, the Rust/TypeScript mismatch, and how subdex makes
  both problems structurally impossible).
- **[Architecture](./docs/architecture.md)** — the end-to-end design, the three
  traits, the engine, and how the guarantees (atomicity, resumability, reorg-safety,
  upgrade-correctness) are enforced.
- **[Code Walkthrough](./docs/code-walkthrough.md)** — a detailed, file-by-file
  explanation of every crate.
- **[Data Flow](./docs/data-flow.md)** — a step-by-step trace of one block from the
  chain into Postgres and out via GraphQL, including the crash-safety and reorg paths.

---

## Project status

**Alpha.** The core pipeline — ingest → process → store → serve — is complete and
proven end to end against a live Substrate chain, real Postgres, and real HTTP. APIs
may still change.

The roadmap toward a production-ready 0.2 — reliability (RPC retries, deep-reorg
handling), performance (an SQD-portal `DataSource`, store pruning), observability
(Prometheus metrics), and DX (shared config, more examples) — is tracked in the
[**v0.2 roadmap** milestone](https://github.com/kunal171/subdex/milestone/1).
Contributions welcome — issues tagged
[`good first issue`](https://github.com/kunal171/subdex/labels/good%20first%20issue)
are a friendly place to start.

---

## License

Licensed under the [Apache License, Version 2.0](./LICENSE).
