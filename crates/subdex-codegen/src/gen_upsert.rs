//! Generate a typed `upsert` helper per entity.
//!
//! Each entity struct (from [`gen_rust`](crate::gen_rust)) gets an `impl` block
//! with one method:
//!
//! ```ignore
//! transfer.upsert(&mut **tx).await?;   // in a Handler::process_block
//! transfer.upsert(store.pool()).await?; // or against the pool directly
//! ```
//!
//! which runs an `INSERT … ON CONFLICT (id) DO UPDATE SET …` against any sqlx
//! Postgres executor — a transaction (`&mut **tx`, so the write commits with the
//! indexer cursor, exactly as the hand-written examples do) *or* a pool. The
//! builder never writes the column list, the `$n` placeholders, or the bind
//! order by hand — the three places those must agree are generated together, so
//! they can't drift.
//!
//! This is the whole point of the codegen: replace the `INSERT INTO transfers
//! (…) VALUES ($1, …) ON CONFLICT …` + one `.bind(…)` per column that every
//! handler otherwise repeats (see `examples/transfers/src/handler.rs`) with a
//! single typed call.
//!
//! ## What the generated code depends on
//!
//! Just `sqlx` (with its `postgres` feature) in the builder's crate — the same
//! way `entities.rs` needs `serde_json` only when a field is JSON. The helper
//! returns `sqlx::Error`; a handler maps it to its own error with `?`/`map_err`
//! as usual.
//!
//! ## BigInt binding
//!
//! `BigInt` columns are `NUMERIC` but carried as a decimal `String` (balances
//! exceed `i64`). They bind as text and cast in SQL — `$n::text::numeric` — which
//! is exactly what the existing examples do, and casts `NULL` cleanly for the
//! nullable case.

use crate::model::{Entity, FieldType, Scalar, Schema};

/// Render the `impl … { async fn upsert }` blocks for every entity.
pub fn render(schema: &Schema) -> String {
    let mut out = String::new();
    for e in &schema.entities {
        out.push_str(&render_upsert(e));
        out.push('\n');
    }
    out
}

/// True if any entity is a `BigInt` (or has one), so the generated file needs
/// the `::text::numeric` cast path. Purely informational for callers/tests.
pub fn uses_numeric_cast(schema: &Schema) -> bool {
    schema
        .entities
        .iter()
        .flat_map(|e| &e.fields)
        .any(|f| matches!(f.ty, FieldType::Scalar(Scalar::BigInt)))
}

fn render_upsert(e: &Entity) -> String {
    // The column list, placeholder list, and DO UPDATE assignments are all built
    // from the same field iteration so they stay in lock-step.
    let mut columns = Vec::new();
    let mut placeholders = Vec::new();
    let mut updates = Vec::new();
    let mut binds = Vec::new();

    for (i, f) in e.fields.iter().enumerate() {
        let n = i + 1;
        let col = quote_ident(&f.column);
        columns.push(col.clone());
        placeholders.push(placeholder(n, &f.ty));
        // The PK is the conflict target; never overwrite it with itself.
        if !f.is_id {
            updates.push(format!("{col} = EXCLUDED.{col}"));
        }
        binds.push(format!("        .bind(&self.{})", f.column));
    }

    let id_col = e
        .id_field()
        .map(|f| quote_ident(&f.column))
        .unwrap_or_else(|| "\"id\"".to_string());

    // When an entity has *only* an id, there's nothing to update on conflict, so
    // `DO NOTHING` (an empty `DO UPDATE SET` is a SQL error).
    let conflict = if updates.is_empty() {
        format!("ON CONFLICT ({id_col}) DO NOTHING")
    } else {
        format!(
            "ON CONFLICT ({id_col}) DO UPDATE SET {}",
            updates.join(", ")
        )
    };

    let sql = format!(
        "INSERT INTO {table} ({cols}) VALUES ({vals}) {conflict}",
        table = e.table,
        cols = columns.join(", "),
        vals = placeholders.join(", "),
    );

    let mut s = String::new();
    s.push_str(&format!("impl {} {{\n", e.name));
    s.push_str(
        "    /// Insert this row, or update it if a row with the same `id` already\n\
         \x20   /// exists (upsert on the primary key). Pass a transaction\n\
         \x20   /// (`&mut **tx`) to commit with the indexer cursor, or the pool.\n",
    );
    s.push_str("    pub async fn upsert<'e, E>(&self, executor: E) -> Result<(), sqlx::Error>\n");
    s.push_str("    where\n");
    s.push_str("        E: sqlx::PgExecutor<'e>,\n");
    s.push_str("    {\n");
    // The SQL contains double-quoted identifiers (`"from"`, `"id"`, …), so it must
    // be a *raw* Rust string literal — a plain `"…"` would be terminated by the
    // first embedded `"`. `r#"…"#` is safe because the SQL never contains `"#`.
    s.push_str(&format!(
        "        sqlx::query(\n            r#\"{sql}\"#,\n        )\n"
    ));
    for b in &binds {
        s.push_str(b);
        s.push('\n');
    }
    s.push_str("        .execute(executor)\n");
    s.push_str("        .await?;\n");
    s.push_str("        Ok(())\n");
    s.push_str("    }\n");
    s.push_str("}\n");
    s
}

/// The `$n` placeholder for a field, with a `::text::numeric` cast for `BigInt`
/// (bound as a decimal string) — matching the migration's `NUMERIC` column.
fn placeholder(n: usize, ty: &FieldType) -> String {
    match ty {
        FieldType::Scalar(Scalar::BigInt) => format!("${n}::text::numeric"),
        _ => format!("${n}"),
    }
}

/// Quote a column identifier so SQL keywords (`from`, `to`, …) are valid — the
/// same rule the migration generator uses, so the two agree.
fn quote_ident(col: &str) -> String {
    format!("\"{}\"", col.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_schema;

    fn gen(src: &str) -> String {
        render(&parse_schema(src).unwrap())
    }

    #[test]
    fn upsert_inserts_all_columns_and_updates_non_id_on_conflict() {
        let out = gen("type Transfer @entity { id: ID! amount: BigInt! note: String }");
        assert!(out.contains("impl Transfer {"));
        assert!(out.contains("pub async fn upsert<'e, E>"));
        assert!(out.contains("E: sqlx::PgExecutor<'e>"));
        // All three columns inserted, quoted.
        assert!(out.contains("INSERT INTO transfers (\"id\", \"amount\", \"note\")"));
        // BigInt binds as text and casts to numeric; others are plain $n.
        assert!(
            out.contains("VALUES ($1, $2::text::numeric, $3)"),
            "got:\n{out}"
        );
        // Upsert on the PK; the id is the conflict target, not an update target.
        assert!(out.contains("ON CONFLICT (\"id\") DO UPDATE SET"));
        assert!(out.contains("\"amount\" = EXCLUDED.\"amount\""));
        assert!(out.contains("\"note\" = EXCLUDED.\"note\""));
        assert!(!out.contains("\"id\" = EXCLUDED.\"id\""));
        // Binds are in column order.
        assert!(out.contains(".bind(&self.id)"));
        assert!(out.contains(".bind(&self.amount)"));
        assert!(out.contains(".bind(&self.note)"));
        assert!(out.contains(".execute(executor)"));
    }

    #[test]
    fn sql_is_a_raw_string_literal() {
        // The SQL embeds double-quoted identifiers (`"from"`), so it MUST be a raw
        // literal — a plain `"…"` would be closed by the first `"` and not compile.
        // (This exact bug shipped past the substring asserts once; guard it.)
        let out = gen("type Transfer @entity { id: ID! from: String! }");
        assert!(
            out.contains("r#\"INSERT INTO transfers"),
            "SQL must be a raw string literal, got:\n{out}"
        );
        // No bare `"INSERT` (which would be a broken plain literal).
        assert!(!out.contains("            \"INSERT INTO"));
    }

    #[test]
    fn id_only_entity_does_nothing_on_conflict() {
        // No non-id columns → an empty `DO UPDATE SET` would be a SQL error.
        let out = gen("type Tag @entity { id: ID! }");
        assert!(
            out.contains("INSERT INTO tags (\"id\") VALUES ($1) ON CONFLICT (\"id\") DO NOTHING")
        );
        assert!(!out.contains("DO UPDATE"));
    }

    #[test]
    fn keyword_columns_are_quoted() {
        let out = gen("type Transfer @entity { id: ID! from: String! to: String! }");
        assert!(out.contains("(\"id\", \"from\", \"to\")"));
        assert!(out.contains("\"from\" = EXCLUDED.\"from\""));
    }

    #[test]
    fn numeric_cast_detected_only_with_bigint() {
        assert!(uses_numeric_cast(
            &parse_schema("type T @entity { id: ID! a: BigInt! }").unwrap()
        ));
        assert!(!uses_numeric_cast(
            &parse_schema("type T @entity { id: ID! a: Int! }").unwrap()
        ));
    }
}
