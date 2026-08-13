//! Scaffold a runnable indexer project — the `subdex-codegen new <name>` command.
//!
//! Produces a self-contained Cargo project a builder can `cargo run` against a
//! chain: framework git-dependencies, a wired `main.rs` (source → store →
//! processor → optional GraphQL), a stub `Handler` to fill in, a `schema.graphql`
//! starting point, `.env.example`, and a `docker-compose.yml` for Postgres.
//!
//! It generates *files as strings* ([`scaffold`]); the CLI writes them. This
//! keeps it testable without touching the filesystem, the same split the other
//! generators use.
//!
//! The stub deliberately does **not** call the codegen output — a fresh project
//! compiles and runs on its own. A builder then writes their schema and runs
//! `subdex-codegen generate` to fill in `entities.rs` / `migrations` / `graphql.rs`
//! (the README the scaffold writes explains this next step).

/// The git URL new projects depend on. Kept in one place so it's easy to swap for
/// a crates.io release later.
const REPO_GIT: &str = "https://github.com/kunal171/subdex";

/// One generated file: a path **relative to the new project root**, and its body.
pub struct ScaffoldFile {
    pub path: String,
    pub contents: String,
}

/// Render every file for a new project named `name`.
///
/// `name` is used as the crate name and the binary name; it should be a valid
/// Cargo package name (lowercase, `-`/`_`). The caller validates and reports.
pub fn scaffold(name: &str) -> Vec<ScaffoldFile> {
    vec![
        file("Cargo.toml", cargo_toml(name)),
        file("src/main.rs", main_rs(name)),
        file("src/handler.rs", HANDLER_RS.to_string()),
        file("src/lib.rs", LIB_RS.to_string()),
        file("schema.graphql", SCHEMA_GRAPHQL.to_string()),
        file(".env.example", ENV_EXAMPLE.to_string()),
        file("docker-compose.yml", docker_compose(name)),
        file(".gitignore", GITIGNORE.to_string()),
        file("README.md", readme(name)),
    ]
}

/// A valid Cargo package name: non-empty, ASCII, only `[a-z0-9_-]`, not starting
/// with a digit or `-`. Returns the offending reason on failure.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("project name must not be empty".to_string());
    }
    let first = name.chars().next().unwrap();
    if first.is_ascii_digit() || first == '-' {
        return Err(format!(
            "project name `{name}` must not start with a digit or `-`"
        ));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-'))
    {
        return Err(format!(
            "project name `{name}` has an invalid character `{bad}` (use lowercase letters, digits, `-`, `_`)"
        ));
    }
    Ok(())
}

fn file(path: &str, contents: String) -> ScaffoldFile {
    ScaffoldFile {
        path: path.to_string(),
        contents,
    }
}

fn cargo_toml(name: &str) -> String {
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "{name}"
path = "src/main.rs"

[dependencies]
# The subdex framework crates (pinned to the repo; swap for a crates.io version
# once published).
subdex = {{ git = "{REPO_GIT}" }}
subdex-source = {{ git = "{REPO_GIT}" }}
subdex-store = {{ git = "{REPO_GIT}" }}
subdex-graphql = {{ git = "{REPO_GIT}" }}
subdex-config = {{ git = "{REPO_GIT}" }}

async-graphql = "7"
async-trait = "0.1"
tokio = {{ version = "1", features = ["full"] }}
sqlx = {{ version = "0.9", features = ["runtime-tokio", "tls-rustls", "postgres"] }}
tracing = "0.1"
tracing-subscriber = {{ version = "0.3", features = ["env-filter"] }}
anyhow = "1"
"#
    )
}

const LIB_RS: &str = r#"//! Your indexer's library crate: the handler(s) and (once you run
//! `subdex-codegen generate`) the generated `entities` / `graphql` modules.

pub mod handler;
pub use handler::MyHandler;
"#;

const HANDLER_RS: &str = r#"//! Your handler: turn decoded blocks into rows in your own tables.
//!
//! Fill in `init` (create your tables) and `process_block` (write rows). The
//! writes use the transaction the processor hands you, so they commit atomically
//! with the indexer cursor — a crash never half-applies a block.
//!
//! The `subdex_source::value` helpers (`field_u128`, `field_account_ss58`, …) pull
//! typed fields out of an event's dynamic `fields` value.

use async_trait::async_trait;
use subdex::{Block, Handler, Result, Store, SubdexError};
use subdex_store::PgStore;

pub struct MyHandler;

#[async_trait]
impl Handler<PgStore> for MyHandler {
    /// Create your tables once at startup (runs outside the per-block tx).
    async fn init(&self, store: &PgStore) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS events (\
                id            BIGSERIAL PRIMARY KEY, \
                block_height  BIGINT NOT NULL, \
                event_index   BIGINT NOT NULL, \
                pallet        TEXT   NOT NULL, \
                name          TEXT   NOT NULL, \
                UNIQUE (block_height, event_index))",
        )
        .execute(store.pool())
        .await
        .map_err(|e| SubdexError::Handler(format!("create events table: {e}")))?;
        Ok(())
    }

    /// Called once per block, in order. Write your rows on `tx`.
    async fn process_block<'a>(
        &self,
        block: &Block,
        tx: &mut <PgStore as Store>::Tx<'a>,
    ) -> Result<()> {
        // Starter behavior: record one row per event so you can see data flowing.
        // Replace this with your own event matching + `subdex_source::value` reads.
        for ev in &block.events {
            sqlx::query(
                "INSERT INTO events (block_height, event_index, pallet, name) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (block_height, event_index) DO NOTHING",
            )
            .bind(block.id.number as i64)
            .bind(ev.index as i64)
            .bind(&ev.pallet)
            .bind(&ev.name)
            .execute(&mut **tx)
            .await
            .map_err(|e| SubdexError::Handler(format!("insert event: {e}")))?;
        }
        Ok(())
    }

    fn name(&self) -> &str {
        "my_handler"
    }
}
"#;

/// The generated `main.rs`, with `{{CRATE}}` standing in for the crate path (the
/// package name with `-` normalized to `_`), substituted by [`main_rs`].
const MAIN_RS_TEMPLATE: &str = r#"//! Wires a source + store + your handler into a `Processor`, backfills to the
//! finalized head, then follows the tip. Configuration comes from env / a local
//! `.env` (WS_URL, DATABASE_URL, …) via `subdex_config`.

use std::sync::Arc;
use subdex::{DataSource, Processor};
use subdex_config::IndexerConfig;
use subdex_source::SubxtSource;
use subdex_store::PgStore;

// Your crate (named after the project) exposes the handler.
use {{CRATE}}::MyHandler;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = IndexerConfig::load()?;
    let follow = env_or("FOLLOW", "1") != "0";

    tracing::info!(url = %cfg.source.url.as_deref().unwrap_or(""), "connecting to chain");
    let source = SubxtSource::connect(cfg.source_config()).await?;
    let store = PgStore::connect(cfg.store_config()).await?;

    // Start height: use the configured one, else the last ~20 finalized blocks so
    // a fresh run does something immediately.
    let head = source.finalized_head().await?;
    let mut proc_cfg = cfg.processor_config();
    if cfg.processor.start_height.is_none() {
        proc_cfg = proc_cfg.with_start_height(head.saturating_sub(20));
    }

    let processor = Processor::new(source, store, vec![Arc::new(MyHandler)], proc_cfg);
    processor.init().await?;

    if follow {
        tracing::info!("indexing — backfill then follow (Ctrl-C to stop)");
        processor.backfill().await?;
        let shutdown = async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down after the current block");
        };
        processor.follow_until(shutdown).await?;
    } else {
        let next = processor.backfill().await?;
        tracing::info!(next, "backfill complete");
    }
    Ok(())
}
"#;

/// Substitute the crate path into [`MAIN_RS_TEMPLATE`]. The crate path is the
/// package name with `-` normalized to `_` (Rust crate identifiers use `_`).
fn main_rs(name: &str) -> String {
    MAIN_RS_TEMPLATE.replace("{{CRATE}}", &name.replace('-', "_"))
}

const SCHEMA_GRAPHQL: &str = r#"# Your entity schema. Run `subdex-codegen generate schema.graphql` to generate
# entities.rs (structs + typed upserts), a SQL migration, and graphql.rs
# (an async-graphql read API) from it.
#
# Supported: @entity types with a required `id: ID!`, scalars (ID, String, Int,
# BigInt, Float, Boolean, DateTime, Bytes, JSON), scalar list fields, @index /
# @unique, enums, and a field typed as another @entity (stored as its id string).

type Event @entity {
    id: ID!
    blockHeight: Int! @index
    pallet: String!
    name: String!
}
"#;

const ENV_EXAMPLE: &str = r#"# Copy to `.env` and edit. WS_URL + DATABASE_URL are required.

# Chain RPC endpoint (WebSocket).
WS_URL=wss://rpc.polkadot.io

# Postgres to index into.
DATABASE_URL=postgres://postgres:postgres@localhost:55432/subdex

# --- Optional (defaults shown) ---
# START_HEIGHT=          # backfill start (fresh DB); defaults to head-20
# FOLLOW=1               # follow the tip after backfill (0 = exit)
# RUST_LOG=info
"#;

fn docker_compose(name: &str) -> String {
    format!(
        r#"# Postgres for `{name}` to index into. Bring it up, then `cargo run`:
#   docker compose up -d
#   WS_URL=wss://rpc.polkadot.io cargo run
services:
  db:
    image: postgres:16-alpine
    environment:
      POSTGRES_USER: postgres
      POSTGRES_PASSWORD: postgres
      POSTGRES_DB: subdex
    ports:
      - "55432:5432"
    volumes:
      - {name}-pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres -d subdex"]
      interval: 3s
      timeout: 3s
      retries: 10

volumes:
  {name}-pgdata:
"#
    )
}

const GITIGNORE: &str = r#"/target
.env
"#;

fn readme(name: &str) -> String {
    format!(
        r#"# {name}

A [subdex](https://github.com/kunal171/subdex) indexer.

## Run

```bash
# 1. Start Postgres
docker compose up -d

# 2. Point at a chain and run (backfills, then follows the tip)
WS_URL=wss://rpc.polkadot.io cargo run
```

Configuration is read from the environment or a local `.env` (see `.env.example`):
`WS_URL` and `DATABASE_URL` are required.

## What's here

- `src/handler.rs` — your `Handler`: it creates a table and records one row per
  event. Replace the body with your own event matching, using the
  `subdex_source::value` helpers (`field_u128`, `field_account_ss58`, …) to read
  typed fields.
- `src/main.rs` — wires the source + store + your handler into a `Processor`.
- `schema.graphql` — an entity schema you can grow into a generated storage + API
  layer.

## Schema-first (optional)

Instead of hand-writing tables, describe them in `schema.graphql` and generate the
Rust + SQL + GraphQL:

```bash
subdex-codegen generate schema.graphql --out src/generated
```

This writes `entities.rs` (structs + typed upserts), a SQL migration, and
`graphql.rs` (an async-graphql read API). Wire the migration into your handler's
`init` and call the generated `.upsert(...)` from `process_block`.
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_the_expected_file_set() {
        let files = scaffold("my_indexer");
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        for expected in [
            "Cargo.toml",
            "src/main.rs",
            "src/handler.rs",
            "src/lib.rs",
            "schema.graphql",
            ".env.example",
            "docker-compose.yml",
            ".gitignore",
            "README.md",
        ] {
            assert!(paths.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn cargo_toml_uses_the_project_name_and_git_deps() {
        let files = scaffold("acme_index");
        let cargo = &files
            .iter()
            .find(|f| f.path == "Cargo.toml")
            .unwrap()
            .contents;
        assert!(cargo.contains("name = \"acme_index\""));
        assert!(cargo.contains("subdex = { git ="));
        // A binary named after the project.
        assert!(cargo.contains("[[bin]]\nname = \"acme_index\""));
    }

    #[test]
    fn handler_and_main_are_coherent() {
        let files = scaffold("x");
        let handler = &files
            .iter()
            .find(|f| f.path == "src/handler.rs")
            .unwrap()
            .contents;
        let lib = &files
            .iter()
            .find(|f| f.path == "src/lib.rs")
            .unwrap()
            .contents;
        // lib re-exports MyHandler; main imports it; handler defines it.
        assert!(handler.contains("pub struct MyHandler"));
        assert!(lib.contains("pub use handler::MyHandler"));
    }

    #[test]
    fn compose_and_readme_reference_the_name() {
        let files = scaffold("cool_dex");
        let compose = &files
            .iter()
            .find(|f| f.path == "docker-compose.yml")
            .unwrap()
            .contents;
        assert!(compose.contains("cool_dex-pgdata"));
        let readme = &files
            .iter()
            .find(|f| f.path == "README.md")
            .unwrap()
            .contents;
        assert!(readme.starts_with("# cool_dex"));
    }

    #[test]
    fn name_validation() {
        assert!(validate_name("my_indexer").is_ok());
        assert!(validate_name("acme-dex").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("9lives").is_err()); // leading digit
        assert!(validate_name("-x").is_err()); // leading dash
        assert!(validate_name("Bad Name").is_err()); // space + uppercase
    }
}
