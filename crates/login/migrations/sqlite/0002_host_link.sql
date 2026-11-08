-- R2b: the world host's service link (login pushes SpawnPlayer / Kick), and when a session ended.
ALTER TABLE worlds ADD COLUMN host_link TEXT NULL;
ALTER TABLE worlds ADD COLUMN host_link_cert TEXT NULL;
ALTER TABLE sessions ADD COLUMN ended_at INTEGER NULL;
