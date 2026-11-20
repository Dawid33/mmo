-- The deployment region a world belongs to. One login service can list worlds
-- from several regions while joining each through its own coordinator link.
ALTER TABLE worlds ADD COLUMN region TEXT NOT NULL DEFAULT 'eu-central-1';
