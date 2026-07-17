-- AssetsHandler's entity table. Owned by the handler, versioned by the handler —
-- the framework only owns `subdex_block` (see PgStore::run_handler_migrations).
CREATE TABLE IF NOT EXISTS asset_lifecycle (
    id           BIGSERIAL PRIMARY KEY,
    block_height BIGINT NOT NULL,
    event_index  BIGINT NOT NULL,
    action       TEXT NOT NULL,
    asset_id     BIGINT,
    owner        TEXT,
    UNIQUE (block_height, event_index)
);
