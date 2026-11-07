-- World store schema v1 (Postgres). Keep column-for-column in step with
-- ../sqlite/0001_schema_v1.sql: only the shared subset (INTEGER/BIGINT, TEXT,
-- BLOB/BYTEA). u64 values are stored as i64 by bit cast. No world_id columns.
CREATE TABLE world (
    id BIGINT NOT NULL PRIMARY KEY CHECK (id = 1),
    format_version BIGINT NOT NULL,
    region_blocks BIGINT NOT NULL,
    chunk_blocks BIGINT NOT NULL,
    content_crc BIGINT NOT NULL,
    world_tick BIGINT NOT NULL,
    super_region_regions BIGINT NOT NULL,
    created_utc TEXT NOT NULL
);
CREATE TABLE forged_artifact (
    id BIGINT NOT NULL PRIMARY KEY,
    bytes BYTEA NOT NULL
);
CREATE TABLE regions (
    rx BIGINT NOT NULL,
    rz BIGINT NOT NULL,
    saved_tick BIGINT NOT NULL,
    terrain_version BIGINT NOT NULL,
    meta BYTEA NOT NULL,
    ground BYTEA NOT NULL,
    PRIMARY KEY (rx, rz)
);
CREATE TABLE chunks (
    rx BIGINT NOT NULL,
    rz BIGINT NOT NULL,
    cx BIGINT NOT NULL,
    cy BIGINT NOT NULL,
    cz BIGINT NOT NULL,
    data BYTEA NOT NULL,
    PRIMARY KEY (rx, rz, cx, cy, cz)
);
CREATE TABLE entities (
    birth_rx BIGINT NOT NULL,
    birth_rz BIGINT NOT NULL,
    serial BIGINT NOT NULL,
    rx BIGINT NOT NULL,
    rz BIGINT NOT NULL,
    saved_tick BIGINT NOT NULL,
    kind BIGINT NOT NULL,
    bundle BYTEA NOT NULL,
    PRIMARY KEY (birth_rx, birth_rz, serial)
);
CREATE INDEX entities_region ON entities (rx, rz);
CREATE TABLE players (
    player_id BIGINT NOT NULL PRIMARY KEY,
    token TEXT NOT NULL UNIQUE,
    record BYTEA,
    saved_tick BIGINT
);
CREATE TABLE history (
    id BIGINT NOT NULL PRIMARY KEY,
    bytes BYTEA NOT NULL
);
CREATE TABLE in_flight (
    seq BIGINT NOT NULL PRIMARY KEY,
    kind BIGINT NOT NULL, -- 0 transfer, 1 intent
    target_rx BIGINT NOT NULL,
    target_rz BIGINT NOT NULL,
    stamp BIGINT NOT NULL,
    payload BYTEA NOT NULL
);
-- R2: super_regions(owner, epoch) goes here.
