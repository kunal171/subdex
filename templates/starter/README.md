# subdex starter

A minimal, runnable [subdex](https://github.com/kunal171/subdex) indexer. Clone
it, point it at a chain, and it backfills + follows the tip into Postgres and
serves a GraphQL API — then edit the schema + handler to index what you need.

It's **schema-first**: `schema.graphql` is compiled by `subdex-codegen` into the
committed files under `src/generated/` (entity structs + typed upserts + a GraphQL
API) and `migrations/`. You write only the event→entity mapping in
`src/handler.rs`.

## Run it

```bash
# 1. Start Postgres
docker compose up -d          # or: just up

# 2. Run against a chain (backfill, then follow) + GraphQL on :4350
WS_URL=wss://rpc.polkadot.io cargo run     # or: just run
```

Open the API at <http://localhost:4350/graphql> and query `transfers`,
`transfersCount`, or `indexerStatus`.

## Make it yours

1. **Edit `schema.graphql`** — describe your entities (`@entity` types).
2. **Regenerate** — `subdex-codegen generate schema.graphql --out src/generated`
   (or `just codegen`). This rewrites `src/generated/` + the migration.
3. **Edit `src/handler.rs`** — match your chain's events and build the generated
   entity structs; call their `.upsert(...)`.
4. **Run** — `just run`.

## What's here

| Path | What |
|---|---|
| `schema.graphql` | Your entity schema (the source of truth). |
| `src/generated/` | Generated from the schema — **don't hand-edit**. |
| `src/handler.rs` | Your event→entity mapping (the part you write). |
| `src/main.rs` | Wiring: source → store → processor + GraphQL. |
| `migrations/` | Generated SQL; applied on startup. |
| `subdex.toml` | Config (source/store/processor); env vars override it. |
| `docker-compose.yml` | Postgres for local dev. |
| `justfile` | `up` / `codegen` / `run` / `backfill` / `psql` shortcuts. |

## Next

The full walkthrough — data sources (RPC / portal / hybrid), handler patterns,
config knobs, GraphQL, observability, fast backfill, and operations — is in
[`docs/GUIDE.md`](https://github.com/kunal171/subdex/blob/main/docs/GUIDE.md).
