-- R3 task 12: two-phase save cuts. Keep column-for-column in step with ../sqlite/0008_cut_decisions.sql.
-- One row per decided cut: the coordinator's decision (committed = 1) or an abort
-- (committed = 0, by the coordinator or by a host's "abort unless decided"). The first
-- writer wins on the primary key; a decision is never changed. Each host's part of the
-- cut is a Postgres prepared transaction (gid `mmo-cut-<world tag>-<cut>-<host>`) that is
-- committed or rolled back as this row says. SQLite never prepares a cut.
CREATE TABLE cut_decisions (
    cut_id BIGINT NOT NULL PRIMARY KEY,
    tick BIGINT NOT NULL,
    committed BIGINT NOT NULL,
    decided_utc TEXT NOT NULL
);
