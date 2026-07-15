//! [`AssetsHandler`] — indexes `Assets.Created` / `Assets.Destroyed` events into
//! an `asset_lifecycle` table.
//!
//! This is the **high-throughput** handler shape: it overrides `process_batch`
//! (instead of `process_block`) to accumulate every matching event across the
//! *whole batch* in memory, then write them all in **one** multi-row INSERT —
//! avoiding the per-row-upsert-per-block anti-pattern. It still commits on the
//! processor's transaction, so it stays atomic with the cursor and the other
//! handler.

use crate::value_ext::{as_account_ss58, as_u128, field};
use async_trait::async_trait;
use subdex::{Block, Handler, Result, Store, SubdexError};
use subdex_store::PgStore;

/// One accumulated row, ready to bulk-insert.
struct Row {
    block_height: i64,
    event_index: i64,
    action: &'static str,
    asset_id: Option<i64>,
    owner: Option<String>,
}

/// Indexes asset create/destroy lifecycle events with a bulk-write path.
pub struct AssetsHandler;

impl AssetsHandler {
    fn action(pallet: &str, name: &str) -> Option<&'static str> {
        match (pallet, name) {
            ("Assets", "Created") => Some("created"),
            ("Assets", "Destroyed") => Some("destroyed"),
            _ => None,
        }
    }

    /// Extract the accumulated rows for one block (pure; unit-testable).
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

#[async_trait]
impl Handler<PgStore> for AssetsHandler {
    async fn init(&self, store: &PgStore) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS asset_lifecycle (\
                id           BIGSERIAL PRIMARY KEY, \
                block_height BIGINT NOT NULL, \
                event_index  BIGINT NOT NULL, \
                action       TEXT NOT NULL, \
                asset_id     BIGINT, \
                owner        TEXT, \
                UNIQUE (block_height, event_index))",
        )
        .execute(store.pool())
        .await
        .map_err(|e| SubdexError::Handler(format!("create asset_lifecycle: {e}")))?;
        Ok(())
    }

    // We only override process_batch (not process_block), so the default
    // per-block delegation is bypassed — this handler always sees the whole batch.
    async fn process_batch<'a>(
        &self,
        blocks: &[Block],
        tx: &mut <PgStore as Store>::Tx<'a>,
    ) -> Result<()> {
        // Accumulate every matching event across the whole batch.
        let rows: Vec<Row> = blocks.iter().flat_map(Self::rows_for_block).collect();
        if rows.is_empty() {
            return Ok(());
        }

        // One multi-row INSERT for the entire batch, built with bound parameters
        // (no string interpolation of values). This is the throughput win over
        // inserting row-by-row.
        let mut sql = String::from(
            "INSERT INTO asset_lifecycle \
                (block_height, event_index, action, asset_id, owner) VALUES ",
        );
        for i in 0..rows.len() {
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

        // The SQL is built only from a fixed template + generated `$N`
        // placeholders — no user/chain data is interpolated into the string
        // (every value is a bound parameter below), so this is audited-safe.
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        for r in &rows {
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

    // process_block is still required by the trait, but it is never called for
    // this handler (we overrode process_batch). Delegate defensively in case the
    // engine ever routes a single block here.
    async fn process_block<'a>(
        &self,
        block: &Block,
        tx: &mut <PgStore as Store>::Tx<'a>,
    ) -> Result<()> {
        self.process_batch(std::slice::from_ref(block), tx).await
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
