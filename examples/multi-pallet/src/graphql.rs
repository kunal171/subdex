//! GraphQL API exposing **both** handlers' entity types, composed with the
//! framework's built-in `indexerStatus` query. This shows that a multi-handler
//! indexer serves all its tables from the one Postgres pool the indexer writes to.

use async_graphql::{Context, MergedObject, Object, SimpleObject};
use sqlx::{PgPool, Row};
use subdex_graphql::StatusQuery;

/// A `Balances.Transfer` row.
#[derive(SimpleObject, Debug, Clone)]
pub struct BalanceTransfer {
    pub block_height: i64,
    pub event_index: i64,
    pub from_account: Option<String>,
    pub to_account: Option<String>,
    /// Amount as a decimal string (balances can exceed i64).
    pub amount: Option<String>,
}

/// An `Assets` lifecycle row (`created` / `destroyed`).
#[derive(SimpleObject, Debug, Clone)]
pub struct AssetEvent {
    pub block_height: i64,
    pub event_index: i64,
    pub action: String,
    pub asset_id: Option<i64>,
    pub owner: Option<String>,
}

/// Resolvers for the balances handler's table.
#[derive(Default)]
pub struct BalancesQuery;

#[Object]
impl BalancesQuery {
    /// Recent balance transfers, newest block first (`limit` clamped to 1..=200).
    async fn balance_transfers(
        &self,
        ctx: &Context<'_>,
        limit: Option<i32>,
    ) -> async_graphql::Result<Vec<BalanceTransfer>> {
        let pool = ctx.data::<PgPool>()?;
        let limit = limit.unwrap_or(25).clamp(1, 200) as i64;
        let rows = sqlx::query(
            "SELECT block_height, event_index, from_account, to_account, amount::text AS amount \
             FROM balance_transfers ORDER BY block_height DESC, event_index DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| BalanceTransfer {
                block_height: r.get("block_height"),
                event_index: r.get("event_index"),
                from_account: r.get("from_account"),
                to_account: r.get("to_account"),
                amount: r.get("amount"),
            })
            .collect())
    }
}

/// Resolvers for the assets handler's table.
#[derive(Default)]
pub struct AssetsQuery;

#[Object]
impl AssetsQuery {
    /// Recent asset lifecycle events, newest block first (`limit` clamped to 1..=200).
    async fn asset_events(
        &self,
        ctx: &Context<'_>,
        limit: Option<i32>,
    ) -> async_graphql::Result<Vec<AssetEvent>> {
        let pool = ctx.data::<PgPool>()?;
        let limit = limit.unwrap_or(25).clamp(1, 200) as i64;
        let rows = sqlx::query(
            "SELECT block_height, event_index, action, asset_id, owner \
             FROM asset_lifecycle ORDER BY block_height DESC, event_index DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| AssetEvent {
                block_height: r.get("block_height"),
                event_index: r.get("event_index"),
                action: r.get("action"),
                asset_id: r.get("asset_id"),
                owner: r.get("owner"),
            })
            .collect())
    }
}

/// The composed root: both handlers' queries plus the framework's `indexerStatus`.
#[derive(MergedObject, Default)]
pub struct QueryRoot(pub BalancesQuery, pub AssetsQuery, pub StatusQuery);
