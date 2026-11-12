-- R3 task 1: one wall-anchored world clock for every host of a world.
-- Keep column-for-column in step with ../sqlite/0006_world_clock.sql.
-- clock_anchor_*: a wall instant (unix ms) and the world tick due then; NULL until the
-- first host or coordinator process of a split world writes it (first writer wins).
-- `worldctl clock-reset` clears it.
ALTER TABLE world ADD COLUMN clock_anchor_unix_ms BIGINT NULL;
ALTER TABLE world ADD COLUMN clock_anchor_tick BIGINT NULL;
