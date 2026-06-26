# subdex example: `transfers`

A minimal, runnable [`subdex`](../../) indexer. It records every
`Assets.Deposited` and `Assets.Withdrawn` event (common token-movement events on
Substrate chains) into a Postgres `transfers` table.

This is the canonical "how do I use subdex" starting point: it shows the whole
shape of a real indexer in ~120 lines — a `Handler`, wiring a `SubxtSource` +
`PgStore` + `Processor`, and a `main`.

## What it does

- Implements a [`Handler`] (`TransfersHandler`) that, per block, inserts one row
  per matching event into its own `transfers` table — on the processor's
  transaction, so writes commit atomically with the indexer cursor.
- Backfills from a start height to the finalized head, then follows the tip.

## Run it

```bash
# 1. A Postgres to index into
docker run -d --name pg -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=subdex \
    -p 55432:5432 postgres:16-alpine

# 2. Configure (WS_URL + DATABASE_URL are required). The .env is auto-loaded
#    from the directory you run cargo in (the repo root), so copy it there:
cp examples/transfers/.env.example .env
#   edit the .env if your endpoints differ

# 3. Run the indexer (backfills ~20 recent blocks, then follows the tip)
cargo run -p subdex-example-transfers

# backfill only (don't follow the tip), from a specific height — env vars
# always override the .env file:
FOLLOW=0 START_HEIGHT=8660000 cargo run -p subdex-example-transfers
```

> Config is read from the environment; a local `.env` (in the working directory)
> is auto-loaded. `WS_URL` and `DATABASE_URL` are **required** — there are no
> hardcoded endpoints/credentials.

## Query it — GraphQL

By default the example **serves a GraphQL API while it indexes**, at
`http://localhost:4350/graphql` (open it in a browser for the GraphiQL
playground). The API exposes the example's own `transfers` data **and** the
framework's built-in `indexerStatus` in one schema:

```graphql
{
  transfersCount
  transfers(limit: 5, direction: "deposit") {
    blockHeight
    direction
    assetId
    account   # SS58 address
    amount
  }
  indexerStatus {
    height
    specVersion
    indexedBlocks
  }
}
```

```bash
curl -s localhost:4350/graphql -H 'content-type: application/json' \
  -d '{"query":"{ transfersCount indexerStatus { height indexedBlocks } }"}'
```

Or query the table directly:

```sql
SELECT direction, count(*) FROM transfers GROUP BY direction;
SELECT block_height, direction, asset_id, account, amount
  FROM transfers ORDER BY block_height DESC LIMIT 10;
```

## Configuration

| Var            | Default                                               | Meaning                                    |
|----------------|-------------------------------------------------------|--------------------------------------------|
| `WS_URL`       | **required** (e.g. `wss://your-substrate-node:9944`)  | Chain RPC endpoint                         |
| `DATABASE_URL` | **required** (e.g. `postgres://postgres:postgres@localhost:55432/subdex`) | Postgres connection     |
| `START_HEIGHT` | `finalized_head − 20`                                 | Backfill start (only used on a fresh DB)   |
| `FOLLOW`       | `1`                                                   | Follow the tip after backfill (`0` exits)  |
| `SERVE`        | `1`                                                   | Serve the GraphQL API (`0` to disable)     |
| `GRAPHQL_PORT` | `4350`                                                | Port for the GraphQL server                |
| `RUST_LOG`     | `info`                                                | Log level                                  |

## Table

```
transfers(id, block_height, event_index, direction, asset_id, account, amount)
  UNIQUE(block_height, event_index)   -- re-indexing is idempotent
```

`amount` is `NUMERIC` (balances can exceed i64). `account` is the **SS58** address
(the `5…` form, Substrate prefix 42 — the same as block explorers / Polkadot.js).
