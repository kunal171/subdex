//! [`AssetsHandler`] — indexes `Assets.Created` / `Assets.Destroyed` events into
//! an `asset_lifecycle` table.
//!
//! This is the **two-phase / high-throughput** handler shape:
//! - `prepare` (pure compute, no DB) accumulates every matching event across the
//!   whole batch into rows — the engine runs this **concurrently** with the other
//!   handlers' `prepare`;
//! - [`PreparedAssets::write`] (serial, on the shared transaction) bulk-writes
//!   them in **one** multi-row INSERT.
//!
//! It stays atomic with the cursor and the other handler (all writes share the
//! one transaction), and avoids the per-row-upsert-per-block anti-pattern.

use crate::value_ext::{as_account_ss58, as_u128, field};
use async_trait::async_trait;
use subdex::{Block, Handler, Prepared, Result, Store, SubdexError};
use subdex_store::PgStore;

/// One accumulated row, ready to bulk-insert.
struct Row {
    block_height: i64,
    event_index: i64,
    action: &'static str,
    asset_id: Option<i64>,
    owner: Option<String>,
}

/// Phase-1 output: the rows to write, carried to the serial write phase.
pub struct PreparedAssets {
    rows: Vec<Row>,
}

#[async_trait]
impl Prepared<PgStore> for PreparedAssets {
    async fn write<'a>(self: Box<Self>, tx: &mut <PgStore as Store>::Tx<'a>) -> Result<()> {
        if self.rows.is_empty() {
            return Ok(());
        }
        // One multi-row INSERT for the whole batch, built with bound parameters
        // ($N placeholders only — no value interpolation). This is the throughput
        // win over inserting row-by-row.
        let mut sql = String::from(
            "INSERT INTO asset_lifecycle \
                (block_height, event_index, action, asset_id, owner) VALUES ",
        );
        for i in 0..self.rows.len() {
            let b = i * 5;
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&format!(
                "(${}, ${}, ${}, ${}, ${})",
                b + 1,
                b + 2,
                b + 3,
                b + 4,
                b + 5
            ));
        }
        sql.push_str(" ON CONFLICT (block_height, event_index) DO NOTHING");

        // The SQL is a fixed template + generated `$N` placeholders only — every
        // value is a bound parameter below — so this runtime string is audited-safe.
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        for r in &self.rows {
            q = q
                .bind(r.block_height)
                .bind(r.event_index)
                .bind(r.action)
                .bind(r.asset_id)
                .bind(r.owner.clone());
        }
        q.execute(&mut **tx)
            .await
            .map_err(|e| SubdexError::Handler(format!("bulk insert asset_lifecycle: {e}")))?;
        Ok(())
    }
}

/// Indexes asset create/destroy lifecycle events with a two-phase bulk-write path.
pub struct AssetsHandler;

impl AssetsHandler {
    fn action(pallet: &str, name: &str) -> Option<&'static str> {
        match (pallet, name) {
            ("Assets", "Created") => Some("created"),
            ("Assets", "Destroyed") => Some("destroyed"),
            _ => None,
        }
    }

    /// Extract the rows for one block (pure; unit-testable).
    fn rows_for_block(block: &Block) -> Vec<Row> {
        let mut out = Vec::new();
        for ev in &block.events {
            let Some(action) = Self::action(&ev.pallet, &ev.name) else {
                continue;
            };
            out.push(Row {
                block_height: block.id.number as i64,
                event_index: ev.index as i64,
                action,
                asset_id: field(&ev.fields, "asset_id")
                    .and_then(as_u128)
                    .map(|v| v as i64),
                owner: field(&ev.fields, "owner").and_then(as_account_ss58),
            });
        }
        out
    }
}

/// This handler's own **versioned** migrations, embedded at compile time from
/// `migrations/assets/`. Unlike an ad-hoc `CREATE TABLE IF NOT EXISTS` in `init`,
/// these apply once, in order, and are recorded — so a schema evolution (v2 adds
/// an index) reaches an existing deployment exactly once, and a fresh DB gets the
/// whole set. The framework owns `subdex_block`; the handler owns this.
static MIGRATOR: subdex_store::Migrator = sqlx::migrate!("./migrations/assets");

#[async_trait]
impl Handler<PgStore> for AssetsHandler {
    async fn init(&self, store: &PgStore) -> Result<()> {
        // Applied once, in order, tracked in `_sqlx_migrations_assets` — isolated
        // from the framework's own `_sqlx_migrations`.
        store.run_handler_migrations(&MIGRATOR, self.name()).await
    }

    // Phase 1 — pure compute (no tx). The engine runs this concurrently with the
    // other handlers' prepare; the rows are written later in the serial phase.
    async fn prepare(&self, blocks: &[Block]) -> Result<Option<Box<dyn Prepared<PgStore>>>> {
        let rows: Vec<Row> = blocks.iter().flat_map(Self::rows_for_block).collect();
        Ok(Some(Box::new(PreparedAssets { rows })))
    }

    // process_block is required by the trait but never reached: we return a
    // `Some(prepared)` from `prepare`, so the engine uses the write phase. Kept as
    // a defensive no-op-shaped delegation.
    async fn process_block<'a>(
        &self,
        block: &Block,
        tx: &mut <PgStore as Store>::Tx<'a>,
    ) -> Result<()> {
        let rows: Vec<Row> = Self::rows_for_block(block);
        Box::new(PreparedAssets { rows }).write(tx).await
    }

    fn name(&self) -> &str {
        "assets"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_maps_only_known_events() {
        assert_eq!(AssetsHandler::action("Assets", "Created"), Some("created"));
        assert_eq!(
            AssetsHandler::action("Assets", "Destroyed"),
            Some("destroyed")
        );
        assert_eq!(AssetsHandler::action("Assets", "Issued"), None);
        assert_eq!(AssetsHandler::action("Balances", "Created"), None);
    }
}
