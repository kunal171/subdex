# RFC: SQD-portal DataSource (#24)

## Goal
A second `DataSource` that backfills history from the SQD (Subsquid) portal —
pre-decoded, columnar, batched — far faster than per-block RPC (~33 blk/s today →
target 1000s blk/s), while leaving the engine and handlers untouched.

## What the portal actually is (verified against docs.sqd.dev)
- `POST https://portal.sqd.dev/datasets/{dataset}/stream`, JSON body selects a
  `[fromBlock, toBlock]` range + which fields (block/event/call) to include.
- Response: **JSON-lines**, one line per block (optionally gzipped). Empty range →
  still returns the last block so you can advance.
- Finalized head via response headers `X-Sqd-Finalized-Head-Number/-Hash`.
- Block header carries `number, hash, parentHash, specVersion, timestamp`.
- Events: `{ index, extrinsicIndex, name, phase, args }`; calls:
  `{ extrinsicIndex, name, success, args, origin }`.
- **Event/call `args` are decoded JSON objects, not SCALE bytes.**
- **No real-time streaming for Substrate** — finalized history only.

## Two hard constraints this imposes

### A. The portal is historical-only → hybrid is required, not optional
`next_finalized()` can't stream from the portal. So a portal-only source can
implement `finalized_head` + `fetch_batch` (backfill) but **not** live follow.
A full indexer needs: **portal for backfill → `SubxtSource` for the tip.**

Design: ship a `HybridSource { portal, rpc }` that delegates
`fetch_batch`→portal and `next_finalized`→rpc, with `finalized_head` from
whichever is ahead. `SqdPortalSource` on its own is a valid backfill-only source
(errors clearly if `next_finalized` is called).

### B. Portal args are JSON, RPC args are `scale_value::Value`
Our `Event.fields` / `Extrinsic.args` are `scale_value::Value`, produced by
subxt's SCALE decode. The portal gives **JSON**. These are different shapes, so:

> The acceptance criterion "byte-identical handler output vs an RPC backfill"
> is **not achievable for the decoded `fields`/`args` payloads.** A handler that
> reads `ev.fields` as a specific `scale_value::Value` shape (like the transfers
> example's `value_ext`) sees a different structure from the portal.

Options for the `Value` gap:
1. **`serde_json::Value` → `scale_value::Value` bridge (chosen).** Convert the
   portal's JSON into a `scale_value::Value` (objects→named composite,
   arrays→unnamed composite, numbers→u128/i128, strings→string, bool→bool). This
   keeps the *type* (`Value`) identical so handlers compile and run unchanged; the
   *contents* match for scalar/simple fields and differ only in how complex
   types (enums, byte arrays) are represented. Document the difference.
2. Add a `RawJson` variant to the model — invasive, breaks the "one Value type"
   simplicity. Rejected.
3. Portal-source only usable with handlers that read JSON — splits the ecosystem.
   Rejected.

So: **structural criteria (contiguity, parent-hash chaining, event
names/indices, counts) WILL match RPC byte-for-byte; the decoded arg *values*
will be equivalent-but-not-identical**, and that's documented as a known,
inherent property of using a different decoder.

## Proposed shape (crate `subdex-source`, behind a `sqd` feature)
```
crates/subdex-source/
  src/
    sqd/
      mod.rs        // SqdPortalSource + SqdConfig
      client.rs     // POST /stream, parse JSON-lines, read head headers, gzip
      mapping.rs    // portal JSON block -> subdex Block (+ json_to_value bridge)
    hybrid.rs       // HybridSource<Portal, Rpc>
```
- `SqdConfig { portal_url, dataset, selection }` (+ retry reuse from #23).
- `reqwest` (already a workspace dep) for HTTP; `serde_json` for parsing.
- `DataSelection` maps to the portal `fields` selector (events/extrinsics/none).
- Honour spec_version/timestamp from the header (no Timestamp.set extrinsic parse
  needed — the portal gives `timestamp` directly, a nice win over RPC).

## Acceptance criteria (revised, honest)
- [x] `SqdPortalSource` passes **structural** assertions (contiguity, parent-hash
  chaining, non-empty decoded event names/indices) — same test as live RPC.
- [~] Backfill output matches RPC on **structural fields + event identity + counts**;
  the decoded `Value` contents are *equivalent* (documented divergence, not
  byte-identical — inherent to a different decoder).
- [ ] `HybridSource` documented: portal backfill → RPC tip.
- [ ] Benchmark portal vs RPC on a fixed range.

## Scope options for the first PR
- **S1 (recommended):** `SqdPortalSource` (finalized_head + fetch_batch) + JSON→Value
  mapping + `sqd` feature + unit tests on mapping with recorded JSON fixtures.
  Defer `HybridSource` and the live benchmark to a fast follow.
- **S2:** S1 + `HybridSource` in the same PR.
- **S3:** everything incl. a live benchmark harness (needs a working Substrate
  dataset on the public portal + a matching RPC endpoint for the same chain).

## Open question that gates feasibility of the acceptance test
The public portal hosts specific datasets (polkadot, kusama, moonbeam, …). The
"byte-identical vs RPC over the same range" test needs **both** a portal dataset
AND an RPC endpoint for the *same chain*. Our test chain (Unit) is almost
certainly **not** on the public SQD portal. So the live cross-check must target a
chain that is on both (e.g. Polkadot) — or stay an `#[ignore]`d manual test.
