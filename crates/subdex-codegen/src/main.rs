//! The `subdex-codegen` CLI.
//!
//! ```bash
//! subdex-codegen check schema.graphql   # parse + validate, print a summary
//! subdex-codegen check schema/          # a directory of *.graphql
//! ```
//!
//! Generation subcommands (entities, migration, upserts, GraphQL) land in the
//! following PRs — see `docs/rfcs/034-schema-first-codegen.md`. Today the CLI
//! validates a schema and shows exactly what would be generated, which is already
//! useful for catching a bad schema before it becomes a bad table.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("check") => match args.get(2) {
            Some(path) => check(Path::new(path)),
            None => {
                eprintln!("error: `check` needs a path to a schema file or directory");
                usage();
                ExitCode::FAILURE
            }
        },
        Some("generate") => {
            // subdex-codegen generate <schema> [--out <dir>]
            let Some(schema) = args.get(2) else {
                eprintln!("error: `generate` needs a path to a schema file or directory");
                usage();
                return ExitCode::FAILURE;
            };
            let out = out_dir(&args).unwrap_or_else(|| "generated".to_string());
            generate(Path::new(schema), Path::new(&out))
        }
        Some("new") => {
            // subdex-codegen new <name> [--out <dir>]
            let Some(name) = args.get(2) else {
                eprintln!("error: `new` needs a project name");
                usage();
                return ExitCode::FAILURE;
            };
            // Default the output directory to ./<name>.
            let out = out_dir(&args).unwrap_or_else(|| name.clone());
            new_project(name, Path::new(&out))
        }
        Some("--help") | Some("-h") | Some("help") | None => {
            usage();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("error: unknown command `{other}`");
            usage();
            ExitCode::FAILURE
        }
    }
}

/// Parse `--out <dir>` from the args, if present.
fn out_dir(args: &[String]) -> Option<String> {
    let i = args.iter().position(|a| a == "--out" || a == "-o")?;
    args.get(i + 1).cloned()
}

fn usage() {
    eprintln!(
        "subdex-codegen — schema-first codegen for subdex

USAGE:
    subdex-codegen check <schema.graphql | schema-dir>
        Parse and validate a schema, printing the tables, columns and indexes
        that will be generated. Does not write anything.

    subdex-codegen generate <schema.graphql | schema-dir> [--out <dir>]
        Generate `entities.rs` (Rust structs + typed upserts), a
        `migrations/NNNN_schema.sql` migration, and `graphql.rs` (async-graphql
        read models + resolvers) into <dir> (default: ./generated). Files carry a
        DO-NOT-EDIT header — edit the schema and re-run, don't hand-edit output.

    subdex-codegen new <name> [--out <dir>]
        Scaffold a runnable starter indexer project into <dir> (default: ./<name>):
        a wired main.rs, a stub handler, schema.graphql, .env.example, and a
        docker-compose.yml for Postgres. `cd <name> && docker compose up -d` then
        `WS_URL=… cargo run`.

See docs/rfcs/034-schema-first-codegen.md for the full pipeline."
    );
}

/// Scaffold a new project named `name` into `out`.
fn new_project(name: &str, out: &Path) -> ExitCode {
    if let Err(e) = subdex_codegen::scaffold::validate_name(name) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    // Refuse to write into a non-empty directory, to avoid clobbering.
    if out.exists()
        && out
            .read_dir()
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    {
        eprintln!("error: `{}` already exists and is not empty", out.display());
        return ExitCode::FAILURE;
    }

    for f in subdex_codegen::scaffold::scaffold(name) {
        let path = out.join(&f.path);
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("error: creating `{}`: {e}", parent.display());
                return ExitCode::FAILURE;
            }
        }
        if let Err(e) = std::fs::write(&path, &f.contents) {
            eprintln!("error: writing `{}`: {e}", path.display());
            return ExitCode::FAILURE;
        }
    }

    println!(
        "✓ scaffolded `{name}` into {}\n\n  cd {}\n  docker compose up -d\n  WS_URL=wss://rpc.polkadot.io cargo run",
        out.display(),
        out.display(),
    );
    ExitCode::SUCCESS
}

/// Load + parse a schema from a file or directory, printing an error and
/// returning `None` on failure.
fn load(path: &Path) -> Option<subdex_codegen::Schema> {
    let result = if path.is_dir() {
        subdex_codegen::parse_schema_dir(path).map_err(|e| e.to_string())
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| format!("reading `{}`: {e}", path.display()))
            .and_then(|t| subdex_codegen::parse_schema(&t).map_err(|e| e.to_string()))
    };
    match result {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("error: {e}");
            None
        }
    }
}

/// Generate `entities.rs` + a migration into `out`.
fn generate(schema_path: &Path, out: &Path) -> ExitCode {
    let Some(schema) = load(schema_path) else {
        return ExitCode::FAILURE;
    };
    let g = subdex_codegen::generate(&schema);

    let migrations = out.join("migrations");
    if let Err(e) = std::fs::create_dir_all(&migrations) {
        eprintln!("error: creating `{}`: {e}", migrations.display());
        return ExitCode::FAILURE;
    }

    let entities_path = out.join("entities.rs");
    let migration_path = migrations.join(&g.migration_name);
    let graphql_path = out.join("graphql.rs");
    if let Err(e) = std::fs::write(&entities_path, &g.entities_rs) {
        eprintln!("error: writing `{}`: {e}", entities_path.display());
        return ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::write(&migration_path, &g.migration_sql) {
        eprintln!("error: writing `{}`: {e}", migration_path.display());
        return ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::write(&graphql_path, &g.graphql_rs) {
        eprintln!("error: writing `{}`: {e}", graphql_path.display());
        return ExitCode::FAILURE;
    }

    println!(
        "✓ generated {} entit{} into {}:\n    {}\n    {}\n    {}",
        schema.entities.len(),
        if schema.entities.len() == 1 {
            "y"
        } else {
            "ies"
        },
        out.display(),
        entities_path.display(),
        migration_path.display(),
        graphql_path.display(),
    );
    ExitCode::SUCCESS
}

/// Parse a schema (file or directory) and print what it resolves to.
fn check(path: &Path) -> ExitCode {
    let Some(schema) = load(path) else {
        return ExitCode::FAILURE;
    };

    println!(
        "✓ {} — {} entit{}, {} enum{}",
        path.display(),
        schema.entities.len(),
        if schema.entities.len() == 1 {
            "y"
        } else {
            "ies"
        },
        schema.enums.len(),
        if schema.enums.len() == 1 { "" } else { "s" },
    );
    for e in &schema.entities {
        println!("\n  {} → table `{}`", e.name, e.table);
        for f in &e.fields {
            let mut notes = Vec::new();
            if f.is_id {
                notes.push("PRIMARY KEY");
            }
            if f.unique {
                notes.push("UNIQUE");
            }
            if f.indexed {
                notes.push("INDEX");
            }
            if !f.nullable && !f.is_id {
                notes.push("NOT NULL");
            }
            let suffix = if notes.is_empty() {
                String::new()
            } else {
                format!("  [{}]", notes.join(", "))
            };
            println!(
                "    {:<24} {:<20} {}{}",
                f.column,
                f.pg_type(),
                f.rust_type(),
                suffix
            );
        }
    }
    if !schema.enums.is_empty() {
        println!("\n  enums (stored as TEXT):");
        for en in &schema.enums {
            println!("    {} = {}", en.name, en.values.join(" | "));
        }
    }
    ExitCode::SUCCESS
}
