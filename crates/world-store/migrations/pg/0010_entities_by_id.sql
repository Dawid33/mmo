-- R3 task 12b, fix round 1: find an entity's rows by its id. Keep column-for-column in
-- step with ../sqlite/0010_entities_by_id.sql.
-- 0009 keyed entities per region, so nothing finds the other rows of one entity any more.
-- A cut refuses to save an entity that still has a row in a region the cut cannot be
-- deleting (src/rows.rs, check_stale_rows), and a committed world cut reports an entity
-- stored in two regions (src/two_phase.rs); both look the other row up by id here.
-- Not unique: two hosts' parts of one cut may legitimately hold the row being deleted in
-- one region and the row being inserted in another until the decision.
CREATE INDEX entities_id ON entities (birth_rx, birth_rz, serial);
