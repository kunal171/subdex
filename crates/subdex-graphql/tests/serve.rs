//! Integration test: serve the built-in indexer-status GraphQL over HTTP against
//! a real Postgres and query it end to end.
//!
//! **Database-dependent**, so `#[ignore]`d. Run with a Postgres available:
//!
//! ```bash
//! docker run -d --name pg -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=subdex \
//!     -p 55432:5432 postgres:16-alpine
//! SUBDEX_TEST_DB=postgres://postgres:postgres@localhost:55432/subdex \
//!     cargo test -p subdex-graphql --test serve -- --ignored --nocapture
//! ```

use sqlx::{Connection, PgConnection, PgPool};
use subdex_graphql::{build_status_schema, router, GraphqlConfig};

fn admin_url() -> String {
    std::env::var("SUBDEX_TEST_DB")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:55432/subdex".to_string())
}

fn with_db(url: &str, db: &str) -> String {
    let (base, _) = url.rsplit_once('/').expect("url has /db");
    format!("{base}/{db}")
}

async fn make_db(suffix: &str) -> String {
    let admin = admin_url();
    let db = format!("subdex_gql_{suffix}");
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
    let admin = admin_url();
    let db = format!("subdex_gql_{suffix}");
    if let Ok(mut conn) = PgConnection::connect(&admin).await {
        let _ = sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {db} WITH (FORCE)"
        )))
        .execute(&mut conn)
        .await;
        conn.close().await.ok();
    }
}

/// Seed a minimal `subdex_block` table with two rows so the status query has
/// something to report.
async fn seed(pool: &PgPool) {
    sqlx::raw_sql(
        "CREATE TABLE subdex_block ( \
            height BIGINT PRIMARY KEY, hash TEXT NOT NULL, parent_hash TEXT NOT NULL, \
            timestamp BIGINT, spec_version BIGINT NOT NULL, \
            indexed_at TIMESTAMPTZ NOT NULL DEFAULT now()); \
         INSERT INTO subdex_block (height, hash, parent_hash, timestamp, spec_version) VALUES \
            (10, '0x0a', '0x09', 1700000000000, 145), \
            (11, '0x0b', '0x0a', 1700000006000, 147);",
    )
    .execute(pool)
    .await
    .expect("seed");
}

/// Boot the GraphQL server on an ephemeral port, query `indexerStatus` over real
/// HTTP, and assert the response reflects the seeded cursor.
#[tokio::test]
#[ignore = "database: needs Postgres; run with --ignored"]
async fn serves_indexer_status_over_http() {
    let url = make_db("status").await;
    let pool = PgPool::connect(&url).await.expect("connect pool");
    seed(&pool).await;

    // Bind to port 0 → OS picks a free port; build the router and serve it on a
    // background task.
    let config = GraphqlConfig::on_port(0);
    let schema = build_status_schema(pool.clone());
    let app = router(schema, &config);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    // Query over HTTP.
    let body = serde_json::json!({
        "query": "{ indexerStatus { height hash specVersion blockTimestamp indexedBlocks } }"
    });
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/graphql"))
        .json(&body)
        .send()
        .await
        .expect("send")
        .json::<serde_json::Value>()
        .await
        .expect("json");

    let s = &resp["data"]["indexerStatus"];
    assert_eq!(s["height"], 11, "reports the highest indexed height");
    assert_eq!(s["hash"], "0x0b");
    assert_eq!(s["specVersion"], 147);
    assert_eq!(s["blockTimestamp"], 1_700_000_006_000i64);
    assert_eq!(s["indexedBlocks"], 2);

    println!("OK: indexerStatus served over HTTP -> {s}");

    drop_db("status").await;
}
