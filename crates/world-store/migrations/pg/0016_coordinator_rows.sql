-- Elastic 2a task 5: the coordinator's whole state is one durable projection, written with a
-- compare-and-set. Keep column-for-column in step with ../sqlite/0016_coordinator_rows.sql.
-- version: bumped by every write; a write names the version it read (`version = $v`) besides
-- the claim's epoch — 0 rows with the epoch matching is a Conflict (a second writer).
-- core: the coordinator core's projection (bincode + lz4, `game::coordinator` format byte).
-- history: the history (`game::history::persist`), written only when it changed.
-- `state` (the old single blob) is dropped: worlds are re-forged, never migrated (no data path).
ALTER TABLE coordinator ADD COLUMN version BIGINT NOT NULL DEFAULT 0;
ALTER TABLE coordinator ADD COLUMN core BYTEA;
ALTER TABLE coordinator ADD COLUMN history BYTEA;
ALTER TABLE coordinator DROP COLUMN state;
