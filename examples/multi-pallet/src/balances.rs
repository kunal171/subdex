//! [`BalancesHandler`] — indexes `Balances.Transfer` events into a
//! `balance_transfers` table.
//!
//! This is the **simple** handler shape: implement `process_block`, write one row
//! per matching event. The writes go on the processor's transaction, so they
//! commit atomically with the cursor *and* with the other handler's writes.

use crate::value_ext::{as_account_ss58, as_u128, field};
use async_trait::async_trait;
use subdex::{Block, Handler, Result, Store, SubdexError};
use subdex_store::PgStore;

/// Indexes `Balances.Transfer { from, to, amount }` events.
pub struct BalancesHandler;

#[async_trait]
impl Handler<PgStore> for BalancesHandler {
    async fn init(&self, store: &PgStore) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS balance_transfers (\
                id           BIGSERIAL PRIMARY KEY, \
                block_height BIGINT NOT NULL, \
                event_index  BIGINT NOT NULL, \
                from_account TEXT, \
                to_account   TEXT, \
                amount       NUMERIC, \
                UNIQUE (block_height, event_index))",
        )
        .execute(store.pool())
        .await
        .map_err(|e| SubdexError::Handler(format!("create balance_transfers: {e}")))?;
        Ok(())
    }

    async fn process_block<'a>(
        &self,
        block: &Block,
        tx: &mut <PgStore as Store>::Tx<'a>,
    ) -> Result<()> {
        for ev in &block.events {
            if ev.pallet != "Balances" || ev.name != "Transfer" {
                continue;
            }
            let from = field(&ev.fields, "from").and_then(as_account_ss58);
            let to = field(&ev.fields, "to").and_then(as_account_ss58);
            let amount = field(&ev.fields, "amount").and_then(as_u128);

            sqlx::query(
                "INSERT INTO balance_transfers \
                    (block_height, event_index, from_account, to_account, amount) \
                 VALUES ($1, $2, $3, $4, $5::text::numeric) \
                 ON CONFLICT (block_height, event_index) DO NOTHING",
            )
            .bind(block.id.number as i64)
            .bind(ev.index as i64)
            .bind(from)
            .bind(to)
            .bind(amount.map(|v| v.to_string()))
            .execute(&mut **tx)
            .await
            .map_err(|e| SubdexError::Handler(format!("insert balance_transfer: {e}")))?;
        }
        Ok(())
    }

    fn name(&self) -> &str {
        "balances"
    }
}
