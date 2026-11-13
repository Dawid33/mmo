-- R3 task 12b: entity rows are keyed per region. Keep column-for-column in step with
-- ../pg/0009_entities_per_region.sql (see there for why).
-- SQLite cannot change a primary key in place: the table is rebuilt with the new key and
-- every row copied unchanged; entities_region goes with the old table (the new key's
-- prefix (rx, rz) serves those reads).
CREATE TABLE entities_by_region (
    birth_rx INTEGER NOT NULL,
    birth_rz INTEGER NOT NULL,
    serial INTEGER NOT NULL,
    rx INTEGER NOT NULL,
    rz INTEGER NOT NULL,
    saved_tick INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    bundle BLOB NOT NULL,
    PRIMARY KEY (rx, rz, birth_rx, birth_rz, serial)
);
INSERT INTO entities_by_region (birth_rx, birth_rz, serial, rx, rz, saved_tick, kind, bundle)
    SELECT birth_rx, birth_rz, serial, rx, rz, saved_tick, kind, bundle FROM entities;
DROP TABLE entities;
ALTER TABLE entities_by_region RENAME TO entities;
