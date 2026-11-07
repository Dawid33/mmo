CREATE TABLE accounts (
    account_id        INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    email             TEXT NOT NULL UNIQUE,
    password_hash     TEXT NOT NULL,
    email_verified_at INTEGER,
    created_at        INTEGER NOT NULL
);

CREATE TABLE refresh_tokens (
    token_hash BLOB NOT NULL PRIMARY KEY,
    account_id INTEGER NOT NULL,
    family     BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER
);
CREATE INDEX refresh_tokens_account ON refresh_tokens (account_id);
CREATE INDEX refresh_tokens_family ON refresh_tokens (family);

CREATE TABLE email_tokens (
    token_hash BLOB NOT NULL PRIMARY KEY,
    account_id INTEGER NOT NULL,
    kind       INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER
);

CREATE TABLE worlds (
    world_id     TEXT NOT NULL PRIMARY KEY,
    name         TEXT NOT NULL,
    status       INTEGER NOT NULL,
    quic         TEXT,
    webtransport TEXT,
    cert_hash    TEXT,
    created_at   INTEGER NOT NULL
);

CREATE TABLE world_players (
    account_id   INTEGER NOT NULL,
    world_id     TEXT NOT NULL,
    player_token TEXT NOT NULL UNIQUE,
    created_at   INTEGER NOT NULL,
    PRIMARY KEY (account_id, world_id)
);

CREATE TABLE sessions (
    account_id    INTEGER NOT NULL,
    world_id      TEXT NOT NULL,
    session_epoch INTEGER NOT NULL,
    started_at    INTEGER NOT NULL,
    PRIMARY KEY (account_id, world_id)
);
