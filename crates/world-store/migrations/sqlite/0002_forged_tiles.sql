-- Forged world as tiles (forged-world-in-db plan, Task 4). Keep column-for-column
-- in step with ../pg/0002_forged_tiles.sql: only the shared subset.
-- The original forge file stays as the archive (re-tiling, export); hosts read
-- only the meta, the tiles and the stored world map.
ALTER TABLE forged_artifact RENAME TO forged_archive;
CREATE TABLE forged_meta (
    id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
    format_version INTEGER NOT NULL,
    bytes BLOB NOT NULL
);
CREATE TABLE forged_tiles (
    tx INTEGER NOT NULL,
    tz INTEGER NOT NULL,
    bytes BLOB NOT NULL,
    PRIMARY KEY (tx, tz)
);
CREATE TABLE forged_world_map (
    id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
    bytes BLOB NOT NULL
);
