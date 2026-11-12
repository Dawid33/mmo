-- R3 task 3: the host registry and the world generation.
-- Keep column-for-column in step with ../pg/0007_hosts.sql.
-- world.generation: bumped when a host reboots after a crash (its row was not clean and
-- still at the world's generation); every host that sees a higher one restarts from the
-- same save. hosts: one row per host ordinal; `clean` (0/1) is set by a save cut and
-- cleared by every boot; the address columns are the host's published endpoints.
ALTER TABLE world ADD COLUMN generation BIGINT NOT NULL DEFAULT 0;
CREATE TABLE hosts (
    ordinal BIGINT NOT NULL PRIMARY KEY,
    boot BIGINT NOT NULL,
    generation BIGINT NOT NULL,
    clean BIGINT NOT NULL,
    peer_addr TEXT NULL,
    peer_cert TEXT NULL,
    quic TEXT NULL,
    webtransport TEXT NULL,
    wt_cert_hash TEXT NULL,
    updated_utc TEXT NOT NULL
);
