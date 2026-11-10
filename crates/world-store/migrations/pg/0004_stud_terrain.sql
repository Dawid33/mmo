-- Stud terrain (stud world plan, P5). Keep column-for-column in step with
-- ../sqlite/0004_stud_terrain.sql.
-- stud_chunks: the stud chunks of a region that its ground recipe does not give back
-- (edited chunks whole, materialised pristine chunks under the surface as markers).
-- regions.stud_ground: the region's stud ground recipes (NULL: a voxel region).
-- world.terrain: the terrain kind the world's regions are saved in (0 voxels,
-- 1 studs; NULL until a server first serves it). A world that already has saved
-- regions is a voxel world.
CREATE TABLE stud_chunks (
    rx BIGINT NOT NULL,
    rz BIGINT NOT NULL,
    cx BIGINT NOT NULL,
    cy BIGINT NOT NULL,
    cz BIGINT NOT NULL,
    data BYTEA NOT NULL,
    PRIMARY KEY (rx, rz, cx, cy, cz)
);
ALTER TABLE regions ADD COLUMN stud_ground BYTEA;
ALTER TABLE world ADD COLUMN terrain BIGINT;
UPDATE world SET terrain = 0 WHERE EXISTS (SELECT 1 FROM regions);
