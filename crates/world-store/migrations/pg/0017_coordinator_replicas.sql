-- Elastic 2b task 4: the coordinator's replicas. Keep column-for-column in step with
-- ../sqlite/0017_coordinator_replicas.sql. Worlds are re-forged, never migrated.
-- coordinator_replicas: one row per coordinator process start (a replica), its id issued from
-- coordinator.next_replica and never reused. seen_ms: the database clock (ms) of its last beat;
-- state: game::coordinator::REPLICA_LIVE (0) or REPLICA_DEAD (1); addr: where it listens for
-- hosts (a label); registered_utc: the database clock (ms) at its registration.
CREATE TABLE coordinator_replicas (
    replica BIGINT NOT NULL PRIMARY KEY,
    seen_ms BIGINT NOT NULL,
    state BIGINT NOT NULL,
    addr TEXT NOT NULL,
    registered_utc BIGINT NOT NULL
);
-- outbox: messages a transition decided for hosts linked to another replica (written in the
-- transition's own transaction; Elastic 2b task 5 fills it). seq orders them by commit.
CREATE TABLE outbox (
    seq BIGSERIAL NOT NULL PRIMARY KEY,
    version BIGINT NOT NULL,
    host BIGINT NOT NULL,
    boot BIGINT NOT NULL,
    msg BYTEA NOT NULL
);
CREATE INDEX outbox_host ON outbox (host, seq);
-- next_replica: the next replica id to issue. history_version: the row version whose
-- transition last wrote `history` (a refresh reads the history only when it is newer).
ALTER TABLE coordinator ADD COLUMN next_replica BIGINT NOT NULL DEFAULT 1;
ALTER TABLE coordinator ADD COLUMN history_version BIGINT NOT NULL DEFAULT 0;
