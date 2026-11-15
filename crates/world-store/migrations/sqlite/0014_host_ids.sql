-- Elastic 1 task 2: a host's id is issued at registration and never reused.
-- Keep statement-for-statement in step with ../pg/0014_host_ids.sql.
-- hosts.id (was ordinal): the host's id, issued by register_host from world.next_host_id
-- under the world row's lock and never issued twice (a pruned row's id included). An
-- existing world's rows keep their numbers (0, 1, ...) as ids: they are old hosts that
-- never register again; the counter starts above every one of them. name: free text that
-- labels the process for people (VOXEL_HOST_NAME), never a key. drain_at_ms: the database
-- clock (ms since the epoch) at which a draining host leaves; NULL = not draining.
-- hosts.reg is no longer read (a host id is its registration number); in_flight.host,
-- super_regions.owner and successions hold the same numbers and need no change.
ALTER TABLE hosts RENAME COLUMN ordinal TO id;
ALTER TABLE hosts ADD COLUMN name TEXT NOT NULL DEFAULT '';
ALTER TABLE hosts ADD COLUMN drain_at_ms BIGINT;
ALTER TABLE world ADD COLUMN next_host_id BIGINT NOT NULL DEFAULT 1;
UPDATE world SET next_host_id = (SELECT COALESCE(MAX(id), 0) + 1 FROM hosts);
