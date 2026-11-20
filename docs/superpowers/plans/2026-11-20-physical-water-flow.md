# Physical moving water

## Status

This build implements the local-authority portion of Option B from the
physical-water investigation:

- authoritative `WATER_FULL = 4096` integer volume per one-metre cell;
- sparse O(1) activation and sparse absolute cell overrides, including zero;
- canonical two-phase flow with independently capped operation budgets,
  start-of-step donor volume, a small hot-edit reserve and round-robin chunks;
- rollback-emitted volume/activation deltas and client prediction;
- lazy recipe fallback: untouched cells are never copied into a live chunk;
- wake/sleep activity frontier, block-edit displacement, and refusal when a
  solid write has no destination capacity;
- near-water tile remeshing from predicted dynamic state;
- versioned `regions.water_state` persistence in world-cut snapshots;
- process metrics and a bounded periodic `[water]` line.

Dynamic LOD, source/sink reservoirs, cross-region face transfers, host peer
journalling, cut recovery of an in-flight debit, and succession ownership have
not been enabled. Region faces are therefore sealed in this phase. This is
intentional: an ordinary halo pressure copy or an unjournalled `Reliable`
message could create or lose volume across a disconnect/cut.

## Authority and update

`game::WaterRegion` is the portable field. An absent override reads the
immutable recipe; a present value is authoritative even when it is zero.
`GameData` persists activated chunks, absolute overrides, signed recipe volume
delta, the edit-seeded hot queue, fixed chunk masks, and round-robin cursors in
rollback state. Activation is one marker insertion and no recipe scan.

Each tick selects at most 512 donors: 64 edit-seeded hot cells and the rest
through fair chunk round-robin. Named caps independently bound 2,560 edge
probes, 1,024 transfers, 1,024 unique writes, 256 activations, 7,168 wake
visits and 1,024 delta cells. Vertical movement is planned before horizontal
equalisation, and each edge carries at most `WATER_MAX_FLOW`. A donor and
recipient are read from start-of-step volume; all accepted debits and credits
are committed together in canonical `WorldBlockPos` order. A refused request
is requeued rather than discarded, and flow-created wakes are always fair.

A solid block edit into water is refused unless the six open neighbour cells
can hold the displaced integer volume. The accepted path removes the occupied
cell and fills neighbours in canonical order above, below, west, east, north,
south. Breaking a cell activates its chunk and wakes the local one-cell front.
Tree growth does not overwrite water.

## Rollback, snapshots, and wire

Undoable cell/map writes own every change to activation markers, absolute
overrides, scheduler state, cursor position, and signed volume delta.
`GameDataUpdateKind::SetWater` is appended to the internal append-only update
enum so Do and Undo both reach the renderer. It contains chunk activation
booleans and `(WorldBlockPos, Option<u16>)` values: `Some(0)` is an exact dry
override and `None` restores the immutable recipe.

`ServerPacket::Region` carries the `Rollback` snapshot with the new state, so a
fresh client receives the exact authority. No additional client packet or
cross-region packet is introduced in this phase. Existing server and client
binaries must therefore be released together. The region face remains sealed
until a dedicated stamped debit/credit packet is defined.

`PortableRegion` carries the scheduler masks and cursors with `WaterRegion`;
restore does not scan a whole standing body. A legacy state without a saved
queue gets a bounded, resumable bootstrap over changed overrides.
World cuts write it to nullable `regions.water_state`:

```text
byte 0: FORMAT_REGION_WATER = 2
body:   zstd(bincode(game::WaterRegion))
```

The store explicitly reads the unshipped format-1 body and turns every absent
recipe cell in an activated chunk into a zero override. That preserves the
old activated-chunk dry semantics without re-refilling any lake/river cell.

Migration `0024_region_water_state` adds the nullable column to PostgreSQL and
SQLite. `NULL` decodes as legacy static-only water. A present but undecodable
blob is `StoreError::Corrupt` and is never regenerated.

## Rendering

The near-water pass stores one `WaterRegion` mirror per loaded region. Snapshot
state, Do deltas, and Undo deltas use the same "override, including zero,
else recipe" rule. One delta coalesces its affected columns/tile apron in a
`BTreeSet`; tile in-flight and builds-per-frame limits remain unchanged.
Partly filled cells currently render as a wet block volume; moving
velocity/current visualisation is intentionally absent.

Far `LodWater` remains recipe-derived and therefore is not promoted as current
dynamic state. B4 must either regenerate affected LOD or invalidate a bounded
far zone and fall back to near-only while it is stale.

## Observability

The host exposition includes:

- `mmo_water_frontier_cells`, `mmo_water_frontier_chunks`,
  `mmo_water_override_cells` and `mmo_water_oldest_fair_wait_ticks`;
- `mmo_water_operations_total{kind=...}` and
  `mmo_water_budget_exhausted_total{kind=...}`;
- `mmo_water_pending_transfers` and `mmo_water_oldest_pending_milliseconds`
  (zero while region transfer is sealed);
- `mmo_water_transfers_total{direction=local|face|source|sink}`;
- `mmo_water_active_cells`, `mmo_water_active_chunks`,
  `mmo_water_transferred_units_total` and
  `mmo_water_deferred_work_total`;
- average and maximum water-step wall-time gauges.

Each active region emits one bounded `[water]` summary every 20 world ticks.
No per-cell values, players, entities, raw SQL, or URLs enter that line.

## Deferred completion

Before normal multi-region/world promotion, B3 must:

1. represent neighbour head/room without copying pressure as authority;
2. debit one face owner, emit a transfer stamped for T+1, and de-duplicate its
   `(tick, face, sequence)` identity at the receiver;
3. journal state-bearing peer transfers and account for them in world-cut
   sender/receiver rows, abort, restart, and succession adoption;
4. preserve an exact debit/credit ledger through disconnect, cut, takeover,
   rollback generation, and restored pending work;
5. preserve canonical x-wrap ownership and prove split-region output matches
   one-region output.

B4 then needs dynamic LOD invalidation/refresh, explicit finite or reservoir
source/sink policy for forged oceans/lakes, and owner-approved performance
measurements against the provisional active-cell budgets.

## Validation

`just water-check` type-checks the production game/store/server/client slice and
audits committed tests. Client tests are deliberately not run by that recipe.
Performance, real-GPU water appearance, reconnect/prediction visuals, and
cross-region/cut scenarios remain manual acceptance work; no benchmark timings
are claimed here.
