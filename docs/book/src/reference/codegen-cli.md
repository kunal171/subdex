# `subdex-codegen` CLI

Install it from the workspace (`cargo install --path crates/subdex-codegen`, or
`cargo run -p subdex-codegen -- …` in-tree).

## `check`

```bash
subdex-codegen check <schema.graphql | schema-dir>
```

Parse and validate a schema, printing the tables, columns, and indexes that would
be generated. Writes nothing — useful for catching a bad schema before it becomes
a bad table.

## `generate`

```bash
subdex-codegen generate <schema.graphql | schema-dir> [--out <dir>]
```

Generate, into `<dir>` (default `./generated`):

- `entities.rs` — one struct per `@entity`, **plus a typed `.upsert()`**;
- `migrations/0001_schema.sql` — `CREATE TABLE` + indexes;
- `graphql.rs` — an async-graphql read API (a list + count resolver per entity,
  merged with the framework's `indexerStatus`).

All files carry a `DO NOT EDIT` header. Edit the schema and re-run — never
hand-edit the output. Pass a **directory** to concatenate a modular
`schema/*.graphql`.

## `new`

```bash
subdex-codegen new <name> [--out <dir>]
```

Scaffold a runnable starter project into `<dir>` (default `./<name>`): a wired
`main.rs`, a stub handler, `schema.graphql`, `.env.example`, and a
`docker-compose.yml`. Then:

```bash
cd <name>
docker compose up -d
WS_URL=wss://rpc.polkadot.io cargo run
```

See the [starter template](../starter-template.md) for the schema-first variant
that also commits the generated code.
