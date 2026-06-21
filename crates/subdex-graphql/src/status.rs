//! The built-in **indexer status** GraphQL query, backed by the framework's
//! `subdex_block` bookkeeping table.
//!
//! Every subdex indexer gets this for free: a client can ask "how far has the
//! indexer progressed?" without the user writing any resolver.

use async_graphql::{Context, Object, SimpleObject};
use sqlx::{PgPool, Row};

/// A snapshot of the indexer's progress.
#[derive(SimpleObject, Debug, Clone, PartialEq)]
pub struct IndexerStatus {
    /// Highest block height the indexer has committed, or `null` if nothing has
    /// been indexed yet.
    pub height: Option<i64>,
    /// Hash of the highest indexed block.
    pub hash: Option<String>,
    /// Runtime spec version that block was decoded under.
    pub spec_version: Option<i64>,
    /// Unix-ms timestamp of the highest indexed block, if known.
    pub block_timestamp: Option<i64>,
    /// Total number of block rows retained in bookkeeping (reorg-window size).
    pub indexed_blocks: i64,
}

/// The query root providing [`IndexerStatus`]. Compose this into your own schema
/// (e.g. with `#[graphql(flatten)]`), or serve it directly via the server harness
/// (added in the next commit).
#[derive(Default)]
pub struct StatusQuery;

#[Object]
impl StatusQuery {
    /// The indexer's current progress (top of the `subdex_block` table).
    async fn indexer_status(&self, ctx: &Context<'_>) -> async_graphql::Result<IndexerStatus> {
        let pool = ctx.data::<PgPool>()?;
        load_status(pool).await.map_err(Into::into)
    }
}

/// Load the status snapshot from the `subdex_block` table.
pub(crate) async fn load_status(pool: &PgPool) -> Result<IndexerStatus, sqlx::Error> {
    // Highest block row (the cursor) + total retained count, in one round-trip.
    let row = sqlx::query(
        "SELECT \
            (SELECT height FROM subdex_block ORDER BY height DESC LIMIT 1) AS height, \
            (SELECT hash FROM subdex_block ORDER BY height DESC LIMIT 1) AS hash, \
            (SELECT spec_version FROM subdex_block ORDER BY height DESC LIMIT 1) AS spec_version, \
            (SELECT timestamp FROM subdex_block ORDER BY height DESC LIMIT 1) AS block_timestamp, \
            (SELECT count(*) FROM subdex_block) AS indexed_blocks",
    )
    .fetch_one(pool)
    .await?;

    Ok(IndexerStatus {
        height: row.try_get("height")?,
        hash: row.try_get("hash")?,
        spec_version: row.try_get("spec_version")?,
        block_timestamp: row.try_get("block_timestamp")?,
        indexed_blocks: row.try_get("indexed_blocks")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Schema};

    /// The schema built from StatusQuery exposes the expected query + fields.
    /// Pure schema introspection — no database needed.
    #[test]
    fn schema_exposes_indexer_status() {
        let schema = Schema::build(StatusQuery, EmptyMutation, EmptySubscription).finish();
        let sdl = schema.sdl();
        assert!(sdl.contains("indexerStatus"), "query field present:\n{sdl}");
        assert!(sdl.contains("type IndexerStatus"));
        for field in [
            "height",
            "hash",
            "specVersion",
            "blockTimestamp",
            "indexedBlocks",
        ] {
            assert!(sdl.contains(field), "missing field {field} in SDL:\n{sdl}");
        }
    }
}
