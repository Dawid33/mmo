-- R4 task 6: successions. Keep column-for-column in step with ../sqlite/0013_successions.sql.
-- One row per host declared dead (kind 0, a crash) or gone after a clean leave (kind 1)
-- whose super-regions were all moved to one heir, written only by the coordinator's fenced
-- declaration (src/succession.rs). Open while settled_cut IS NULL AND abandoned = 0: the
-- next committed world cut settles it (settled_cut = that cut, in its decision's
-- transaction); a world rollback abandons it (the land stays with the heir).
-- base_cut: the last committed cut at the declaration (0: none). survivors: the ordinals,
-- comma-separated, of every other live dirty host at the declaration (they report their
-- journals to the heir). moved: the super-regions moved, `sx:sz:epoch` comma-separated,
-- at their new epochs. Both are what a restarted coordinator re-sends.
CREATE TABLE successions (
    id BIGINT NOT NULL PRIMARY KEY,
    predecessor BIGINT NOT NULL,
    predecessor_boot BIGINT NOT NULL,
    heir BIGINT NOT NULL,
    kind BIGINT NOT NULL,
    base_cut BIGINT NOT NULL,
    settled_cut BIGINT NULL,
    abandoned BIGINT NOT NULL DEFAULT 0,
    declared_utc TEXT NOT NULL,
    survivors TEXT NOT NULL,
    moved TEXT NOT NULL
);
