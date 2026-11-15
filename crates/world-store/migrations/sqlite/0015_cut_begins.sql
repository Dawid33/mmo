-- Elastic 1 task 4: a world cut's begin, recorded by the coordinator. Keep column-for-column in step with ../pg/0015_cut_begins.sql.
-- hosts_below: world.next_host_id read when the begin was recorded — every host id below it
-- was issued before; a host whose id is at or above it registered after the cut began, owned
-- nothing at its tick, and may be absent from the cut's decision (two_phase::decide_cut).
-- A cut with no row here needs every live dirty host listed, as before. SQLite never runs a
-- world cut: the table stays empty there.
CREATE TABLE cut_begins (
    cut_id BIGINT NOT NULL PRIMARY KEY,
    hosts_below BIGINT NOT NULL,
    begun_utc TEXT NOT NULL
);
