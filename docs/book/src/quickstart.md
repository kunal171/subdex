# Quickstart

The fastest way to see subdex work is the bundled
[`transfers`](https://github.com/kunal171/subdex/tree/main/examples/transfers)
example, which indexes `Assets.Deposited` / `Assets.Withdrawn` events into
Postgres and serves them over GraphQL.

**Prerequisites:** Rust ≥ 1.96, Docker (for Postgres).

```bash
# 1. A Postgres to index into
docker run -d --name subdex-db \
    -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=subdex \
    -p 55432:5432 postgres:16-alpine

# 2. Configure (WS_URL + DATABASE_URL are required; a local .env is auto-loaded)
cp examples/transfers/.env.example .env
#   edit WS_URL to point at a chain, e.g. wss://rpc.polkadot.io

# 3. Run the indexer (backfills ~20 recent blocks, then follows the tip).
cargo run -p subdex-example-transfers
```

Then query what it indexed (in `psql` or any client →
`postgres://postgres:postgres@localhost:55432/subdex`):

```sql
SELECT direction, count(*) FROM transfers GROUP BY direction;

SELECT block_height, direction, asset_id, account, amount
FROM transfers ORDER BY block_height DESC LIMIT 10;
```

Accounts are rendered as **SS58** (`5…`) addresses, just like block explorers.

## Two example shapes

- [`transfers`](https://github.com/kunal171/subdex/tree/main/examples/transfers) —
  a single hand-written handler, one event → one table.
- [`multi-pallet`](https://github.com/kunal171/subdex/tree/main/examples/multi-pallet)
  — two pallets, two handlers (one bulk-writing via the two-phase path), committing
  **atomically** into two tables and served over GraphQL.

## Next

- Prefer a clone-and-edit starting point? See the
  [starter template](./starter-template.md).
- Ready to index your own chain? Follow the
  [full guide](./guide.md).
