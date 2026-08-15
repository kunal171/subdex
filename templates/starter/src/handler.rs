//! Your handler: map chain events to rows in your generated tables.
//!
//! This starter indexes `Balances.Transfer { from, to, amount }` into the
//! `Transfer` entity from `schema.graphql`. Edit the schema + this file to index
//! whatever your chain emits.
//!
//! The shape is **load → extract → process → persist**:
//! - the processor hands you each `Block` (load);
//! - you read event fields with the `subdex_source::value` helpers (extract);
//! - you build a generated entity struct (process);
//! - you call its generated `.upsert(...)` on the transaction (persist), so the
//!   write commits atomically with the indexer cursor.

use async_trait::async_trait;
use subdex::{Block, Handler, Result, Store, SubdexError};
use subdex_source::value::{field_account_ss58, field_bigint};
use subdex_store::PgStore;

// The generated entity struct + its typed `.upsert()`.
use crate::generated::entities::Transfer;

/// Embeds the generated migration (`migrations/0001_schema.sql`) so the table is
/// created once, in order, tracked in `_sqlx_migrations_my_handler`.
static MIGRATOR: subdex_store::Migrator = sqlx::migrate!("./migrations");

pub struct MyHandler;

#[async_trait]
impl Handler<PgStore> for MyHandler {
    /// Apply the generated migration once at startup.
    async fn init(&self, store: &PgStore) -> Result<()> {
        store.run_handler_migrations(&MIGRATOR, self.name()).await?;

        // OPTIONAL — fast backfill: drop the `@index` on `block_height` while
        // bulk-inserting, and let the framework recreate it at head. Only drops on
        // a fresh DB; a no-op on resume. Never defer the PK or a UNIQUE you upsert
        // on. See docs/GUIDE.md § Backfill fast.
        store
            .defer_index(
                "transfers_block_height_idx",
                "CREATE INDEX IF NOT EXISTS transfers_block_height_idx ON transfers (block_height)",
            )
            .await?;
        Ok(())
    }

    /// Called once per block, in order. Write rows on `tx`.
    async fn process_block<'a>(
        &self,
        block: &Block,
        tx: &mut <PgStore as Store>::Tx<'a>,
    ) -> Result<()> {
        for ev in &block.events {
            if (ev.pallet.as_str(), ev.name.as_str()) != ("Balances", "Transfer") {
                continue;
            }
            let row = Transfer {
                // A stable, unique id per event: block height + event index.
                id: format!("{}-{}", block.id.number, ev.index),
                block_height: block.id.number as i32,
                from: field_account_ss58(&ev.fields, "from", 42).unwrap_or_default(),
                to: field_account_ss58(&ev.fields, "to", 42),
                amount: field_bigint(&ev.fields, "amount").unwrap_or_else(|| "0".into()),
            };
            row.upsert(&mut **tx)
                .await
                .map_err(|e| SubdexError::Handler(format!("upsert transfer: {e}")))?;
        }
        Ok(())
    }

    fn name(&self) -> &str {
        "my_handler"
    }
}
