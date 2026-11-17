-- Elastic 2b task 4: the coordinator's replicas. Keep column-for-column in step with
-- ../pg/0017_coordinator_replicas.sql. Worlds are re-forged, never migrated.
-- coordinator_replicas: one row per coordinator process start (a replica), its id issued from
-- coordinator.next_replica and never reused. seen_ms: the database clock (ms) of its last beat;
-- state: game::coordinator::REPLICA_LIVE (0) or REPLICA_DEAD (1); addr: where it listens for
-- hosts (a label); registered_utc: the database clock (ms) at its registration.
-- SQLite (one process, role `all`): its one replica registers; nothing beats or reads the rows.
CREATE TABLE coordinator_replicas (
    replica INTEGER NOT NULL PRIMARY KEY,
    seen_ms INTEGER NOT NULL,
    state INTEGER NOT NULL,
    addr TEXT NOT NULL,
    registered_utc INTEGER NOT NULL
);
-- outbox: messages a transition decided for hosts linked to another replica (written in the
-- transition's own transaction; Elastic 2b task 5 fills it). seq orders them by commit.
-- SQLite never writes it.
CREATE TABLE outbox (
    seq INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    version INTEGER NOT NULL,
    host INTEGER NOT NULL,
    boot INTEGER NOT NULL,
    msg BLOB NOT NULL
);
CREATE INDEX outbox_host ON outbox (host, seq);
-- next_replica: the next replica id to issue. history_version: the row version whose
-- transition last wrote `history` (a refresh reads the history only when it is newer).
ALTER TABLE coordinator ADD COLUMN next_replica INTEGER NOT NULL DEFAULT 1;
ALTER TABLE coordinator ADD COLUMN history_version INTEGER NOT NULL DEFAULT 0;
