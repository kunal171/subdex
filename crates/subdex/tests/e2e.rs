//! End-to-end integration test: a real [`SubxtSource`] (Unit mainnet) feeding a
//! real [`PgStore`] (Postgres) through the [`Processor`], with a handler that
//! writes decoded events into its own table.
//!
//! **Network + database dependent**, so `#[ignore]`d. Run with both available:
//!
//! ```bash
//! docker run -d --name pg -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=subdex \
//!     -p 55432:5432 postgres:16-alpine
//! SUBDEX_TEST_DB=postgres://postgres:postgres@localhost:55432/subdex \
//! SUBDEX_TEST_WS=wss://archive2.mainnet-unit.com \
//!     cargo test -p subdex --test e2e -- --ignored --nocapture
//! ```

use async_trait::async_trait;
use sqlx::{Connection, PgConnection, Row};
use std::sync::Arc;
use subdex::{Block, DataSource, Handler, Processor, ProcessorConfig, Result, Store};
use subdex_source::{SourceConfig, SubxtSource};
use subdex_store::{PgStore, StoreConfig};

fn db_url() -> String {
    std::env::var("SUBDEX_TEST_DB")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:55432/subdex".to_string())
}

fn ws_url() -> String {
    std::env::var("SUBDEX_TEST_WS")
        .unwrap_or_else(|_| "wss://archive2.mainnet-unit.com".to_string())
}

fn with_db(url: &str, db: &str) -> String {
    let (base, _) = url.rsplit_once('/').expect("url has /db");
    format!("{base}/{db}")
}

async fn make_db(suffix: &str) -> String {
    let admin = db_url();
    let db = format!("subdex_e2e_{suffix}");
    let mut conn = PgConnection::connect(&admin).await.expect("connect admin");
    let _ = sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {db} WITH (FORCE)"
    )))
    .execute(&mut conn)
    .await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db}")))
        .execute(&mut conn)
        .await
        .expect("create db");
    conn.close().await.ok();
    with_db(&admin, &db)
}

async fn drop_db(suffix: &str) {
    let admin = db_url();
    let db = format!("subdex_e2e_{suffix}");
    if let Ok(mut conn) = PgConnection::connect(&admin).await {
        let _ = sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {db} WITH (FORCE)"
        )))
        .execute(&mut conn)
        .await;
        conn.close().await.ok();
    }
}

/// A real handler: counts events per pallet by inserting one row per event into
/// its own `event_log` table, demonstrating that handler writes commit on the
/// same transaction as the cursor.
struct EventLogHandler;

#[async_trait]
impl Handler<PgStore> for EventLogHandler {
    async fn init(&self, store: &PgStore) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS event_log (\
                id BIGSERIAL PRIMARY KEY, \
                block_height BIGINT NOT NULL, \
                pallet TEXT NOT NULL, \
                name TEXT NOT NULL)",
        )
        .execute(store.pool())
        .await
        .map_err(|e| subdex::SubdexError::Handler(format!("create table: {e}")))?;
        Ok(())
    }

    async fn process_block<'a>(
        &self,
        block: &Block,
        tx: &mut <PgStore as Store>::Tx<'a>,
    ) -> Result<()> {
        for ev in &block.events {
            sqlx::query("INSERT INTO event_log (block_height, pallet, name) VALUES ($1, $2, $3)")
                .bind(block.id.number as i64)
                .bind(&ev.pallet)
                .bind(&ev.name)
                .execute(&mut **tx)
                .await
                .map_err(|e| subdex::SubdexError::Handler(format!("insert: {e}")))?;
        }
        Ok(())
    }

    fn name(&self) -> &str {
        "event-log"
    }
}

/// Full pipeline: connect source + store, backfill a small recent window from
/// mainnet, and assert the cursor advanced and the handler wrote events on the
/// same transaction.
#[tokio::test]
#[ignore = "network+database: needs Unit RPC and Postgres; run with --ignored"]
async fn backfills_mainnet_into_postgres() {
    let url = make_db("backfill").await;

    let source = SubxtSource::connect(SourceConfig::new(ws_url()).with_batch_size(5))
        .await
        .expect("connect source");
    let store = PgStore::connect(StoreConfig::new(&url))
        .await
        .expect("connect store");

    // Index a small recent window so the test is quick.
    let head = source.finalized_head().await.expect("head");
    let start = head.saturating_sub(3);

    let processor = Processor::new(
        source,
        store.clone(),
        vec![Arc::new(EventLogHandler)],
        ProcessorConfig::from_height(start).with_batch_size(5),
    );

    processor.init().await.expect("init");
    let next = processor.backfill().await.expect("backfill");

    // Cursor advanced to the head; resume height is head + 1.
    let cursor = processor
        .store()
        .cursor()
        .await
        .expect("cursor")
        .expect("some");
    assert_eq!(cursor.number, head, "cursor at finalized head");
    assert_eq!(next, head + 1);

    // Handler wrote events on the same DB; there should be some.
    let count: i64 = sqlx::query("SELECT count(*) FROM event_log")
        .fetch_one(store.pool())
        .await
        .expect("count")
        .get(0);
    assert!(count > 0, "handler should have logged events, got {count}");

    // Every logged event's height is within the indexed window.
    let in_range: i64 = sqlx::query("SELECT count(*) FROM event_log WHERE block_height < $1")
        .bind(start as i64)
        .fetch_one(store.pool())
        .await
        .expect("range count")
        .get(0);
    assert_eq!(in_range, 0, "no events below the start height");

    println!("OK: indexed blocks {start}..={head} into Postgres, {count} events logged");

    drop_db("backfill").await;
}
