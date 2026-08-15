# Crates

subdex is a Cargo workspace of focused crates. Depend on the ones you need; the
`subdex` façade re-exports the engine and core types.

| Crate | Role |
|---|---|
| [`subdex-core`](https://github.com/kunal171/subdex/tree/main/crates/subdex-core) | Core traits (`DataSource`, `Handler`, `Store`, `ProcessorObserver`) and chain-agnostic types (`Block`, `Event`, …). No async runtime, no DB — anything can implement the contracts. |
| [`subdex`](https://github.com/kunal171/subdex/tree/main/crates/subdex) | The indexing engine: drives a `DataSource` through `Handler`s into a `Store`, with resumable progress and reorg handling. Optional `metrics` feature (Prometheus). |
| [`subdex-source`](https://github.com/kunal171/subdex/tree/main/crates/subdex-source) | `SubxtSource` (direct RPC), `SqdPortalSource` + `HybridSource` (`sqd` feature), and the `value` field-extraction helpers for handlers. |
| [`subdex-store`](https://github.com/kunal171/subdex/tree/main/crates/subdex-store) | `PgStore`: the Postgres-backed `Store` — cursor, reorg rollback, handler migrations, deferred indexes. |
| [`subdex-graphql`](https://github.com/kunal171/subdex/tree/main/crates/subdex-graphql) | GraphQL serving (async-graphql + axum), including the built-in `indexerStatus` query. |
| [`subdex-config`](https://github.com/kunal171/subdex/tree/main/crates/subdex-config) | Typed, layered config: `subdex.toml` overlaid by environment variables. |
| [`subdex-codegen`](https://github.com/kunal171/subdex/tree/main/crates/subdex-codegen) | The `subdex-codegen` CLI: `schema.graphql` → entity structs + migration + typed upserts + GraphQL API, plus the project scaffolder. |

See [Architecture](../architecture.md) for how they fit together.
