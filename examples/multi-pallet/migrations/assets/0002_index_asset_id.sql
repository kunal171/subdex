-- A later evolution: index the column we filter/aggregate on. This is the case
-- ad-hoc `CREATE TABLE IF NOT EXISTS` in init() can't express — an existing
-- deployment needs this applied exactly once, a fresh one gets it from scratch.
CREATE INDEX IF NOT EXISTS asset_lifecycle_asset_id_idx ON asset_lifecycle (asset_id);
