-- Elastic 3 task 1: a host's load rides its heartbeat. Keep column-for-column in step with
-- ../sqlite/0018_host_load.sql. Worlds are re-forged, never migrated.
-- load_players: sessions the host holds; load_regions: regions it runs; load_tick_permille: its
-- worst tick compute time since the previous beat, over the tick period, x1000 (capped at 10000).
-- Written by the beat's own UPDATE (heartbeat_sql); nothing reads it for a decision yet.
ALTER TABLE hosts ADD COLUMN load_players BIGINT NOT NULL DEFAULT 0;
ALTER TABLE hosts ADD COLUMN load_regions BIGINT NOT NULL DEFAULT 0;
ALTER TABLE hosts ADD COLUMN load_tick_permille BIGINT NOT NULL DEFAULT 0;
