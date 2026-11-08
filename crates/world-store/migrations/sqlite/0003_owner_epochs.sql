-- Fulfils 0001's "R2: super_regions(owner, epoch)" note (R2b role split, Task 1).
-- Keep column-for-column in step with ../pg/0003_owner_epochs.sql.
-- super_regions: which host process owns a super-region, and the epoch it claimed
-- it at (the fence: a lower epoch is refused). coordinator: the one coordinator's
-- epoch and saved state. in_flight rows belong to the host that cut them.
-- Player ids are assigned by the database (INTEGER PRIMARY KEY already does here).
CREATE TABLE super_regions (
    sx INTEGER NOT NULL,
    sz INTEGER NOT NULL,
    owner INTEGER NOT NULL,
    epoch INTEGER NOT NULL,
    PRIMARY KEY (sx, sz)
);
CREATE TABLE coordinator (
    id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
    epoch INTEGER NOT NULL,
    state BLOB,
    saved_utc TEXT
);
ALTER TABLE in_flight ADD COLUMN host INTEGER NOT NULL DEFAULT 0;
