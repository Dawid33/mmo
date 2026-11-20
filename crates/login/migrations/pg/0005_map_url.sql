-- The world map (plan 2026-11-20-block-level-world-map, Task 11): the base URL of the world's map tile
-- route (`<url>/v1/map/tiles/{level}/{x}/{z}`), sent to clients in the world list and the join ticket.
-- NULL = the world serves no map (the browser uses `{origin}/map/<world id>` regardless).
ALTER TABLE worlds ADD COLUMN map_url TEXT NULL;
