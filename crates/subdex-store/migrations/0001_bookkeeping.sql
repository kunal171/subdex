-- subdex framework bookkeeping schema.
--
-- Everything here is namespaced with the `subdex_` prefix so it never collides
-- with the entity tables a framework user creates in their own handlers.

-- One row per indexed block, recording its hash. This is BOTH the progress
-- cursor (the row with the MAX height is "where we are") and the reorg-detection
-- record (we compare an incoming block's parent_hash against the stored hash for
-- its parent height). On a reorg we delete rows strictly above the fork height.
--
-- For unbounded chains this table would grow without limit; the processor is
-- responsible for pruning rows below the finalized head it no longer needs for
-- reorg checks (added when the processor lands). The schema keeps the full
-- (height, hash) so rollback can always find the fork point within retained rows.
CREATE TABLE IF NOT EXISTS subdex_block (
    height     BIGINT      PRIMARY KEY,
    hash       TEXT        NOT NULL,
    parent_hash TEXT       NOT NULL,
    -- Unix ms timestamp of the block, if known (from Timestamp.set).
    timestamp  BIGINT,
    -- Runtime spec version the block was decoded under (useful for debugging
    -- upgrade boundaries).
    spec_version BIGINT    NOT NULL,
    -- When this row was indexed (server clock), for operational visibility.
    indexed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Fast "is this height already indexed / what's its hash" lookups by hash, used
-- when validating chains across reorgs.
CREATE INDEX IF NOT EXISTS subdex_block_hash_idx ON subdex_block (hash);
