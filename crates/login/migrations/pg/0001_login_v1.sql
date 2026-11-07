CREATE TABLE accounts (
    account_id        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    email             TEXT NOT NULL UNIQUE,
    password_hash     TEXT NOT NULL,
    email_verified_at BIGINT,
    created_at        BIGINT NOT NULL
);

CREATE TABLE refresh_tokens (
    token_hash BYTEA NOT NULL PRIMARY KEY,
    account_id BIGINT NOT NULL,
    family     BYTEA NOT NULL,
    expires_at BIGINT NOT NULL,
    revoked_at BIGINT
);
CREATE INDEX refresh_tokens_account ON refresh_tokens (account_id);
CREATE INDEX refresh_tokens_family ON refresh_tokens (family);

CREATE TABLE email_tokens (
    token_hash BYTEA NOT NULL PRIMARY KEY,
    account_id BIGINT NOT NULL,
    kind       BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    used_at    BIGINT
);

CREATE TABLE worlds (
    world_id     TEXT NOT NULL PRIMARY KEY,
    name         TEXT NOT NULL,
    status       BIGINT NOT NULL,
    quic         TEXT,
    webtransport TEXT,
    cert_hash    TEXT,
    created_at   BIGINT NOT NULL
);

CREATE TABLE world_players (
    account_id   BIGINT NOT NULL,
    world_id     TEXT NOT NULL,
    player_token TEXT NOT NULL UNIQUE,
    created_at   BIGINT NOT NULL,
    PRIMARY KEY (account_id, world_id)
);

CREATE TABLE sessions (
    account_id    BIGINT NOT NULL,
    world_id      TEXT NOT NULL,
    session_epoch BIGINT NOT NULL,
    started_at    BIGINT NOT NULL,
    PRIMARY KEY (account_id, world_id)
);
