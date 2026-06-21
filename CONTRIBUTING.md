# Contributing to subdex

Thanks for your interest in contributing! This document describes how the project
is developed so your changes land smoothly.

## Prerequisites

- **Rust ≥ 1.96** (sqlx 0.9 requires ≥ 1.94; CI pins 1.96.0). Install via
  [rustup](https://rustup.rs).
- **Docker** — for the database-backed integration tests.
- Components: `rustfmt` and `clippy` (`rustup component add rustfmt clippy`).

## Development workflow

The project uses **short-lived feature branches → pull request → squash-merge into
`main`**. `main` is protected: a PR can only merge when CI is green.

1. **Branch** off `main` with a descriptive name:
   - `feat/...` for features, `fix/...` for fixes, `docs/...`, `ci/...`, `chore/...`.
2. **Make small, focused commits.** Each commit should compile and pass its tests
   on its own — the history should read as a sequence of meaningful steps, not one
   giant blob. Prefer several small commits over one large one.
3. **Write tests for every change** (see [Testing](#testing)). New behaviour
   without a test won't be accepted.
4. **Open a PR** against `main`. CI runs automatically; it must pass to merge.
5. PRs are **squash-merged**, keeping `main`'s history linear.

### Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/)-style prefixes,
scoped by crate where it helps:

```
feat(processor): run_until + follow_until with graceful shutdown
fix(store): make set_cursor idempotent on re-index
docs(readme): add quickstart
test(graphql): gated HTTP integration test
ci: add cargo-deny dependency audit
```

The body should explain **what** changed and **why**, and note how it was verified.

## Before you push

Run the same checks CI runs — they must all pass:

```bash
cargo fmt --all --check                              # formatting
cargo clippy --workspace --all-targets -- -D warnings # lints (warnings are errors)
cargo test --workspace                               # offline test suite
```

If `fmt` reports diffs, run `cargo fmt` to fix them.

## Testing

The suite has two tiers:

- **Offline tests** — fast, hermetic, no network or database. These run on every
  push/PR and **must stay green**. Use in-memory test doubles (see
  [`crates/subdex/src/testkit.rs`](./crates/subdex/src/testkit.rs)) for engine
  logic.
- **Integration tests** — marked `#[ignore]` so the offline run skips them. They
  need external resources and are run explicitly:

  ```bash
  # Postgres (store + graphql integration tests)
  docker run -d --name pg -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=subdex \
      -p 55432:5432 postgres:16-alpine

  SUBDEX_TEST_DB=postgres://postgres:postgres@localhost:55432/subdex \
      cargo test --workspace -- --ignored

  # Live-chain tests additionally need an RPC endpoint:
  SUBDEX_TEST_WS=wss://archive2.mainnet-unit.com \
      cargo test -p subdex-source --test live_unit -- --ignored
  ```

CI runs the **DB** integration tests (via a Postgres service) but **not** the
live-chain ones (they hit a public RPC). If your change touches chain decoding,
please run the live tests locally and mention the result in your PR.

### Testing conventions

- Gate any test that needs the network or a database with `#[ignore = "reason"]`,
  and read its config from an env var (`SUBDEX_TEST_DB`, `SUBDEX_TEST_WS`) with a
  sensible local default.
- Integration tests that touch a database should use an **isolated, throwaway
  database** created and dropped around the test, so they can run concurrently.

## Project layout

| Crate | Purpose |
|---|---|
| `subdex-core` | Traits (`DataSource`/`Handler`/`Store`) + types. No runtime/db deps. |
| `subdex-source` | `subxt` RPC `DataSource`. |
| `subdex-store` | Postgres `Store` via `sqlx`. |
| `subdex` | The engine (`Processor`): backfill, follow, reorg, `run_until`. |
| `subdex-graphql` | GraphQL serving toolkit. |
| `examples/transfers` | A complete runnable example. |

See [`docs/`](./docs) for the architecture, a file-by-file code walkthrough, and a
data-flow trace.

## Code style

- Keep the heavy **doc comments** the codebase uses — explain the *why*, not just
  the *what*. Every public trait, type, and non-obvious function should have a
  `///` doc.
- No `cargo clippy` warnings (CI denies them).
- Dependencies should be at their latest compatible versions; the dependency audit
  (`cargo deny`) runs in CI.

## Reporting issues

Open a GitHub issue with: what you expected, what happened, and a minimal
reproduction (chain endpoint / block range / query if relevant).

## License

By contributing, you agree your contributions are licensed under the project's
[Apache-2.0](./LICENSE) license.
