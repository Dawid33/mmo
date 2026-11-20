-- Elastic 5 task 2: a host's load per super-region rides its heartbeat. Keep column-for-column
-- in step with ../sqlite/0022_host_land_load.sql. Worlds are re-forged, never migrated.
-- land_load: game::encode_packet of a Vec<game::SrLoad> (the most loaded first, at most
-- game::LAND_LOAD_MAX), written by the beat's own UPDATE (heartbeat_sql); NULL (a boot beat, an
-- empty list) reads as empty. Read by the coordinator's hosts read (a move's land).
ALTER TABLE hosts ADD COLUMN land_load BYTEA;
