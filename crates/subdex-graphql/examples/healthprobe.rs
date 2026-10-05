//! Serve just the health routes against a real database, for probing by hand:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:55432/subdex \
//!   cargo run -p subdex-graphql --example healthprobe
//! curl -i localhost:4350/healthz   # 200 ok
//! curl -i localhost:4350/readyz    # 200 + height, or 503
//! ```
use subdex_graphql::{build_status_schema, router_with_health, GraphqlConfig};

#[tokio::main]
async fn main() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect(&url)
        .await
        .expect("connect");
    let app = router_with_health(
        build_status_schema(pool.clone()),
        &GraphqlConfig::default(),
        pool,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:4350")
        .await
        .unwrap();
    println!("health routes on http://127.0.0.1:4350");
    axum::serve(listener, app).await.unwrap();
}
