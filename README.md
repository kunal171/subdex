<div align="center">

# subdex

**A general-purpose, code-first blockchain indexer framework for [Substrate](https://substrate.io) chains — written in Rust.**

[![CI](https://github.com/kunal171/subdex/actions/workflows/ci.yml/badge.svg)](https://github.com/kunal171/subdex/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-1.96%2B-orange)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](./LICENSE)
[![Status](https://img.shields.io/badge/status-alpha-yellow)](#project-status)

*subdex is to Substrate what [Subsquid/SQD](https://sqd.dev) is — but Rust-native end to end: you implement a `Handler` trait in plain Rust, define your own tables, and the framework drives a resumable, reorg-safe pipeline from the chain into Postgres, with an optional GraphQL API.*

**📖 [Documentation](https://kunal171.github.io/subdex/) · [Build an indexer](./docs/GUIDE.md) · [Starter template](./templates/starter/)**

</div>

---

## Why subdex

Indexers that decode against a **single pinned runtime metadata** silently break
when a chain upgrades — storage layouts, event shapes, and call encodings drift,
and your indexer keeps "working" while writing wrong data. subdex avoids this by:

- **Decoding each block against the metadata for _its own_ spec version** — so it
  stays correct across runtime upgrades automatically, with no per-chain codegen.
- **Being written in the same language as Substrate itself** — chain types can be
  shared rather than re-derived, eliminating an entire class of drift bugs.
- **Code-first ergonomics** — you write a small Rust `Handler` and own your tables.
  Prefer schema-first? An **optional** `schema.graphql` generates the structs,
  migrations, typed upserts, and a GraphQL API — decoding always stays dynamic, so
  upgrade-correctness is never traded away.

It is **resumable** (a `(height, hash)` cursor survives restarts), **reorg-safe**
(it walks to the true common ancestor and rolls back), and **atomic** (your writes
commit on the same transaction as the cursor advance — never half-applied).

---

## Architecture

Three composable traits (in `subdex-core`) form the pipeline. You implement
**`Handler`**; the framework provides the rest.

```
   DataSource ──decoded Block──► Handler(s) ──rows──► Store ──► Postgres ──► GraphQL
   (RPC / SQD portal)            (your code)      (cursor + reorg + txn)
                    └─────── the Processor engine runs the loop ───────┘
```

| Trait | Role | Default implementation |
|---|---|---|
| `DataSource` | Produces decoded blocks for a range + the live finalized tip | RPC via `subxt`, or the SQD portal (`subdex-source`) |
| `Handler` | **You implement this** — turn a block into your rows | — |
| `Store` | Owns the cursor + reorg rollback; hands handlers a txn | Postgres via `sqlx` (`subdex-store`) |

Each is a trait, so the pieces are swappable — the portal source and the metrics
observer both dropped in without touching the engine or any handler.

→ [Full architecture](./docs/architecture.md) · [Data flow](./docs/data-flow.md)

---

## Quickstart

**Prerequisites:** Rust ≥ 1.96, Docker (for Postgres).

```bash
# 1. A Postgres to index into
docker run -d --name subdex-db \
    -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=subdex \
    -p 55432:5432 postgres:16-alpine

# 2. Configure (WS_URL + DATABASE_URL are required; a local .env is auto-loaded)
cp examples/transfers/.env.example .env

# 3. Run the indexer — backfills, then follows the tip. Ctrl-C to stop.
cargo run -p subdex-example-transfers
```

It indexes `Assets.Deposited` / `Assets.Withdrawn` into a `transfers` table and
serves GraphQL at `http://localhost:4350/graphql`. Accounts render as **SS58**
(`5…`) addresses, just like block explorers.

**Or run it all in Docker** — Postgres *and* the indexer, no local Rust needed:

```bash
WS_URL=wss://rpc.polkadot.io docker compose up --build
```

---

## Build your own

Scaffold a complete, runnable project in one command:

```bash
subdex-codegen new my-indexer
cd my-indexer && docker compose up -d
WS_URL=wss://rpc.polkadot.io cargo run
```

Then describe your tables in `schema.graphql` and regenerate:

```graphql
type Transfer @entity {
  id: ID!
  blockHeight: Int! @index
  from: String!
  amount: BigInt!
}
```

```bash
subdex-codegen generate schema.graphql --out src/generated
```

That writes entity structs **with typed `.upsert()` helpers**, the SQL migration,
and an `async-graphql` read API. Your handler just maps events to those structs.

→ **[Build an indexer on your chain](./docs/GUIDE.md)** — the full walkthrough, with
the options at every step (data source, schema, handler patterns, migrations,
config, GraphQL, observability, fast backfill, operations).

---

## Crates

| Crate | Purpose |
|---|---|
| [`subdex-core`](./crates/subdex-core) | Traits (`DataSource`/`Handler`/`Store`) + chain-agnostic types. No runtime/db deps. |
| [`subdex`](./crates/subdex) | The engine: backfill + live-follow, reorg handling, concurrent handler compute. |
| [`subdex-source`](./crates/subdex-source) | RPC via `subxt`, the SQD-portal + `HybridSource` (`sqd` feature), and the `value` field-extraction helpers. |
| [`subdex-store`](./crates/subdex-store) | Postgres `Store` — cursor, atomic commit, reorg rollback, handler migrations, deferred indexes. |
| [`subdex-graphql`](./crates/subdex-graphql) | GraphQL serving (`async-graphql` + `axum`) + a built-in `indexerStatus` query. |
| [`subdex-config`](./crates/subdex-config) | Typed, layered (TOML + env) config loader. |
| [`subdex-codegen`](./crates/subdex-codegen) | `schema.graphql` → entities + migration + upserts + GraphQL API, plus a project scaffolder. |

Runnable examples: [`transfers`](./examples/transfers) (one handler) and
[`multi-pallet`](./examples/multi-pallet) (two handlers, two write patterns, one
atomic commit).

---

## Documentation

📖 **[kunal171.github.io/subdex](https://kunal171.github.io/subdex/)** — the full
docs site (searchable; built from [`docs/book/`](./docs/book) with mdBook).

| Doc | What |
|---|---|
| [**GUIDE**](./docs/GUIDE.md) | Build an indexer on your chain, step by step |
| [**Configuration & operations**](./docs/CONFIGURATION.md) | Every knob, data-source trade-offs, metrics, reorgs, testing |
| [Architecture](./docs/architecture.md) | The design and how the guarantees are enforced |
| [Code walkthrough](./docs/code-walkthrough.md) | A file-by-file tour of the crates |
| [Data flow](./docs/data-flow.md) | One block, chain → Postgres → GraphQL |
| [Design decisions](./docs/DESIGN-DECISIONS.md) | Why it's built this way; what we tried first |

---

## Project status

**Alpha.** The core pipeline — ingest → process → store → serve — is complete and
proven end to end against a live Substrate chain, real Postgres, and real HTTP.
APIs may still change before 1.0.

Two milestones have shipped:

- **v0.2 — reliability, performance & observability**: RPC retries with backoff,
  deep-reorg handling, the SQD-portal + `HybridSource` sources, store pruning,
  concurrent handler compute, handler-owned migrations, Prometheus metrics, and
  shared config.
- **v0.3 — schema-first & builder DX**: the `subdex-codegen` toolchain, reusable
  field-extraction helpers, deferred-index backfill, a project scaffolder, and the
  [starter template](./templates/starter/) + [GUIDE](./docs/GUIDE.md).

Contributions welcome — issues tagged
[`good first issue`](https://github.com/kunal171/subdex/labels/good%20first%20issue)
are a friendly place to start. See [CONTRIBUTING.md](./CONTRIBUTING.md).

---

## License

Licensed under the [Apache License, Version 2.0](./LICENSE).
