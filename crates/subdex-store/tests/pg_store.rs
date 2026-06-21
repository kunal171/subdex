//! Integration tests for [`PgStore`] against a real Postgres.
//!
//! **Database-dependent** and therefore `#[ignore]`d by default. Run with a
//! Postgres available:
//!
//! ```bash
//! docker run -d --name pg -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=subdex \
//!     -p 55432:5432 postgres:16-alpine
//! SUBDEX_TEST_DB=postgres://postgres:postgres@localhost:55432/subdex \
//!     cargo test -p subdex-store --test pg_store -- --ignored --nocapture
//! ```
//!
//! Each test uses a uniquely-named throwaway database created/dropped around it,
//! so they are isolated and can run concurrently.

use sqlx::{Connection, PgConnection};
use subdex_core::{Block, BlockId, Store};
use subdex_store::{PgStore, StoreConfig};

fn admin_url() -> String {
    std::env::var("SUBDEX_TEST_DB")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:55432/subdex".to_string())
}

/// Replace the database name in a postgres URL.
fn with_db(url: &str, db: &str) -> String {
    let (base, _old) = url.rsplit_once('/').expect("url has a /db part");
    format!("{base}/{db}")
}

/// Create a fresh throwaway database and return its URL. The caller drops it via
/// [`drop_db`].
async fn make_db(suffix: &str) -> String {
    let admin = admin_url();
    let db = format!("subdex_test_{suffix}");
    let mut conn = PgConnection::connect(&admin).await.expect("connect admin");
    // Defensive: drop if a previous run leaked it. `raw_sql` takes an owned
    // String (sqlx 0.9 requires the query to be 'static for DDL).
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
    let admin = admin_url();
    let db = format!("subdex_test_{suffix}");
    if let Ok(mut conn) = PgConnection::connect(&admin).await {
        let _ = sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {db} WITH (FORCE)"
        )))
        .execute(&mut conn)
        .await;
        conn.close().await.ok();
    }
}

fn block(number: u32, hash: &str, parent: &str, spec: u32) -> Block {
    Block {
        id: BlockId {
            number,
            hash: hash.into(),
        },
        parent_hash: parent.into(),
        timestamp: Some(1_700_000_000_000 + number as u64),
        spec_version: spec,
        finalized: true,
        extrinsics: vec![],
        events: vec![],
    }
}

/// Commit one block through the store's transaction lifecycle.
async fn commit_block(store: &PgStore, b: &Block) {
    let mut tx = store.begin().await.expect("begin");
    store.set_cursor(&mut tx, b).await.expect("set_cursor");
    store.commit(tx).await.expect("commit");
}

#[tokio::test]
#[ignore = "database: needs Postgres; run with --ignored"]
async fn init_is_idempotent_and_cursor_starts_empty() {
    let url = make_db("init").await;
    let store = PgStore::connect(StoreConfig::new(&url))
        .await
        .expect("connect");

    store.init().await.expect("init");
    // Running init again must not error (migrations are idempotent).
    store.init().await.expect("init again");

    assert!(
        store.cursor().await.expect("cursor").is_none(),
        "no blocks yet"
    );
    assert!(store.hash_at(1).await.expect("hash_at").is_none());

    drop_db("init").await;
}

#[tokio::test]
#[ignore = "database: needs Postgres; run with --ignored"]
async fn commit_advances_cursor_and_records_metadata() {
    let url = make_db("commit").await;
    let store = PgStore::connect(StoreConfig::new(&url))
        .await
        .expect("connect");
    store.init().await.expect("init");

    commit_block(&store, &block(1, "0xaa", "0x00", 145)).await;
    commit_block(&store, &block(2, "0xbb", "0xaa", 145)).await;
    commit_block(&store, &block(3, "0xcc", "0xbb", 147)).await;

    let cursor = store.cursor().await.expect("cursor").expect("some");
    assert_eq!(cursor.number, 3, "cursor is the highest block");
    assert_eq!(cursor.hash, "0xcc");

    assert_eq!(store.hash_at(2).await.expect("h2"), Some("0xbb".into()));
    assert_eq!(store.hash_at(99).await.expect("h99"), None);

    drop_db("commit").await;
}

#[tokio::test]
#[ignore = "database: needs Postgres; run with --ignored"]
async fn set_cursor_upsert_is_idempotent() {
    let url = make_db("upsert").await;
    let store = PgStore::connect(StoreConfig::new(&url))
        .await
        .expect("connect");
    store.init().await.expect("init");

    commit_block(&store, &block(5, "0xold", "0x04", 145)).await;
    // Re-commit the same height with a different hash (e.g. after a rollback the
    // corrected chain re-indexes height 5). ON CONFLICT should update in place.
    commit_block(&store, &block(5, "0xnew", "0x04", 146)).await;

    let cursor = store.cursor().await.expect("cursor").expect("some");
    assert_eq!(cursor.number, 5);
    assert_eq!(cursor.hash, "0xnew", "re-commit updated the hash in place");

    drop_db("upsert").await;
}

#[tokio::test]
#[ignore = "database: needs Postgres; run with --ignored"]
async fn rollback_removes_blocks_above_fork() {
    let url = make_db("rollback").await;
    let store = PgStore::connect(StoreConfig::new(&url))
        .await
        .expect("connect");
    store.init().await.expect("init");

    for n in 1..=5 {
        let parent = if n == 1 {
            "0x00".to_string()
        } else {
            format!("0x{:02x}", n - 1)
        };
        commit_block(&store, &block(n, &format!("0x{n:02x}"), &parent, 145)).await;
    }
    assert_eq!(store.cursor().await.expect("c").expect("s").number, 5);

    // Reorg detected at height 3: roll back everything above 3.
    store.rollback_to(3).await.expect("rollback");

    let cursor = store.cursor().await.expect("c").expect("s");
    assert_eq!(cursor.number, 3, "cursor back at fork point");
    assert!(
        store.hash_at(4).await.expect("h4").is_none(),
        "height 4 removed"
    );
    assert!(
        store.hash_at(5).await.expect("h5").is_none(),
        "height 5 removed"
    );
    assert!(
        store.hash_at(3).await.expect("h3").is_some(),
        "height 3 retained"
    );

    drop_db("rollback").await;
}
