-- Forged world as tiles (forged-world-in-db plan, Task 4). Keep column-for-column
-- in step with ../sqlite/0002_forged_tiles.sql: only the shared subset.
-- The original forge file stays as the archive (re-tiling, export); hosts read
-- only the meta, the tiles and the stored world map.
ALTER TABLE forged_artifact RENAME TO forged_archive;
CREATE TABLE forged_meta (
    id BIGINT NOT NULL PRIMARY KEY CHECK (id = 1),
    format_version BIGINT NOT NULL,
    bytes BYTEA NOT NULL
);
CREATE TABLE forged_tiles (
    tx BIGINT NOT NULL,
    tz BIGINT NOT NULL,
    bytes BYTEA NOT NULL,
    PRIMARY KEY (tx, tz)
);
CREATE TABLE forged_world_map (
    id BIGINT NOT NULL PRIMARY KEY CHECK (id = 1),
    bytes BYTEA NOT NULL
);
