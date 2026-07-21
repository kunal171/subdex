//! # subdex-codegen
//!
//! Schema-first codegen for [subdex](https://github.com/kunal171/subdex): turn a
//! GraphQL `@entity` schema into the Rust + SQL + GraphQL boilerplate an indexer
//! would otherwise hand-write three times over (table DDL, row structs and
//! INSERTs, and API types).
//!
//! ```graphql
//! type Transfer @entity {
//!   id: ID!
//!   from: String! @index
//!   amount: BigInt!
//! }
//! ```
//!
//! A builder writes that, runs `subdex-codegen`, and gets entity structs, a
//! migration, typed upsert helpers, and GraphQL types — then only writes the
//! event→entity mapping in their `Handler`.
//!
//! **What this does *not* touch:** decoding. subdex decodes chain data
//! dynamically against each block's own metadata (`scale_value::Value`), which is
//! what keeps it correct across runtime upgrades. Codegen is for the
//! *storage and serving* side only — never the decode side.
//!
//! ## Status
//!
//! This crate is built in stages (see `docs/rfcs/034-schema-first-codegen.md`):
//! 1. **schema parsing → entity model** ← *you are here*
//! 2. entity structs + SQL migration generation
//! 3. typed upsert helpers
//! 4. `async-graphql` types + resolvers
//!
//! ## Supported schema dialect (v1)
//!
//! - `type X @entity { … }` with a required `id: ID!` primary key.
//! - Scalars: `ID`, `String`, `Int`, `BigInt`, `Float`, `Boolean`, `DateTime`,
//!   `Bytes`, `JSON`.
//! - Lists of scalars (`[String!]!`) → native Postgres arrays (`TEXT[]`).
//! - Field directives: `@index`, `@unique`.
//! - `enum` types (stored as `TEXT`).
//! - A field typed as another `@entity` is a **relation stored as that entity's
//!   `id` string** (no joins in v1).
//! - `@derivedFrom` fields are skipped — they're virtual reverse relations and
//!   generate no column.
//!
//! Anything else is a clear error rather than a silently-wrong table. The parser
//! is validated against a real 103-entity production schema.

pub mod model;
pub mod parse;

pub use model::{Entity, EnumDef, Field, FieldType, Scalar, Schema};
pub use parse::{parse_schema, ParseError};

/// Parse every `*.graphql` file in a directory (sorted by filename) as one
/// schema — the modular `schema/` layout, without a concatenation step.
///
/// Returns the merged [`Schema`]. A parse error in any file is returned as-is.
pub fn parse_schema_dir(dir: &std::path::Path) -> Result<Schema, SchemaDirError> {
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| SchemaDirError::Read {
            path: dir.display().to_string(),
            reason: e.to_string(),
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "graphql"))
        .collect();
    paths.sort();

    if paths.is_empty() {
        return Err(SchemaDirError::Empty {
            path: dir.display().to_string(),
        });
    }

    // Concatenate then parse once, so cross-file references (an entity in one
    // file referencing an enum in another) resolve.
    let mut combined = String::new();
    for p in &paths {
        let text = std::fs::read_to_string(p).map_err(|e| SchemaDirError::Read {
            path: p.display().to_string(),
            reason: e.to_string(),
        })?;
        combined.push_str(&text);
        combined.push('\n');
    }

    parse_schema(&combined).map_err(SchemaDirError::Parse)
}

/// Errors from reading a schema directory.
#[derive(Debug, thiserror::Error)]
pub enum SchemaDirError {
    #[error("reading `{path}`: {reason}")]
    Read { path: String, reason: String },
    #[error("no *.graphql files found in `{path}`")]
    Empty { path: String },
    #[error(transparent)]
    Parse(#[from] ParseError),
}
