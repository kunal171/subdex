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

fn usage() {
    eprintln!(
        "subdex-codegen — schema-first codegen for subdex

USAGE:
    subdex-codegen check <schema.graphql | schema-dir>
        Parse and validate a GraphQL @entity schema, printing the entities,
        columns and indexes that will be generated.

Generation subcommands are coming in subsequent releases (see RFC 034)."
    );
}

/// Parse a schema (file or directory) and print what it resolves to.
fn check(path: &Path) -> ExitCode {
    let schema = if path.is_dir() {
        match subdex_codegen::parse_schema_dir(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("error: reading `{}`: {e}", path.display());
                return ExitCode::FAILURE;
            }
        };
        match subdex_codegen::parse_schema(&text) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
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
