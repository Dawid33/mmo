-- R3 task 12b: entity rows are keyed per region. Keep column-for-column in step with
-- ../sqlite/0009_entities_per_region.sql.
-- Each host's part of a world cut rewrites only the rows of its own regions (delete by
-- region, then insert). With one key per entity for the whole world, an entity that
-- crossed from one host's region into another's between two cuts was deleted by one
-- part and inserted by the other: two prepared transactions on one row, and the cut
-- (and every later one) could not commit. The key is now (region, entity): two parts
-- never touch one row. An entity in two regions of ONE part is still refused, in code
-- before the cut writes (src/rows.rs); across parts the cut-ownership protocol keeps an
-- entity in exactly one part. Existing rows carry over unchanged (each already names its
-- region). entities_region is dropped: the new key's prefix (rx, rz) serves those reads.
ALTER TABLE entities DROP CONSTRAINT entities_pkey;
ALTER TABLE entities ADD PRIMARY KEY (rx, rz, birth_rx, birth_rz, serial);
DROP INDEX entities_region;
