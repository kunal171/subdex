# Starter template

[`templates/starter/`](https://github.com/kunal171/subdex/tree/main/templates/starter)
is a minimal, runnable, **schema-first** indexer. Clone it, point it at a chain,
and it backfills + follows the tip into Postgres and serves a GraphQL API — then
edit the schema and handler to index what you need.

## Run it

```bash
# from templates/starter/
docker compose up -d                        # or: just up
WS_URL=wss://rpc.polkadot.io cargo run       # or: just run
```

Open <http://localhost:4350/graphql> and query `transfers`, `transfersCount`, or
`indexerStatus`.

## Make it yours

1. **Edit `schema.graphql`** — describe your entities (`@entity` types).
2. **Regenerate** — `subdex-codegen generate schema.graphql --out src/generated`
   (or `just codegen`).
3. **Edit `src/handler.rs`** — match your chain's events, build the generated
   entity structs, and call their `.upsert(...)`.
4. **Run** — `just run`.

## What's inside

| Path | What |
|---|---|
| `schema.graphql` | Your entity schema (the source of truth). |
| `src/generated/` | Generated from the schema — **don't hand-edit**. |
| `src/handler.rs` | Your event → entity mapping (the part you write). |
| `src/main.rs` | Wiring: source → store → processor + GraphQL. |
| `migrations/` | Generated SQL; applied on startup. |
| `subdex.toml` | Config (source/store/processor); env vars override it. |
| `docker-compose.yml` | Postgres for local dev. |
| `justfile` | `up` / `codegen` / `run` / `backfill` / `psql` shortcuts. |

It dogfoods the whole builder stack: the generated structs + upserts, the
[value helpers](./guide.md#reading-event-fields-the-value-helpers), and
[deferred indexes](./guide.md#8-backfill-fast). The [full guide](./guide.md)
explains every piece.
