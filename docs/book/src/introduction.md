# subdex

**A general-purpose, code-first blockchain indexer framework for
[Substrate](https://substrate.io) chains — written in Rust.**

subdex is to Substrate what [Subsquid/SQD](https://sqd.dev) is — but Rust-native
end to end: you implement a `Handler` trait in plain Rust, define your own tables,
and the framework drives a **resumable, reorg-safe** pipeline from the chain into
Postgres, with an optional GraphQL API.

```graphql
# schema.graphql — describe your tables…
type Transfer @entity {
  id: ID!
  blockHeight: Int! @index
  from: String!
  amount: BigInt!
}
```

```rust
// …then write only the event → row mapping.
let row = Transfer {
    id: format!("{}-{}", block.id.number, ev.index),
    block_height: block.id.number as i32,
    from: field_account_ss58(&ev.fields, "from", 42).unwrap_or_default(),
    amount: field_bigint(&ev.fields, "amount").unwrap_or_else(|| "0".into()),
};
row.upsert(&mut **tx).await?;
```

## Why subdex

Indexers that decode against a **single pinned runtime metadata** silently break
when a chain upgrades — storage layouts, event shapes, and call encodings drift,
and the indexer keeps "working" while writing wrong data. subdex avoids this by:

- **Decoding each block against the metadata for _its own_ spec version** — so it
  stays correct across runtime upgrades automatically, with no per-chain codegen.
- **Being written in the same language as Substrate itself** — chain types can be
  shared rather than re-derived, removing a whole class of indexer/runtime drift.
- **Code-first ergonomics** — you write a small Rust `Handler` and define your own
  tables. Optionally, a `schema.graphql` generates the structs, migrations, and a
  GraphQL API for you.

It is **resumable** (a `(height, hash)` cursor survives restarts), **reorg-safe**
(it validates parent hashes and rolls back to the true common ancestor on a fork),
and **atomic** (your writes commit on the same transaction as the cursor advance —
never half-applied).

## Where to go from here

- **[Quickstart](./quickstart.md)** — see it index a live chain in a couple of minutes.
- **[Build an indexer on your chain](./guide.md)** — the full walkthrough, with the
  options at each step.
- **[Architecture](./architecture.md)** — how the pipeline and its guarantees work.
- **[Design decisions](./design-decisions.md)** — why it's built the way it is.

> **Status: alpha.** The core pipeline — ingest → process → store → serve — is
> complete and proven end to end against a live Substrate chain, real Postgres,
> and real HTTP. APIs may still shift before 1.0.
