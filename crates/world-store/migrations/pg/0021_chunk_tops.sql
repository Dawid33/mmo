-- The world map (plan 2026-11-20-block-level-world-map, Task 2): a chunk's own top per block column
-- (`game::map_tile::ChunkTop`, `FORMAT_CHUNK_TOP`), written by the same INSERT … ON CONFLICT that writes
-- `data`, so it always matches the stored chunk and moves with it. NULL (a row saved before this
-- migration) is read as "derive it from data", never as empty. Keep column-for-column in step with
-- ../sqlite/0021_chunk_tops.sql. Worlds are re-forged, never migrated.
ALTER TABLE chunks ADD COLUMN top BYTEA;
