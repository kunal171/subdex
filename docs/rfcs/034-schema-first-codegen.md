# RFC: Schema-first codegen + builder ergonomics (make subdex easy to build on)

## Motivation
Today a subdex builder hand-writes the same entity **three times**: the
`CREATE TABLE` SQL (or a migration), the `INSERT`/upsert SQL, and the
`async-graphql` `SimpleObject` + resolver. That's friction and drift.

The Subsquid-based **unit-indexer** (studied in full) shows the ergonomics we
want: a builder writes **one `schema.graphql`** and gets typed models,
migrations, and a GraphQL API generated for them — they only write the
event→entity mapping. This RFC brings that DX to subdex, in Rust, on our crates.

Goal: **a third party indexes their own chain by writing `schema.graphql` + a
small handler, and running one codegen command** — no hand-written DDL, structs,
or resolvers.

---

## What the unit-indexer does (the workflow to replicate)

The whole loop, verified against the real project:

```
schema/*.graphql ──(build-schema.sh)──▶ schema.graphql
        │
        ├─(squid-typeorm-codegen)──▶ src/model/generated/*.ts   (typed entity classes)
        ├─(squid-typeorm-migration generate + apply)──▶ db/migrations/*.sql  (tables)
        └─(squid-graphql-server)──▶ GraphQL API (auto-served from the schema)

Builder writes: schema/*.graphql  +  src/pallets/<name>.ts (event → entity)
```

- **Declarative schema.** `type UserProfile @entity { id: ID! username: String
  @unique @index bio: String hourlyRate: BigInt }`. Directives used across the
  real schemas: `@entity`, `@index`, `@unique`, `@derivedFrom` (relations),
  enums, scalars (`ID/String/Int/BigInt/Boolean/DateTime/Bytes`).
- **Modular per-pallet schema** concatenated into one file (`build-schema.sh`) so
  teams edit `schema/profile.graphql` etc. without conflicts.
- **Pallet-module pattern.** Each `src/pallets/<name>.ts` exports a consistent
  shape: `get<Pallet>Events(ctx)` (extract), `loadCurrent<Pallet>(ctx)`
  (hydrate existing), `process*` (event → entity Maps), `merge*`, `update*Stats`.
- **Per-batch pipeline** in `main.ts`: load (2 phases, FK-ordered) → extract →
  process → **persist** (`upsert` Maps, parents before children) → stats.
- **Backfill performance flags** (the genuinely clever bit) — env-gated,
  auto-disabled near chain head:
  - `DEFER_INDEXES` — drop non-PK indexes during backfill, recreate at head
    (huge INSERT speedup); restart-safe via a `.dropped-indexes.json` cache.
  - `DEFER_STATS` — skip DB-query-based aggregate stats until near head.
  - `DEFER_ASSET_REFRESH` — skip full storage re-syncs until near head.
- **Value/address helpers** (`utils/value.ts`): `safeEncodeAddress`, `toHex`,
  `safeBigInt/safeNumber`, `validateEventArgs`, etc. — the "pull a typed field
  out of a decoded event safely" toolkit. subdex has a smaller `value_ext` today.

---

## What to bring into subdex — and how

### Tier 1 — the headline: `subdex-codegen` (schema → Rust + SQL + GraphQL)

A new **binary crate** `subdex-codegen` (installable via `cargo install`, run as
`subdex-codegen`) that reads a Subsquid-compatible `schema.graphql` and emits:

1. **Entity structs** (`generated/entities.rs`) — one `#[derive(...)]` struct per
   `@entity`, fields typed from the GraphQL scalars:
   | GraphQL | Rust | Postgres |
   |---|---|---|
   | `ID!` | `String` | `TEXT PRIMARY KEY` |
   | `String` | `Option<String>` | `TEXT` |
   | `String!` | `String` | `TEXT NOT NULL` |
   | `Int` | `Option<i32>` | `INTEGER` |
   | `BigInt` | `Option<String>`* | `NUMERIC` |
   | `Boolean` | `Option<bool>` | `BOOLEAN` |
   | `DateTime` | `Option<i64>` | `BIGINT`/`TIMESTAMPTZ` |
   | `Bytes` | `Option<String>` (hex) | `TEXT` |
   \* BigInt as decimal string to avoid i64 overflow (matches the transfers example).
2. **A SQL migration** (`migrations/NNNN_<schema>.sql`) — `CREATE TABLE` per
   entity with `@index` → `CREATE INDEX`, `@unique` → `UNIQUE`, `id` → PK. Runs
   through the **#33 handler-migration** machinery
   (`store.run_handler_migrations`) — so this reuses what we already shipped.
3. **Typed insert/upsert helpers** — `impl Entity { async fn upsert(&self, tx) }`
   generating the parameterized `INSERT … ON CONFLICT (id) DO UPDATE`. No hand SQL.
4. **`async-graphql` types + a query resolver** — a `SimpleObject` per entity and
   a `Query` with a `<entities>(limit, where…)` field, wired to the same pool —
   reusing the existing **`subdex-graphql`** toolkit. Auto-served like Subsquid's
   `squid-graphql-server`.

The builder writes: `schema.graphql` + a `Handler` whose `process_batch` maps
decoded events to the generated entities and calls `.upsert(tx)`. **No DDL, no
row structs, no resolvers by hand.**

### Tier 2 — richer value/decode helpers (`subdex-source::value` or a new util crate)
Port the safe-extraction toolkit from `utils/value.ts` into reusable Rust:
`ss58` (have it), plus `field_str/field_bigint/field_u128/field_bool`, hex
helpers, `require_fields(...)`. Lifts the per-handler decode boilerplate.

### Tier 3 — deferred backfill optimizations (engine, opt-in)
Bring `DEFER_INDEXES` to the engine/store: on a fresh backfill, drop a handler
table's non-PK indexes, recreate at head (detected via block-timestamp age or
`head - cursor` from the #25 observer). This is the biggest INSERT-throughput
lever the unit-indexer has, and it maps cleanly onto our `Store` + observer.
`DEFER_STATS`/`DEFER_ASSET_REFRESH` are app-level patterns — document them, don't
build engine machinery.

### Tier 4 — a documented pallet-module pattern + a scaffolder
Codify the load→extract→process→persist shape as the recommended structure, and
a `subdex-codegen new <name>` that scaffolds a project skeleton (schema dir,
handler stub, config, docker-compose) — the equivalent of `sqd init`.

### Tier 5 — a template repo + a detailed "build your indexer" guide
The onboarding deliverable. Two parts:

- **A starter template** (`templates/starter/` in-repo, and ideally a standalone
  `subdex-template` repo devs `git clone` / `cargo generate`): a *minimal but
  complete* runnable indexer — `Cargo.toml` depending on the published crates,
  one example `schema.graphql`, a handler stub mapping one event to one entity,
  a `subdex.toml`, `.env.example`, `docker-compose.yml` (Postgres), and a
  `justfile`/`Makefile` wrapping the commands (`codegen`, `migrate`, `run`,
  `serve`) — the subdex equivalent of the unit-indexer's `package.json` scripts +
  `commands.json`. A dev clones it, edits the schema + handler, runs it. Works
  against **any** Substrate chain by changing `WS_URL`.
- **A guide** (`docs/GUIDE.md`, "Build an indexer on your chain"): the full
  walkthrough, written for a third-party dev, covering **options at each step**:
  1. **Choose a data source** — RPC (`SubxtSource`, any chain), SQD portal
     (`SqdPortalSource`, fast backfill), or `HybridSource` (portal→RPC). When to
     pick which; the trade-offs (§ Data sources in the README).
  2. **Define your schema** — the `@entity` dialect, scalars, `@index`/`@unique`,
     enums; modular `schema/*.graphql`; run `subdex-codegen`.
  3. **Write a handler** — the load→extract→process→persist pattern; reading event
     fields with the value helpers (Tier 2); `process_block` (simple) vs
     `process_batch` bulk-write vs the two-phase `prepare`/`write` (#27) — with
     guidance on which to use.
  4. **Migrations** — generated by codegen, applied via `run_handler_migrations`
     (#33); evolving a schema.
  5. **Config** — env + `subdex.toml` via `subdex-config` (#30); all the knobs
     (batch size, concurrency, selection, retry, ss58 prefix, strict, retention,
     reorg depth).
  6. **Serve GraphQL** — the generated resolver + `subdex-graphql`.
  7. **Observe** — the `metrics` feature / Prometheus (#25); the progress reporter.
  8. **Backfill fast** — data selection, batch size, `HybridSource`, deferred
     indexes (Tier 3); the "public RPC is the bottleneck" reality.
  9. **Operate** — resumability, reorg safety, strict mode, running under Docker.

  The guide should read like a tutorial a dev follows top-to-bottom, then a
  reference they return to — mirroring the depth of the unit-indexer README but
  for the subdex/Rust workflow.

---

## Explicitly NOT copying
- **`squid-substrate-typegen`** (generating per-event Rust types from metadata).
  subdex is deliberately **dynamic** (`scale_value::Value`, §2 of DESIGN-DECISIONS)
  — that's our upgrade-correctness property. We keep dynamic decoding; codegen is
  only for the *storage/serve* side, not the *decode* side. This is the key
  divergence from Subsquid and it's a feature.
- The giant hand-maintained `build-schema.sh` concatenation — the Rust codegen can
  read a `schema/` directory directly (glob + parse), no shell concat step.

---

## Design decisions to lock (open questions)

1. **GraphQL parser.** We need to parse the `@entity` dialect. Options:
   `async-graphql-parser` (we already depend on async-graphql — reuse its parser,
   no new dep) vs `graphql-parser` crate. **Lean: `async-graphql-parser`.**
2. **Generated-code location & regeneration.** Commit generated files (like
   Subsquid) so builds don't require the tool, and regenerate on schema change —
   with a "DO NOT EDIT" header. Or a build-script (`build.rs`) that regenerates at
   compile time (no committed artifacts, but heavier builds). **Lean: committed +
   a `subdex-codegen` command**, matching Subsquid's mental model.
3. **Relations (`@derivedFrom`, entity references).** Full FK/relation support is
   a lot. **Lean: v1 supports scalars + `@index`/`@unique` + enums; relations are
   stored as the referenced `id` string (no join magic) with a documented pattern,
   and full `@derivedFrom` is a follow-up.**
4. **How much GraphQL to generate.** A `SimpleObject` + a list query with
   `limit`/order is easy; full Subsquid-style `where`-filtering is a lot. **Lean:
   list + a few common filters (by id, by an indexed field) in v1.**

---

## Suggested delivery (small PRs, per our workflow)
1. **[Tier 1]** `subdex-codegen` crate scaffold + GraphQL schema parser →
   in-memory entity model (parse + validate, unit-tested with fixture schemas).
   No output yet.
2. **[Tier 1]** Entity-struct + migration SQL generation (+ golden-file tests).
3. **[Tier 1]** Upsert-helper generation.
4. **[Tier 1]** GraphQL `SimpleObject` + resolver generation.
5. **[Tier 2]** Value/decode helper crate (port `utils/value.ts` → Rust).
6. **[Tier 5]** Starter template (`templates/starter/`) + rework an example to be
   **schema-first** (delete its hand-written table/struct/resolver, add a
   `schema.graphql`, run codegen) — the dogfood + reference.
7. **[Tier 5]** The `docs/GUIDE.md` "build an indexer on your chain" walkthrough.
8. **[Tier 4]** `subdex-codegen new <name>` scaffolder.
9. **[Tier 3]** Deferred-index backfill support in the engine/store.

## Acceptance (v1)
- [ ] **T1** A builder writes `schema.graphql`, runs `subdex-codegen`, and gets
  entities + a migration + upsert helpers + a GraphQL type/resolver — no hand
  DDL/SQL.
- [ ] **T2** Value helpers available; the schema-first example uses them, not
  ad-hoc decode.
- [ ] **T3** `DEFER_INDEXES`-style backfill speedup available and documented.
- [ ] **T4** `subdex-codegen new` scaffolds a runnable project.
- [ ] **T5** A starter template exists; an example dogfoods schema-first end to
  end (schema → running indexer + GraphQL); `docs/GUIDE.md` walks a third-party
  dev through building on their own chain, with options at each step.
- [ ] The dynamic decode path is untouched (upgrade-correctness preserved).
