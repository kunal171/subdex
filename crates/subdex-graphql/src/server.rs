//! The HTTP server harness: serve an `async-graphql` [`Schema`] over `axum`,
//! with a GraphiQL playground for interactive querying.

use crate::config::GraphqlConfig;
use crate::status::StatusQuery;
use async_graphql::{EmptyMutation, EmptySubscription, ObjectType, Schema, SubscriptionType};
use async_graphql_axum::GraphQL;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;
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
/// - `POST {path}` → GraphQL query execution.
///
/// Exposed separately from [`serve`] so callers can mount it into a larger axum
/// app, add middleware, or test it with `tower`.
pub fn router<Q, M, S>(schema: Schema<Q, M, S>, config: &GraphqlConfig) -> Router
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
{
    let path = config.path.clone();
    let graphiql_path = path.clone();
    Router::new().route(
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

/// Bind to `config.addr` and serve `schema` until the process is stopped.
pub async fn serve<Q, M, S>(
    schema: Schema<Q, M, S>,
    config: GraphqlConfig,
) -> std::io::Result<()>
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
