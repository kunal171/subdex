//! The HTTP server harness: serve an `async-graphql` [`Schema`] over `axum`,
//! with a GraphiQL playground for interactive querying.

use crate::config::GraphqlConfig;
use crate::status::StatusQuery;
use async_graphql::{EmptyMutation, EmptySubscription, ObjectType, Schema, SubscriptionType};
use async_graphql_axum::GraphQL;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Json, Router};
use sqlx::PgPool;

/// A schema serving only the built-in [`StatusQuery`]. Convenient when you just
/// want the indexer-status API without writing any resolvers; otherwise build
/// your own `Schema` (composing [`StatusQuery`]) and pass it to [`serve`].
pub type StatusSchema = Schema<StatusQuery, EmptyMutation, EmptySubscription>;

/// Build a [`StatusSchema`] with the given pool injected as context data.
pub fn build_status_schema(pool: PgPool) -> StatusSchema {
    Schema::build(StatusQuery, EmptyMutation, EmptySubscription)
        .data(pool)
        .finish()
}

/// Build the axum [`Router`] that serves `schema`:
/// - `GET  {path}` → the GraphiQL playground (interactive UI),
/// - `POST {path}` → GraphQL query execution,
/// - `GET  /healthz` → liveness: the process is up and serving (always 200).
///
/// Exposed separately from [`serve`] so callers can mount it into a larger axum
/// app, add middleware, or test it with `tower`.
///
/// Use [`router_with_health`] instead to also expose `/readyz`, which checks the
/// database — load balancers and orchestrators generally want that one.
pub fn router<Q, M, S>(schema: Schema<Q, M, S>, config: &GraphqlConfig) -> Router
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
{
    let path = config.path.clone();
    let graphiql_path = path.clone();
    Router::new().route("/healthz", get(healthz)).route(
        &path,
        get(move || async move {
            Html(
                async_graphql::http::GraphiQLSource::build()
                    .endpoint(&graphiql_path)
                    .title("subdex GraphQL")
                    .finish(),
            )
            .into_response()
        })
        .post_service(GraphQL::new(schema)),
    )
}

/// Liveness: the process is up and the HTTP stack is serving. Deliberately does
/// no I/O — a liveness probe that fails on a transient database blip would get
/// the container killed and restarted, which does not fix a database.
async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// Readiness: the process can actually serve queries, i.e. the pool hands out a
/// connection. Returns 200 with the indexer's progress, or 503 when the database
/// is unreachable. This is the one to point a load-balancer target group at.
async fn readyz(State(pool): State<PgPool>) -> impl IntoResponse {
    match crate::status::load_status(&pool).await {
        Ok(status) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "ready",
                "height": status.height,
                "indexed_blocks": status.indexed_blocks,
            })),
        ),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "status": "unavailable", "error": e.to_string() })),
        ),
    }
}

/// [`router`] plus `GET /readyz`, which verifies the database is reachable and
/// reports the indexer's height. Needs the pool, so it is a separate constructor
/// rather than a flag on [`router`].
pub fn router_with_health<Q, M, S>(
    schema: Schema<Q, M, S>,
    config: &GraphqlConfig,
    pool: PgPool,
) -> Router
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
{
    router(schema, config).route("/readyz", get(readyz).with_state(pool))
}

/// Bind to `config.addr` and serve `schema` until the process is stopped.
pub async fn serve<Q, M, S>(schema: Schema<Q, M, S>, config: GraphqlConfig) -> std::io::Result<()>
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
{
    let app = router(schema, &config);
    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    tracing::info!(addr = %config.addr, path = %config.path, "subdex GraphQL listening");
    axum::serve(listener, app).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn status_schema_builds_with_pool_context() {
        // Build the schema with a lazily-connected pool handle (no connection is
        // made until first query — but constructing the handle needs a Tokio
        // context, hence the async test). Verifies the generic bounds and that
        // the router wiring compiles; DB-backed querying is covered by the gated
        // integration test.
        let pool = PgPoolStub::lazy();
        let schema = build_status_schema(pool);
        assert!(schema.sdl().contains("indexerStatus"));

        // The router builds over the schema (mounts GET playground + POST query).
        let _app = router(schema, &GraphqlConfig::default());
    }

    #[tokio::test]
    async fn healthz_returns_200_without_touching_the_database() {
        // Bind an ephemeral port and drive the real axum stack. The pool points
        // at a database that does not exist: /healthz must still answer 200,
        // which is the whole point of a liveness probe that does no I/O.
        let app = router(
            build_status_schema(PgPoolStub::lazy()),
            &GraphqlConfig::default(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let resp = reqwest::get(format!("http://{addr}/healthz"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn readyz_reports_503_when_the_database_is_unreachable() {
        // Same setup, but /readyz does query the database — so an unreachable
        // one must surface as 503, not a panic or a hang.
        let pool = PgPoolStub::lazy();
        let app = router_with_health(
            build_status_schema(pool.clone()),
            &GraphqlConfig::default(),
            pool,
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let resp = reqwest::get(format!("http://{addr}/readyz")).await.unwrap();
        assert_eq!(resp.status(), 503);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "unavailable");
    }

    /// Helper to obtain a `PgPool` handle without connecting (sqlx connects
    /// lazily on first use).
    struct PgPoolStub;
    impl PgPoolStub {
        fn lazy() -> PgPool {
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://localhost/does_not_connect_until_used")
                .expect("lazy pool")
        }
    }
}
