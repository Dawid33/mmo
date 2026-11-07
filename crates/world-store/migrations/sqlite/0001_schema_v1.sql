-- World store schema v1 (SQLite). Keep column-for-column in step with
-- ../pg/0001_schema_v1.sql: only the shared subset (INTEGER/BIGINT, TEXT,
-- BLOB/BYTEA). u64 values are stored as i64 by bit cast. No world_id columns.
CREATE TABLE world (
    id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
    format_version INTEGER NOT NULL,
    region_blocks INTEGER NOT NULL,
    chunk_blocks INTEGER NOT NULL,
    content_crc INTEGER NOT NULL,
    world_tick INTEGER NOT NULL,
    super_region_regions INTEGER NOT NULL,
    created_utc TEXT NOT NULL
);
CREATE TABLE forged_artifact (
    id INTEGER NOT NULL PRIMARY KEY,
    bytes BLOB NOT NULL
);
CREATE TABLE regions (
    rx INTEGER NOT NULL,
    rz INTEGER NOT NULL,
    saved_tick INTEGER NOT NULL,
    terrain_version INTEGER NOT NULL,
    meta BLOB NOT NULL,
    ground BLOB NOT NULL,
    PRIMARY KEY (rx, rz)
);
CREATE TABLE chunks (
    rx INTEGER NOT NULL,
    rz INTEGER NOT NULL,
    cx INTEGER NOT NULL,
    cy INTEGER NOT NULL,
    cz INTEGER NOT NULL,
    data BLOB NOT NULL,
    PRIMARY KEY (rx, rz, cx, cy, cz)
);
CREATE TABLE entities (
    birth_rx INTEGER NOT NULL,
    birth_rz INTEGER NOT NULL,
    serial INTEGER NOT NULL,
    rx INTEGER NOT NULL,
    rz INTEGER NOT NULL,
    saved_tick INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    bundle BLOB NOT NULL,
    PRIMARY KEY (birth_rx, birth_rz, serial)
);
CREATE INDEX entities_region ON entities (rx, rz);
CREATE TABLE players (
    player_id INTEGER NOT NULL PRIMARY KEY,
    token TEXT NOT NULL UNIQUE,
    record BLOB,
    saved_tick INTEGER
);
CREATE TABLE history (
    id INTEGER NOT NULL PRIMARY KEY,
    bytes BLOB NOT NULL
);
CREATE TABLE in_flight (
    seq INTEGER NOT NULL PRIMARY KEY,
    kind INTEGER NOT NULL, -- 0 transfer, 1 intent
    target_rx INTEGER NOT NULL,
    target_rz INTEGER NOT NULL,
    stamp INTEGER NOT NULL,
    payload BLOB NOT NULL
);
-- R2: super_regions(owner, epoch) goes here.
