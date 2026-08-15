# RFCs

Design proposals, recorded as they were written. Each RFC captures a decision, the
alternatives weighed, and (where relevant) how the shipped implementation differs
from the original proposal.

- [RFC 024 — SQD portal source](./rfcs/024-sqd-portal-source.md): a fast
  historical-backfill `DataSource` over the SQD (Subsquid) portal.
- [RFC 027 — Concurrent handler compute](./rfcs/027-concurrent-handler-compute.md):
  a two-phase `prepare`/`write` path so heavy per-block compute runs concurrently.
- [RFC 034 — Schema-first codegen](./rfcs/034-schema-first-codegen.md): turning a
  `schema.graphql` into structs + migrations + upserts + a GraphQL API, and the
  broader builder-experience work (value helpers, deferred indexes, scaffolder,
  template + guide).

For the running log of smaller decisions, see [Design decisions](./design-decisions.md).
