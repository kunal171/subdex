# Data Flow — tracing one block end to end

This document follows a single block through the entire pipeline, from a node's
RPC to a row in Postgres and out via GraphQL. It ties together the abstractions in
[Architecture](./architecture.md) and the implementation in the
[Code Walkthrough](./code-walkthrough.md).

We'll use the bundled `transfers` example (indexing `Assets.Deposited` /
`Assets.Withdrawn`) as the concrete case.

> **Note on batching:** for clarity this trace follows a single block N. In
> practice the engine fetches and commits blocks in **batches** — a whole batch's
> handler writes and cursor advance commit in **one transaction**
> (`commit_batch`), which is the same reorg-check-then-commit logic shown below,
> just applied to N blocks at once. The single-block path (`commit_block`) still
> exists as a public helper.

---

## Setup (once, at startup)

```
processor.init()
   ├─ store.init()        → runs MIGRATOR → CREATE TABLE subdex_block (if absent)
   └─ for each handler: handler.init(store)
                          → TransfersHandler creates its `transfers` table
```

After `init`, the framework's `subdex_block` table and the user's `transfers`
table both exist. Nothing has been indexed yet, so `store.cursor()` is `None`.

---

## Backfill: processing block N

Say the finalized head is `H` and we start from height `S`. The engine loops over
`[S, H]` in batches. Here's what happens for one block, height `N`.

### 1. Fetch + decode (DataSource)

```
processor.backfill()
   → source.fetch_batch(N, N+batch-1)
        → for height N:  client.at_block(N)         ← subxt positions at block N,
        │                                              carrying block N's metadata
        │                                              (its own spec_version!)
        → map_block(at, finalized = true):
             • number       = N
             • hash         = 0x… (block N's hash)
             • parent_hash  = 0x… (block N-1's hash, from the header)
             • spec_version = e.g. 147
             • extrinsics[] = decoded dynamically into scale_value::Value
             • events[]     = decoded dynamically; each linked to its extrinsic
             • timestamp    = from the Timestamp.set inherent
   → returns Block { … }
```

The decode uses **block N's own metadata**. If block N is before a runtime upgrade
and block N+1 is after, each is decoded under the right metadata with no special
handling — this is the upgrade-correctness property.

### 2. Reorg check (Processor)

```
processor.process_block(Block N):
   parent_height = N - 1
   stored = store.hash_at(N - 1)        → the hash we recorded for N-1
   if stored is Some and stored != Block.parent_hash:
        → REORG (see "Reorg path" below)
   else:
        → commit_block(Block N)         → normal path, continues below
```

For a contiguous backfill the parent hash always matches (we just indexed N-1), so
we proceed to commit.

### 3. Atomic commit (Processor + Handler + Store)

```
commit_block(Block N):
   tx = store.begin()                              ── BEGIN
   │
   ├─ TransfersHandler.process_block(Block N, tx):
   │     for ev in Block.events:
   │        if ev is Assets.Deposited / Withdrawn:
   │           asset_id = value_ext::as_u128(field(ev.fields, "asset_id"))
   │           account  = value_ext::as_account_ss58(field(ev.fields, "who"))
   │           amount   = value_ext::as_u128(field(ev.fields, "amount"))
   │           INSERT INTO transfers (…) VALUES (…)        ← on tx
   │
   ├─ store.set_cursor(tx, Block N):
   │     INSERT INTO subdex_block (N, hash, parent_hash, …) ← on tx
   │
   └─ store.commit(tx)                              ── COMMIT
```

Everything — the `transfers` rows **and** the `subdex_block` cursor row — commits
in **one** transaction. Either both land or neither does.

### 4. Advance

`backfill` sets `next = N + 1` and continues to the next block, until `next > H`.

---

## Crash safety in the middle of this

Suppose the process crashes at different points:

| Crash point | Result |
|---|---|
| During fetch/decode (step 1) | No DB writes yet. On restart, `cursor` is at `N-1`; re-fetch `N`. |
| During handler INSERTs (step 3, before commit) | The open `tx` is never committed → Postgres rolls it back. `cursor` still at `N-1`. On restart, re-process `N` cleanly. |
| After commit (step 3 done) | `cursor` is at `N`. On restart, resume from `N+1`. |

There is **never** a state where some of block N's rows are written but the cursor
doesn't reflect it, or vice versa. That's the atomicity guarantee in action.

---

## Reorg path

Now suppose block N arrives with `parent_hash` that does **not** match the hash we
stored for N-1 — the chain reorganized and replaced our parent.

```
process_block(Block N):
   stored (N-1) = 0x...AAA          (our old parent)
   Block N.parent_hash = 0x...BBB   (the new chain's parent)   → mismatch!

   → store.rollback_to(N - 2)
        DELETE FROM subdex_block WHERE height > N-2
        (the diverged parent at N-1 and everything above is removed)

   → return Some(N - 1)             (tell backfill to re-fetch from N-1)
```

`backfill` sets `next = N - 1` and re-fetches the corrected chain from there,
re-indexing N-1, N, … on the new fork. The database converges to the canonical
chain.

> Note: `rollback_to` only deletes the framework's `subdex_block` rows here. A
> production deployment that needs user entity rows rolled back too would extend
> the store / handlers to tag their rows with the block height so they can be
> removed in the same rollback — a documented extension point. Because subdex
> indexes *finalized* blocks, reorgs are rare in practice.

---

## Live following

Once backfill reaches the head, `follow()` takes over:

```
loop:
   batch = source.next_finalized()     ← blocks until the node finalizes a new block
   for block in batch:
       process_block(block)            ← same path as above (reorg check + atomic commit)
```

New finalized blocks flow through the identical `process_block` logic. Backfill and
live indexing share the same correctness machinery.

---

## Reading the data back (GraphQL)

With data in Postgres, the optional GraphQL layer serves it over HTTP:

```
serve(build_status_schema(pool), config)
   → GET  /graphql   → GraphiQL playground (interactive UI)
   → POST /graphql   → executes queries
```

A client asks for the indexer's progress:

```graphql
{ indexerStatus { height hash specVersion indexedBlocks } }
```

which resolves via `StatusQuery` → `load_status(pool)` →
`SELECT … FROM subdex_block …` → e.g.:

```json
{ "data": { "indexerStatus": {
    "height": 8668945, "hash": "0x…", "specVersion": 147, "indexedBlocks": 64
} } }
```

To expose the `transfers` data itself, you'd add your own resolver (a Rust async
function running a `SELECT` over the same pool) and compose it with `StatusQuery`
into the served schema.

---

## End-to-end summary

```
node RPC ──▶ SubxtSource ──▶ Block (decoded, per-spec) ──▶ Processor
                                                              │
                              reorg check (parent_hash vs stored)
                                                              │
                              ┌── BEGIN ──────────────────────┤
                              │   Handler INSERTs (your rows) │
                              │   set_cursor (subdex_block)   │  ← one transaction
                              └── COMMIT ─────────────────────┘
                                                              │
                                                          Postgres
                                                              │
                                                     GraphQL (optional)
```

Every block takes this exact path; the only branch is reorg vs. normal commit. That
uniformity — one path, with atomic, resumable, reorg-safe, upgrade-correct
guarantees baked into it — is the whole point of the framework.
