-- R2b: kicks are durable until the world host acknowledges them (resent on every link up).
CREATE TABLE pending_kicks (
    world_id      TEXT NOT NULL,
    player_token  TEXT NOT NULL,
    session_epoch BIGINT NOT NULL,
    reason        TEXT NOT NULL,
    created_at    BIGINT NOT NULL,
    PRIMARY KEY (world_id, player_token)
);
