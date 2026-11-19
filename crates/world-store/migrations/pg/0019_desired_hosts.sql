-- Elastic 4 task 2: how many hosts the world wants, decided by the coordinator. Keep
-- column-for-column in step with ../sqlite/0019_desired_hosts.sql. Worlds are re-forged, never
-- migrated.
-- desired_hosts: one row per host the coordinator wants to exist, written ONLY by its store
-- decisions AddHost / DrainHost / RemoveHost (inside its transition, under the coordinator row;
-- world-store/src/fleet.rs). name: h<n>, n from world.next_desired_host, never issued twice; the
-- reconciler names the host's Deployment after it. slot: the lowest one free when it was added
-- (reused after a RemoveHost; at most the fleet's max rows). state: 0 wanted, 1 draining.
-- created_ms: the database clock (ms since the epoch, the lease's clock) at its AddHost.
CREATE TABLE desired_hosts (
    name       TEXT   PRIMARY KEY,
    slot       BIGINT NOT NULL UNIQUE,
    state      BIGINT NOT NULL,
    created_ms BIGINT NOT NULL
);
-- next_desired_host: the number of the next desired host's name (h<n>).
ALTER TABLE world ADD COLUMN next_desired_host BIGINT NOT NULL DEFAULT 1;
