Multi-region world follow-ups (milestone landed; spec 2026-07-05-multi-region-world):

- Entity/player handoff between regions (deferred by design; spec 2026-07-05-multi-region-world).
- Client per-region clock tracking (single shared tick_rate today; 9 SyncClock streams fight over it).
- Durable region persistence (parking lot is in-memory only).
- Cross-region event relay groundwork (deferred with handoff).
- Stable client identity across connections: reconnects get a fresh ClientId, so the manager's no-duplicate-player reconnect path only fires for same-id sessions; the old player entity lingers in its home region.
