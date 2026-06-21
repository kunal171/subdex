//! GraphQL API for the `transfers` example.
//!
//! Exposes the indexed `transfers` rows as a queryable API, composed with the
//! framework's built-in `indexerStatus` query. This demonstrates the full
//! "index *and* serve" shape: the same Postgres pool the indexer writes to backs
//! the read API.

use async_graphql::{Context, MergedObject, Object, SimpleObject};
use sqlx::{PgPool, Row};
use subdex_graphql::StatusQuery;

/// One indexed transfer (a row of the `transfers` table).
#[derive(SimpleObject, Debug, Clone)]
pub struct Transfer {
    /// Block height the event was emitted in.
    pub block_height: i64,
    /// 0-based index of the event within the block.
    pub event_index: i64,
    /// `"deposit"` (Assets.Deposited) or `"withdraw"` (Assets.Withdrawn).
    pub direction: String,
    /// Asset id moved, if decoded.
    pub asset_id: Option<i64>,
    /// SS58 address of the account, if decoded.
    pub account: Option<String>,
    /// Amount moved, as a decimal string (balances can exceed i64).
    pub amount: Option<String>,
}

/// Query resolvers for the example's own `transfers` data.
#[derive(Default)]
pub struct TransfersQuery;

#[Object]
impl TransfersQuery {
    /// Recent transfers, newest block first. `limit` is clamped to `[1, 200]`
    /// (default 25); `direction` optionally filters to `"deposit"`/`"withdraw"`.
    async fn transfers(
        &self,
        ctx: &Context<'_>,
        limit: Option<i32>,
        direction: Option<String>,
    ) -> async_graphql::Result<Vec<Transfer>> {
        let pool = ctx.data::<PgPool>()?;
        let limit = limit.unwrap_or(25).clamp(1, 200) as i64;

        // Two static queries (with/without the direction filter) keep the SQL
        // parameterized — no string interpolation of user input.
        let rows = match direction {
            Some(dir) => {
                sqlx::query(
                    "SELECT block_height, event_index, direction, asset_id, account, \
                            amount::text AS amount \
                     FROM transfers WHERE direction = $1 \
                     ORDER BY block_height DESC, event_index DESC LIMIT $2",
                )
                .bind(dir)
                .bind(limit)
                .fetch_all(pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT block_height, event_index, direction, asset_id, account, \
                            amount::text AS amount \
                     FROM transfers \
                     ORDER BY block_height DESC, event_index DESC LIMIT $1",
                )
                .bind(limit)
                .fetch_all(pool)
                .await?
            }
        };

        Ok(rows
            .into_iter()
            .map(|r| Transfer {
                block_height: r.get("block_height"),
                event_index: r.get("event_index"),
                direction: r.get("direction"),
                asset_id: r.get("asset_id"),
                account: r.get("account"),
                amount: r.get("amount"),
            })
            .collect())
    }

    /// Total number of indexed transfers.
    async fn transfers_count(&self, ctx: &Context<'_>) -> async_graphql::Result<i64> {
        let pool = ctx.data::<PgPool>()?;
        let row = sqlx::query("SELECT count(*) AS n FROM transfers")
            .fetch_one(pool)
            .await?;
        Ok(row.get("n"))
    }
}

/// The example's combined query root: the example's `transfers` queries **plus**
/// the framework's built-in `indexerStatus`. `MergedObject` flattens both into a
/// single GraphQL Query type.
#[derive(MergedObject, Default)]
pub struct QueryRoot(TransfersQuery, StatusQuery);

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Schema};

    /// The merged schema exposes both the example's queries and the built-in
    /// status query. Pure introspection — no database needed.
    #[test]
    fn merged_schema_exposes_all_queries() {
        let schema = Schema::build(QueryRoot::default(), EmptyMutation, EmptySubscription).finish();
        let sdl = schema.sdl();
        for field in ["transfers", "transfersCount", "indexerStatus"] {
            assert!(
                sdl.contains(field),
                "missing query `{field}` in SDL:\n{sdl}"
            );
        }
        assert!(sdl.contains("type Transfer"));
    }
}
