# Configuration reference

Config layers: an optional `subdex.toml` (`[source]` / `[store]` / `[processor]`
tables), overlaid by environment variables, with a local `.env` auto-loaded.
`WS_URL` and `DATABASE_URL` are the only required values. Load it with
`IndexerConfig::load()`.

| Env var | TOML key | Meaning |
|---|---|---|
| `WS_URL` | `source.url` | **Required.** Chain WebSocket RPC endpoint. |
| `DATABASE_URL` | `store.url` | **Required.** Postgres connection string. |
| `BATCH_SIZE` | `source.batch_size` + `processor.batch_size` | Blocks per fetch / per commit. |
| `CONCURRENCY` | `source.concurrency` | In-flight block fetches (hides RPC latency). |
| `SS58_PREFIX` | `source.ss58_prefix` | Address format (42 = Substrate `5…`, 0 = Polkadot `1…`). |
| `STRICT` | `source.strict` | `1` = a per-item decode failure aborts the block (vs tolerate). |
| `REORG_RETENTION` | `store.reorg_retention` | `(height, hash)` rows kept for reorg checks. |
| — | `store.max_connections` | Postgres pool size. |
| `START_HEIGHT` | `processor.start_height` | Backfill start on a fresh DB (default: head − 20). |
| `MAX_REORG_DEPTH` | `processor.max_reorg_depth` | Bound on how far a reorg may rewind. |

Example `subdex.toml`:

```toml
[source]
# url = "wss://rpc.polkadot.io"   # or WS_URL
batch_size = 100
concurrency = 16
ss58_prefix = 42
strict = false

[store]
# url = "postgres://postgres:postgres@localhost:55432/subdex"  # or DATABASE_URL
max_connections = 5
reorg_retention = 5000

[processor]
# start_height = 1000000
batch_size = 100
max_reorg_depth = 64
```

The example binaries add a few app-level knobs read from the environment:
`SERVE` (serve GraphQL), `FOLLOW` (follow the tip after backfill), `GRAPHQL_PORT`,
and `RUST_LOG`.
