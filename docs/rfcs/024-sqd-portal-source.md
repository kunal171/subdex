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

## Acceptance criteria (revised, honest) — STATUS
- [x] `SqdPortalSource` passes **structural** assertions (contiguity, parent-hash
  chaining, non-empty decoded event names) — `tests/live_sqd.rs`, verified live
  against the Polkadot dataset.
- [~] Backfill output matches RPC on **structural fields + event identity**; the
  decoded `Value` contents are *equivalent* (documented divergence on
  `json_to_value`, not byte-identical — inherent to a different decoder).
- [ ] `HybridSource` documented: portal backfill → RPC tip. **DEFERRED** (follow-up).
- [x] Benchmark portal vs RPC — see below.

## What live testing corrected (docs vs reality)
Building against the real portal caught schema deviations from the published docs:
- **Events carry no per-block `index`** — derived from array position instead.
- **Calls carry no `extrinsicIndex`** when only the `call` field is selected —
  made optional, fall back to position.
- The portal does **not cap** a requested range (asking 5000 returned all 5000 in
  one response), so large batches are fine and much faster.

## Benchmark (Polkadot, single request, near tip)
| Selection | Range | Rate |
|---|---|---|
| header-only | 1,000 | ~512 blk/s |
| header-only | 5,000 | ~1,638 blk/s |
| full events+args | 100 | ~5 blk/s* |
| **RPC baseline (public node)** | — | **~33 blk/s** |

15–50× faster than RPC, scaling with range size. *Throughput is dominated by
selected **payload size**, not block count: Polkadot relay-chain `ParaInclusion`
events carry large hex blobs, so full-event selections are heavy. `DataSelection`
matters even more here than on RPC — the default `batch_size` is 1000 and can go
higher.

## Delivered scope (S1)
`SqdPortalSource` (finalized_head + fetch_batch) + JSON→Value mapping + `sqd`
feature + fixture unit tests + a live `#[ignore]`d Polkadot test. `HybridSource`
and a hardened benchmark harness are follow-ups.
