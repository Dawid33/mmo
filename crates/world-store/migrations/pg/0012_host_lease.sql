-- R4 task 1: the lease on a host's row. Keep column-for-column in step with ../sqlite/0012_host_lease.sql.
-- state: 0 live, 1 dead (a takeover marks it). seen_ms: the database clock (ms since the
-- epoch) at the host's last registration or heartbeat. reg: the registration number, one
-- more than every other row's at the host's last registration (higher = registered later).
ALTER TABLE hosts ADD COLUMN state BIGINT NOT NULL DEFAULT 0;
ALTER TABLE hosts ADD COLUMN seen_ms BIGINT NOT NULL DEFAULT 0;
ALTER TABLE hosts ADD COLUMN reg BIGINT NOT NULL DEFAULT 0;
