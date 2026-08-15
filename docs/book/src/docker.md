# Run it in Docker

The repo ships a
[`Dockerfile`](https://github.com/kunal171/subdex/blob/main/Dockerfile) and a
[`docker-compose.yml`](https://github.com/kunal171/subdex/blob/main/docker-compose.yml)
that bring up Postgres **and** the `transfers` indexer together — no local Rust
toolchain needed. Just point it at a chain:

```bash
# WS_URL is the only thing you must set (shell env or a local .env).
WS_URL=wss://rpc.polkadot.io docker compose up --build
```

- GraphQL API: <http://localhost:4350/graphql>
- Postgres: `postgres://postgres:postgres@localhost:55432/subdex`

The image is multi-stage — a Rust build stage, then a slim Debian runtime with CA
certificates (so `rustls` can verify the chain's WSS endpoint). It runs as a
non-root user and ships the `transfers` example by default; override the
`BIN`/`PACKAGE` build args to package a different binary.

For a project of your own, the [starter template](./starter-template.md) includes
its own `docker-compose.yml` for a local Postgres.
