# subdex — Design Decisions & History

A running log of *why* subdex is built the way it is: the decisions we made, the
approaches we tried, where they failed, and what we switched to. New decisions go
at the bottom of the relevant section. This is the "keep track of what we did
first and why we changed" record — read it before reworking a subsystem so you
don't re-walk a path we already ruled out.

For *what* the system does, see [architecture.md](./architecture.md). For the
forward roadmap, see the [v0.2 milestone](https://github.com/kunal171/subdex/milestone/1).

---

## 0. What subdex is (and the north star)

A **general-purpose, code-first Substrate indexer framework in Rust** — a Subsquid
(SQD) alternative where you write plain Rust handlers instead of a schema DSL. The
Unit chain is only a **test target**, never a coupling; "make it generic" has been
a repeated correction (see §11).

Guiding principles, in priority order:
1. **Correctness across runtime upgrades** — decode each block against *its own*
   spec-version metadata. This is the single failure mode that breaks pinned-
   metadata indexers in production, and avoiding it is subdex's reason to exist.
2. **Atomicity & resumability** — a block's data and the cursor advance commit in
   one transaction; a crash never leaves half-indexed state.
3. **Everything behind a trait** — `DataSource` / `Handler` / `Store` (+ the later
   `ProcessorObserver`) so pieces are swappable without touching the others.
4. **Small, verifiable steps** — small commits, feature branch → PR → merge, tests
   at every step, docs written *simultaneously* with code.

---

## 1. Core architecture: three traits + an engine

**Decision.** Split the pipeline into three traits in a dependency-free
`subdex-core` crate — `DataSource` (produce decoded blocks), `Handler` (user code
→ rows), `Store` (cursor + reorg bookkeeping) — driven by a `Processor` engine.
(PRs #1–#4.)

**Why.** Each seam is a real substitution point we expected to exercise: multiple
sources (RPC, later SQD portal), multiple stores, many handlers. Keeping
`subdex-core` free of async-runtime/DB deps means anything can implement the
contracts.

**Validated later.** The SQD-portal source (#24) and the observer hook (#25)
both slotted in behind existing traits with **zero engine/handler changes** —
the trait seams paid off exactly as intended.

---

## 2. Decoding: dynamic `scale_value::Value`, per-block metadata

**Decision.** Decode events/call-args **dynamically** into `scale_value::Value`
against the metadata of each block's own spec version (subxt's `ClientAtBlock`
carries the right metadata), rather than generating typed bindings. (PR #2.)

**Why.** This is the north-star property (§0.1): a backfill spanning a runtime
upgrade decodes old blocks under old metadata and new under new, transparently —
correct by construction, not by remembering to regenerate types.

**Problem faced (subxt API churn).** Getting here fought the subxt API:
`DecodeAsFields` is only implemented for `Value<()>`; the generic `Header` trait
doesn't expose `parent_hash`; the API shifted across 0.42→0.50. We pinned the
mapping to `PolkadotConfig` (concrete) so we could read
`SubstrateHeader::parent_hash` directly, and filed an upstream subxt issue
(#18 in this repo tracks it) about exposing `Header::parent_hash()`.

**Cost accepted.** Dynamic values mean handlers read fields by name/shape rather
than typed structs. We accept this for upgrade-correctness; a `value_ext` helper
in the example smooths the common reads.

---

## 3. Reorg detection: hash-chain check, and the deep-reorg rework

**v1 (PRs #3–#4).** Store `(height, hash)` per indexed block. Before committing
block N, check `block.parent_hash == stored_hash(N-1)`. On mismatch, roll back and
re-fetch.

**Problem.** The v1 rollback assumed the fork was **exactly one block back**:
`fork = parent_height - 1`. A deeper reorg self-corrected only by rewinding one
block per engine iteration (one fetch+rollback round-trip *per block of depth*),
with **no bound** on how far it could go. The code comment itself flagged the fork
point as "unknown from a single block" — a known-shallow shortcut.

**v2 — walk to the true common ancestor (#26, PR #39).** On mismatch,
`find_fork_point` walks **down** comparing our stored hash at each height against
the source's canonical hash there, until they agree — the real ancestor — then
rolls back **once**. Key optimization: the first comparison is free because the
incoming block's `parent_hash` *is* the canonical hash at the parent height, so a
1-block reorg needs no extra fetch. Added `max_reorg_depth` (default 64, `0` =
unbounded): a fork deeper than the bound is a hard error (`ReorgTooDeep`) rather
than an unbounded rewind — on a finalized-block indexer that depth signals a
misconfiguration, not a real fork.

**Problem hit during v2 (tests).** The existing reorg tests committed blocks
directly with an *empty* `ScriptedSource`; the new walk needs the source to serve
canonical hashes at descending heights, so those tests failed with "source
returned no block at height N". **Fix:** rewrote the tests to model the fork
chain *in the source* (`processor_over(fork_b, …)`), which is also more realistic.
An early attempt to claim "depth-1 reorgs never fetch" was wrong (a genuine
depth-1 fork still descends one level to confirm the ancestor); the honest
invariant is "a *matching* parent needs no fetch," which the final test asserts.

---

## 4. Throughput: per-batch commits + concurrent fetch + data selection

Three Subsquid-inspired levers, each shipped and measured:

- **Concurrent batch fetch (PR #15).** Direct RPC is latency-bound; issuing up to
  `concurrency` block fetches in flight hides the round-trip latency. Also dropped
  a redundant second events fetch (~460 → ~387 ms/block).
  - **Problem / negative result.** Against a *public* node, concurrency gave ~0
    speedup and `concurrency=32` triggered RPC timeouts — public nodes
    serialize/rate-limit. **Conclusion:** the node is the bottleneck, not the
    framework; concurrency helps a good/local node, so we kept it but stopped
    chasing throughput on public endpoints.
- **Batch processing (PR #19).** Commit **one transaction per batch**, not per
  block — the DB-side throughput lever. `Handler::process_batch` (default
  delegates to `process_block`) lets a handler accumulate across a batch and
  bulk-write once.
- **Data selection (PR #20).** `DataSelection { events, extrinsics }` so an
  indexer that only needs events doesn't fetch/decode extrinsics every block.
  - **Trade-off documented:** the block timestamp lives in the `Timestamp.set`
    *extrinsic*, so `events_only` yields `timestamp = None`. Acceptable and
    documented rather than worked around.

---

## 5. Ingestion sources

**RPC first (`SubxtSource`, PR #2).** Works against any Substrate chain; does both
backfill and live-follow. Latency-bound (~33–50 blk/s on a public node).

**RPC reliability (#23, PR #35).** Real runs died on a single transient RPC/WS
failure. Added bounded retry-with-exponential-backoff + jitter around the source's
network ops, retrying only transient (`Source`) errors and failing fast on decode
errors.

**SQD-portal source (#24, PR #37).** A second `DataSource` for fast historical
backfill. **We designed before coding** (RFC at
[docs/rfcs/024](./rfcs/024-sqd-portal-source.md)) and the design surfaced two hard
constraints the issue hadn't anticipated:

1. **The portal is historical-only** (no live Substrate tip), so a portal source
   can implement `finalized_head` + `fetch_batch` but **not** `next_finalized`
   (it returns a clear "backfill-only" error). The production shape is therefore a
   *hybrid* — portal backfill → RPC tip — which we deferred to a follow-up.
2. **Portal args are pre-decoded JSON, not SCALE.** subdex's model holds
   `scale_value::Value`. These are different decoders, so the issue's original
   "byte-identical handler output vs RPC" criterion is **impossible**. We
   *relaxed* it (with sign-off) to: structural fields match RPC exactly; a
   `json_to_value` bridge keeps the `Value` *type* so handlers compile/run; the
   decoded *contents* are equivalent-not-identical, documented on the bridge.

**Rejected alternatives for the Value gap:** adding a `RawJson` variant to the
model (invasive, breaks "one Value type"); making the portal source only usable
with JSON-reading handlers (splits the ecosystem). Both rejected in the RFC.

**Problem the live test caught.** Building against the *real* Polkadot portal
revealed the response didn't match SQD's own docs: events carry **no per-block
`index`** and calls carry **no `extrinsicIndex`** (with only the `call` selector).
We derive both from array position now. **Lesson:** the `#[ignore]`d live test
earned its keep — fixture-only tests written from the docs would have shipped
broken.

**Benchmark (Polkadot, verified).** Header-only ~512 blk/s (1k range), ~1,638
blk/s (5k range) vs ~33 blk/s RPC — 15–50× — and the portal doesn't cap the range,
so larger batches are faster. Throughput is dominated by *selected payload size*
(heavy `ParaInclusion` args slow full selections).

---

## 6. Observability: a trait, not a hardcoded metrics backend

**Decision (#25, PR #36).** Add a synchronous, backend-agnostic
`ProcessorObserver` trait (no-op default) the engine calls at batch/reorg/head/
fetch/error points, plus a Prometheus impl behind an **opt-in `metrics` feature**.

**Why a trait (rejected: wire the `metrics` crate directly).** Coupling the engine
to one metrics backend would give consumers no non-Prometheus hook. The trait
drives *any* sink — Prometheus, a progress reporter, a test spy — and we
**dogfooded** it by re-expressing the profile-indexer's progress reporter on top
of it.

**Why synchronous (not `async_trait`).** Hooks fire on the hot batch path; they
should do bounded, non-blocking work (a counter bump / channel send). Sync avoids
per-batch async overhead and keeps the no-op truly zero-cost.

---

## 7. Signer addresses: SS58 with a configurable prefix (#28, PR #41)

**Problem.** The RPC mapping recorded a signed extrinsic's signer as **raw hex**,
while the example rendered account *event fields* as SS58 — inconsistent.

**Approaches considered.**
- *Reuse subxt's `AccountId32::to_ss58check`.* **Rejected:** it hardcodes prefix
  42 and its hashing helper is private — no configurable prefix.
- *Own small SS58 encoder (chosen).* Re-implement the standard algorithm
  (`base58(prefix ++ account ++ blake2b-512("SS58PRE" ++ payload)[0..2])`),
  supporting the 1- and 2-byte prefix forms. `base58` + `blake2` were already
  transitive deps. A test asserts our prefix-42 output is byte-identical to
  subxt's, so switching is safe.

**Extra care.** `address_bytes()` returns the SCALE-encoded `MultiAddress`
(variant tag + payload), not a bare account. We decode the common
`Id`/`Address32` (32-byte) variants and **fall back to raw hex** for shapes we
can't map (Index, Address20, …) — an unusual address never panics or silently
drops the signer. `ss58_prefix` is a `SourceConfig` field (default 42), mirrored
on `SqdConfig` so both sources agree.

**Known limitation (honest).** We could not find a signed-origin sample in ~200
recent Polkadot relay blocks (they're overwhelmingly inherents), so the *portal*
path's `origin` JSON shape is unverified against live data. It therefore
SS58-encodes only a clear 32-byte hex account and otherwise keeps the string
as-is — deliberately conservative rather than over-fitted to a shape we couldn't
confirm.

---

## 8. Decode failures: visible, never silent (#29, PR #42)

**Problem.** `mapping.rs` swallowed per-event / per-extrinsic decode failures into
an empty value:

```rust
.unwrap_or_else(|_| scale_value::Value::unnamed_composite(Vec::new()))
```

An empty value from a *failure* is indistinguishable from an item that genuinely
has no fields. For an indexer whose entire reason to exist is upgrade-correct
decoding (§0.1), silently writing empty data is precisely the corruption mode
we're supposed to prevent.

**Decision.** A shared `on_decode_failure` helper for both decode sites:
- **logs** a structured `warn!` (kind / pallet / item / height / error), and
- **counts** `subdex_decode_failures_total` (labelled by kind/pallet) via the
  `metrics` facade.

**Why the counter needs no feature gate.** The `metrics` facade is a **no-op
unless a recorder is installed**, so the counter is free when the engine's
`metrics` feature is off and shows up in Prometheus when it's on. Gating it
behind a new `subdex-source` feature would have bought nothing — rejected.

**`strict` mode.** New `SourceConfig.strict` (default **off**) escalates a decode
failure to a hard `SubdexError::Decode` that aborts the block (the engine's atomic
commit means nothing half-writes). Intended for CI / correctness testing.
*Rejected alternative:* "strict = just log at `error!` but keep going" — the run
would still 'succeed', so it wouldn't actually catch drift in CI.

**The portal source needs no `strict` knob.** Its `json_to_value` is **total** —
the JSON is already parsed, so there is no per-item decode-failure site; a
malformed portal *line* already surfaces as a `Decode` error in the client. Adding
a `strict` field there would have been a dead knob.

---

## 9. Store pruning: a bounded reorg window (#32, PR #43)

**Problem.** `subdex_block` kept **one row per block, forever**, purely for reorg
detection. On a multi-million-block chain that's millions of rows that are never
read again — reorg checks only look back a bounded window, and subdex indexes
*finalized* blocks.

**Decision.** `StoreConfig.reorg_retention` (default 5000; `0` = keep all), and
`set_cursor` prunes `subdex_block WHERE height < committed - retention` **on the
same transaction** — atomic with the cursor advance, no extra round-trip, and the
latest row is never touched so the cursor stays authoritative.

**Why the store, not the processor.** Two options were on the table: put retention
on `ProcessorConfig` (and add a `prune()` method to the `Store` trait for the
engine to call once per batch), or let the store own its own table's policy. We
chose the **store**: it keeps the `Store` trait surface unchanged and puts the
policy where the table lives. The cost is that pruning runs once per `set_cursor`
(i.e. per block in a batch) rather than once per batch — accepted, because each
`DELETE ... WHERE height < X` is a cheap, bounded, usually-no-op statement.

**A dead field removed.** `ProcessorConfig.reorg_retention` had existed as a stub
since the early days ("Defaults to 0 until the processor implements pruning") and
was **never read** — only its own test asserted it. Now that retention genuinely
lives on `StoreConfig`, keeping an identically-named engine field would actively
mislead, so it was deleted. Constraint documented in both places:
**`reorg_retention` must be ≥ `max_reorg_depth`** (§3) or a reorg's fork point
could be pruned out from under the walk.

---

## 10. Process, tooling & workflow decisions

- **Small commits + feature-branch PRs.** After an early "why is all the work in
  one commit?" correction, every feature since has been small commits on a branch
  → PR → squash-merge to a branch-protected `main` (3 required checks).
- **CI (PR #9), then feature coverage (PR #38).** CI runs fmt/clippy/doc/test +
  a Postgres-service integration job + cargo-deny.
  - **Problem found:** the default CI run compiled **none** of the feature-gated
    code (`sqd`, `metrics`), so it could regress silently. Added `--all-features`
    runs (test count 57 → 75). That same PR also caught a **fresh advisory**
    (RUSTSEC-2026-0204 in crossbeam-epoch) and bumped it — deny scanning the whole
    graph is why a dependency issue surfaced unrelated to the change.
- **`cargo fmt`, not per-tool config.** (Note: the *unit-chain* repo uses prettier,
  not rustfmt — that's a different repo; subdex is standard Rust tooling.)
- **CI infra flakiness.** Once, all three jobs "failed" at a uniform 15m — a
  runner **cancellation**, not a code failure. Diagnosed (the reorg loop provably
  terminates), re-ran, passed in <1m. Don't assume red = your code; check whether
  it's a timeout/cancel.

---

## 11. Recurring corrections (things we had to undo/redo)

These are course-corrections worth not repeating:

- **"Make it generic."** Unit-specific references leaked into the README, docs,
  code, tests, examples, and CI. Cleaned in PRs #21 and #22 (and a test file
  `live_unit.rs` → `live_chain.rs` rename). Rule: Unit is a test target, the
  framework names nothing Unit-specific.
- **Env config, not hardcoded endpoints.** Hardcoded `WS_URL`/`DATABASE_URL`
  removed in favor of required env vars (PR #16); the example README table was
  later corrected to match the code (they were never actually Unit-defaulted).
- **Design before big features.** For #24 we wrote an RFC first; it caught the
  historical-only and JSON-vs-Value constraints *before* code, and let us relax an
  impossible acceptance criterion deliberately rather than discover it late.
- **Verify against reality.** Live tests (RPC and portal) repeatedly caught gaps
  that fixtures/docs missed. Keep the `#[ignore]`d live tests. The portal source
  in particular shipped correct **only** because a live test caught two places
  where SQD's own docs didn't match the real response (§5).
- **Don't leave dead config stubs.** `ProcessorConfig.reorg_retention` sat unread
  for weeks with a "until the processor implements pruning" comment; when pruning
  actually landed it went on `StoreConfig` instead, and the old field would have
  silently shadowed the real one. Deleted (§9). A knob nobody reads is worse than
  no knob.
- **Branch protection requires an up-to-date branch.** A PR whose checks are green
  can still be `BEHIND` main and refuse to merge. Update the branch (which re-runs
  CI) rather than reaching for `--admin`.

---

## 12. Status & open decisions

**Shipped** (all on the [v0.2 milestone](https://github.com/kunal171/subdex/milestone/1)):
RPC retry (#23, §5) · SQD-portal source, first PR (#24, §5) · observability (#25,
§6) · deep-reorg walk (#26, §3) · SS58 signer (#28, §7) · decode-failure
visibility (#29, §8) · store pruning (#32, §9) · CI feature coverage (§10).

**Still open:**

- **HybridSource (#24 follow-up).** Portal backfill → RPC tip — the production
  shape, now that the portal source is backfill-only *by design* (§5). The biggest
  remaining item for making the fast source usable end to end.
- **Concurrent handler compute (#27).** Handlers run sequentially within a batch
  (required for shared-tx atomicity). Likely a two-phase `prepare`(parallel) /
  `write`(serial) split — preserves atomicity, overlaps decode. **API shape is the
  open question**, so this deserves a design pass before code (cf. §5's RFC-first
  approach, which paid off).
- **Handler-owned migrations (#33).** The framework versions its own schema;
  handler tables are ad-hoc `CREATE TABLE IF NOT EXISTS`. Needs a versioned
  migration hook before anyone builds a long-lived production indexer on top.
- **Shared config (#30).** Each binary re-parses the same env vars by hand; a typed
  env+TOML layer would cut the boilerplate (and stop the example README/code from
  drifting apart, as it once did — §11).
- **Multi-handler example (#31).** The only in-repo example wires a single handler;
  the README itself flags the gap.

**How to add to this log:** when you make a non-obvious design choice — or reverse
one — add a short entry: *what we did first, why it fell short, what we switched
to, and the trade-off accepted.* Link the PR/issue.
