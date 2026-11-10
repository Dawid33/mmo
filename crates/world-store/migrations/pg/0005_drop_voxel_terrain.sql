-- Voxel terrain is gone (stud world plan, P10b). Keep in step with
-- ../sqlite/0005_drop_voxel_terrain.sql.
-- chunks: the voxel chunks of a region. regions.ground: its voxel ground recipes
-- (an empty list in every stud region's row). Stud terrain lives in stud_chunks and
-- regions.stud_ground (0004) and is not touched.
-- This never runs over a world saved on voxel terrain: such a store is refused at
-- open, before any migration is applied (src/open.rs, refuse_voxel_world), and stays
-- as it was. world.terrain stays: 0 is still the mark of such a world.
DROP TABLE chunks;
ALTER TABLE regions DROP COLUMN ground;
