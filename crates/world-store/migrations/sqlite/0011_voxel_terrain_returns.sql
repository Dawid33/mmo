-- Voxel terrain returns (voxel terrain return plan, Task 2). Keep column-for-column in
-- step with ../pg/0011_voxel_terrain_returns.sql.
-- chunks and regions.ground come back exactly as 0001_schema_v1 defined them; the stud
-- table and column (0004) go. regions.ground's default is an encoded empty ground list
-- (format byte 1 + zstd of an empty bincode Vec), so a row that predates the column
-- satisfies NOT NULL and decodes.
-- This never runs over a store with saved regions: a store that has not applied 0011 and
-- holds a region (a stud world, or a voxel world from before the stud switch, whose chunks
-- 0005 would drop) is refused at open before any migration (src/open.rs,
-- refuse_unreturned_world) and stays as it was. world.terrain stays: a world with no saved
-- region holds no terrain, so it is tied to voxels here; from now on studs (1) is only the
-- mark that gets a world refused.
CREATE TABLE chunks (
    rx INTEGER NOT NULL,
    rz INTEGER NOT NULL,
    cx INTEGER NOT NULL,
    cy INTEGER NOT NULL,
    cz INTEGER NOT NULL,
    data BLOB NOT NULL,
    PRIMARY KEY (rx, rz, cx, cy, cz)
);
ALTER TABLE regions ADD COLUMN ground BLOB NOT NULL DEFAULT x'0128b52ffd20084100000000000000000000';
DROP TABLE stud_chunks;
ALTER TABLE regions DROP COLUMN stud_ground;
UPDATE world SET terrain = 0 WHERE NOT EXISTS (SELECT 1 FROM regions);
