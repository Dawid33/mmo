-- Coordinator HTTP API task 1: the host mailbox and the login answers. Keep column-for-column in
-- step with ../pg/0020_mailbox.sql. Worlds are re-forged, never migrated.
-- outbox (0017) becomes every host's mailbox: a coordinator transition inserts the messages it
-- decided for a host (ONLY inside a transition, after its compare-and-set locked the coordinator
-- row: seq is commit order), the host reads its rows past its cursor and deletes those at or
-- below it (world-store/src/mailbox.rs). at_ms: the database clock (ms since the epoch, the
-- lease's clock) at the insert — how long the oldest unread row has waited.
-- SQLite (one process, role `all`) never writes it.
ALTER TABLE outbox ADD COLUMN at_ms INTEGER NOT NULL DEFAULT 0;
-- login_outbox: the coordinator's answers to the login service, inserted only inside a
-- transition (seq is commit order); read past a cursor, never deleted by a read — a transition
-- prunes the rows older than its cut-off (at_ms, the database clock at the insert).
-- SQLite never writes it.
CREATE TABLE login_outbox (
    seq   INTEGER PRIMARY KEY AUTOINCREMENT,
    at_ms INTEGER NOT NULL,
    msg   BLOB    NOT NULL
);
