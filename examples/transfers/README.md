# subdex example: `transfers`

A minimal, runnable [`subdex`](../../) indexer. It records every
`Assets.Deposited` and `Assets.Withdrawn` event (the most common token-movement
events on Unit) into a Postgres `transfers` table.

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

# 2. Run the indexer (defaults to Unit mainnet, last ~20 blocks, then follows)
DATABASE_URL=postgres://postgres:postgres@localhost:55432/subdex \
WS_URL=wss://archive2.mainnet-unit.com \
    cargo run -p subdex-example-transfers

# backfill only (don't follow the tip), from a specific height:
FOLLOW=0 START_HEIGHT=8660000 cargo run -p subdex-example-transfers
```

Then query what it indexed:

```sql
SELECT direction, count(*) FROM transfers GROUP BY direction;
SELECT block_height, direction, asset_id, account, amount
  FROM transfers ORDER BY block_height DESC LIMIT 10;
```

## Configuration

| Var            | Default                                               | Meaning                                    |
|----------------|-------------------------------------------------------|--------------------------------------------|
| `WS_URL`       | `wss://archive2.mainnet-unit.com`                     | Chain RPC endpoint                         |
| `DATABASE_URL` | `postgres://postgres:postgres@localhost:55432/subdex` | Postgres connection                        |
| `START_HEIGHT` | `finalized_head − 20`                                 | Backfill start (only used on a fresh DB)   |
| `FOLLOW`       | `1`                                                   | Follow the tip after backfill (`0` exits)  |
| `RUST_LOG`     | `info`                                                | Log level                                  |

## Table

```
transfers(id, block_height, event_index, direction, asset_id, account, amount)
  UNIQUE(block_height, event_index)   -- re-indexing is idempotent
```

`amount` is `NUMERIC` (balances can exceed i64). `account` is the `0x…` hex of
the 32-byte AccountId.
