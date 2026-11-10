-- Stud terrain (stud world plan, P5). Keep column-for-column in step with
-- ../pg/0004_stud_terrain.sql.
-- stud_chunks: the stud chunks of a region that its ground recipe does not give back
-- (edited chunks whole, materialised pristine chunks under the surface as markers).
-- regions.stud_ground: the region's stud ground recipes (NULL: a voxel region).
-- world.terrain: the terrain kind the world's regions are saved in (0 voxels,
-- 1 studs; NULL until a server first serves it). A world that already has saved
-- regions is a voxel world.
CREATE TABLE stud_chunks (
    rx INTEGER NOT NULL,
    rz INTEGER NOT NULL,
    cx INTEGER NOT NULL,
    cy INTEGER NOT NULL,
    cz INTEGER NOT NULL,
    data BLOB NOT NULL,
    PRIMARY KEY (rx, rz, cx, cy, cz)
);
ALTER TABLE regions ADD COLUMN stud_ground BLOB;
ALTER TABLE world ADD COLUMN terrain INTEGER;
UPDATE world SET terrain = 0 WHERE EXISTS (SELECT 1 FROM regions);
