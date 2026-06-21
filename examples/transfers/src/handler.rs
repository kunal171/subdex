//! The example [`Handler`]: records `Assets.Deposited` / `Assets.Withdrawn`
//! events into a `transfers` table.

use crate::value_ext::{as_account_ss58, as_u128, field};
use async_trait::async_trait;
use subdex::{Block, Handler, Result, Store, SubdexError};
use subdex_store::PgStore;

/// Indexes asset deposit/withdraw events. Each matching event becomes one row in
/// the handler's own `transfers` table; the writes go on the processor's
/// transaction so they commit atomically with the indexer cursor.
pub struct TransfersHandler;

impl TransfersHandler {
    /// The two event names we care about, mapped to a `direction` label stored
    /// in the table.
    fn direction(pallet: &str, name: &str) -> Option<&'static str> {
        match (pallet, name) {
            ("Assets", "Deposited") => Some("deposit"),
            ("Assets", "Withdrawn") => Some("withdraw"),
            _ => None,
        }
    }
}

#[async_trait]
impl Handler<PgStore> for TransfersHandler {
    /// Create the example's own table once at startup. Note this runs outside the
    /// per-block transaction (it's schema setup), using the store's pool.
    async fn init(&self, store: &PgStore) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS transfers (\
                id            BIGSERIAL PRIMARY KEY, \
                block_height  BIGINT  NOT NULL, \
                event_index   BIGINT  NOT NULL, \
                direction     TEXT    NOT NULL, \
                asset_id      BIGINT, \
                account       TEXT, \
                amount        NUMERIC, \
                UNIQUE (block_height, event_index))",
        )
        .execute(store.pool())
        .await
        .map_err(|e| SubdexError::Handler(format!("create transfers table: {e}")))?;
        Ok(())
    }

    async fn process_block<'a>(
        &self,
        block: &Block,
        tx: &mut <PgStore as Store>::Tx<'a>,
    ) -> Result<()> {
        for ev in &block.events {
            let Some(direction) = Self::direction(&ev.pallet, &ev.name) else {
                continue;
            };

            // Pull the named fields; tolerate a shape we don't recognize by
            // recording NULLs rather than failing the block.
            let asset_id = field(&ev.fields, "asset_id").and_then(as_u128);
            let account = field(&ev.fields, "who").and_then(as_account_ss58);
            let amount = field(&ev.fields, "amount").and_then(as_u128);

            sqlx::query(
                "INSERT INTO transfers \
                    (block_height, event_index, direction, asset_id, account, amount) \
                 VALUES ($1, $2, $3, $4, $5, $6::text::numeric) \
                 ON CONFLICT (block_height, event_index) DO NOTHING",
            )
            .bind(block.id.number as i64)
            .bind(ev.index as i64)
            .bind(direction)
            .bind(asset_id.map(|v| v as i64))
            .bind(account)
            // amount is u128; store as NUMERIC via its decimal string to avoid
            // overflowing i64 for large balances.
            .bind(amount.map(|v| v.to_string()))
            .execute(&mut **tx)
            .await
            .map_err(|e| SubdexError::Handler(format!("insert transfer: {e}")))?;
        }
        Ok(())
    }

    fn name(&self) -> &str {
        "transfers"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_maps_only_known_events() {
        assert_eq!(
            TransfersHandler::direction("Assets", "Deposited"),
            Some("deposit")
        );
        assert_eq!(
            TransfersHandler::direction("Assets", "Withdrawn"),
            Some("withdraw")
        );
        assert_eq!(TransfersHandler::direction("Assets", "Created"), None);
        assert_eq!(TransfersHandler::direction("Balances", "Deposited"), None);
    }
}
