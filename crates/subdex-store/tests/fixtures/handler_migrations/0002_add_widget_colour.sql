-- A later schema evolution: add a column. Applying this on an existing DB is
-- exactly the case the ad-hoc CREATE TABLE IF NOT EXISTS pattern couldn't handle.
ALTER TABLE widgets ADD COLUMN colour TEXT;
