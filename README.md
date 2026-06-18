# subdex

A general-purpose, **code-first** indexer framework for **Substrate** chains, written in Rust.

`subdex` is to Substrate what Subsquid/SQD is — but Rust-native end to end: you
implement a `Handler` trait in plain Rust, define your own tables, and the
framework drives a resumable, reorg-safe pipeline from the chain into Postgres,
with an optional GraphQL API.

## Why

Indexers that decode against a single pinned runtime metadata silently break when
a chain upgrades (storage layouts, event shapes, and call encodings drift). `subdex`
decodes each block against the metadata for **its own** spec version and is built
in the same language as Substrate itself, so chain types can be shared rather than
re-derived — eliminating an entire class of indexer/runtime drift bugs.

## Architecture

Three composable traits (in `subdex-core`):

| Trait | Role | Default impl |
|---|---|---|
| `DataSource` | Produces decoded blocks for a range + the live tip | direct RPC via `subxt` (`subdex-source`) |
| `Handler` | **User-implemented**, code-first: blocks → your rows | — |
| `Store` | Owns the cursor + reorg rollback; hands handlers a txn | Postgres via `sqlx` (`subdex-store`) |

```
Substrate chain ──(DataSource)──▶ decoded Blocks ──(processor)──▶ Handler(s) ──(Store txn)──▶ Postgres ──▶ async-graphql
```

The processor advances a `(height, hash)` cursor, commits each block's handler
writes atomically with the cursor, validates parent hashes to detect reorgs, and
rolls back above the fork point when one occurs.

## Crates

- `subdex-core` — traits + chain-agnostic types (no runtime/db deps). ✅ implemented
- `subdex-source` — `subxt`-based direct-RPC `DataSource`. _(next)_
- `subdex-store` — Postgres `Store` via `sqlx`. _(planned)_
- `subdex-graphql` — `async-graphql` + `axum` serving. _(planned)_
- `subdex` — the processor + public prelude tying it together. _(planned)_

## Status

Early development. Built feature-by-feature on branches with tests at each step.

## License

Apache-2.0
