-- R3 task 12b, fix round 1: find an entity's rows by its id. Keep column-for-column in
-- step with ../pg/0010_entities_by_id.sql (see there for why). Not unique.
CREATE INDEX entities_id ON entities (birth_rx, birth_rz, serial);
