# subdex — Documentation

In-depth documentation for the `subdex` Substrate indexer framework. If you just
want to *use* it, start with the [top-level README](../README.md); these docs are
for understanding **how it works** and **why it exists**.

## Contents

1. **[Purpose & Motivation](#purpose--motivation)** — why this project was built (below).
2. **[Architecture](./architecture.md)** — the end-to-end design: the three traits, the
   pipeline, and the guarantees (atomicity, resumability, reorg-safety,
   upgrade-correctness).
3. **[Code Walkthrough](./code-walkthrough.md)** — a detailed, file-by-file
   explanation of every crate, with references to the real code.
4. **[Data Flow](./data-flow.md)** — a step-by-step trace of a single block from
   the chain all the way into Postgres and out via GraphQL.

---

## Purpose & Motivation

### The problem

A **blockchain indexer** reads every block of a chain, decodes what happened
(events, transactions, state changes), and writes it into a queryable database.
Applications — explorers, dashboards, wallets, analytics, the front-end of almost
any dApp — don't talk to the chain directly for reads; they query an indexer's
database, because chains are optimized for consensus, not for "give me all
transfers for this account, paginated, sorted by time."

For **Substrate / Polkadot** chains specifically, indexing has two hard,
under-appreciated problems:

1. **Runtime-upgrade drift.** Substrate chains upgrade their runtime over time.
   An upgrade can change a storage layout, add a field to an event, re-key a map,
   or alter a call's encoding. An indexer that decodes every block against a
   *single, pinned* version of the chain's metadata will, after an upgrade,
   silently decode blocks **wrong** — it keeps running, keeps writing rows, but
   the data is corrupt. This is not hypothetical: it is the single most common
   way real Substrate indexers break, and it is invisible until someone notices
   the numbers are off.

2. **Tooling-language mismatch.** Substrate chains are written in Rust. The
   dominant indexing frameworks (Subsquid/SQD, SubQuery) are TypeScript. That
   means the chain's types are *re-derived* into TypeScript via a codegen step
   (`typegen`), which must be kept in lockstep with the runtime by hand. When the
   runtime changes and the codegen isn't regenerated, the indexer's types drift
   from reality — again, silent corruption.

Both problems share a root cause: **the indexer's understanding of the chain's
data is decoupled from the chain itself, and that decoupling has to be maintained
manually.**

### Why this project was built

`subdex` was built to make those two problems **structurally impossible**, by
inverting both assumptions:

- **Decode every block against _its own_ runtime metadata**, not a pinned one.
  The framework fetches the metadata for the spec version each block was authored
  under and decodes against that. A runtime upgrade in the middle of a backfill
  is handled transparently — blocks before the upgrade decode under the old
  metadata, blocks after under the new, with zero code changes. (See
  [`subdex-source`](./code-walkthrough.md#subdex-source) and the per-block
  `spec_version` carried in the data model.)

- **Be written in Rust, the same language as Substrate.** This removes the
  codegen/translation layer entirely: the framework decodes dynamically (so it
  needs no per-chain types at all for the generic case), and where a project
  *does* want typed access, it can share the chain's actual Rust types rather
  than re-deriving them. No `typegen`, no drift.

This is the same realization the Subsquid team themselves reached — they rewrote
their performance-critical data layer in Rust. `subdex` takes that to its
conclusion: a **Rust-native, code-first indexer framework** where the indexer and
the chain speak the same language and the indexer is correct across upgrades by
construction.

### The design goals that fell out of this

Beyond the two core problems, a usable indexer framework has to be:

| Goal | How subdex achieves it |
|---|---|
| **Resumable** | A `(height, hash)` cursor in Postgres; on restart the engine resumes from `cursor + 1`. |
| **Reorg-safe** | Each block's `parent_hash` is validated against the stored hash of the previous height; on a mismatch the diverged tail is rolled back and re-indexed. |
| **Atomic** | A handler's writes and the cursor advance commit on the *same* database transaction — a block is either fully indexed or not at all; a crash never leaves a half-written block. |
| **Ergonomic (code-first)** | You implement one `Handler` trait in plain Rust and own your tables. No schema DSL, no codegen, full type safety and the entire Rust ecosystem. |
| **Composable** | Ingestion, storage, and serving are traits — each is swappable (e.g. a future SQD-portal source) without touching your handlers. |

### What subdex is *not*

- It is **not** schema-first. There is no `schema.graphql` that generates your DB
  model and API. You write Rust. (This is a deliberate trade: more control and
  type-safety, at the cost of writing your own table definitions and resolvers.)
- It is **not** (yet) a decentralized data network like SQD's. It indexes by
  talking to a node over RPC. A columnar/portal data source can be added behind
  the `DataSource` trait later for faster historical sync.

### Who it's for

Teams running or building on a Substrate chain who want their indexer
to be **correct across runtime upgrades**, **in the same language as their chain**
(so types and logic can be shared), and **fully under their control** (self-hosted,
no external indexing service or codegen pipeline to keep in sync).

---

Next: **[Architecture →](./architecture.md)**
