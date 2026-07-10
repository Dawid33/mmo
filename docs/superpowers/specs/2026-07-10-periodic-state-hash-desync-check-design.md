# Periodic State-Hash Desync Check — Design

**Date:** 2026-07-10
**Status:** Approved

## Goal

Detect client/server simulation divergence in production, fast and loudly. Every
`STATE_HASH_INTERVAL_TICKS` ticks the server stamps a crc32 of a region's full
post-tick state onto that tick's event. The client verifies its own state at the
same log position and **crashes with a hard error on mismatch** — divergence is
always a determinism bug to fix, never something to paper over. No resync, no
recovery path.

## Background

- `game::state_hash(&Rollback) -> u32` already exists (crc32fast over `GameData`
  via `std::hash::Hash`) and is the basis of the existing test convergence
  assertions. It is deterministic across machines. This design reuses it
  unchanged.
- The world is multi-region: the server runs one `RegionRunner` per region; the
  client simulates its subscribed 3×3 window. Hashes are therefore **per
  region**, delivered to that region's subscribers, and each subscribed region
  verifies independently.
- The client's live state is *confirmed state plus applied predictions*. A hash
  of server state after tick N can only be compared against the client state
  with confirmed events applied **through exactly N and nothing after** —
  materializing that boundary state is the heart of the client-side check.
- This is orthogonal to the per-entry undo verification (`HashMode`): that
  verifies each mutation's inverse locally; this verifies that two machines'
  full states agree.

## Design

### Wire / event change

`GameEventKind::Tick` becomes:

```rust
Tick { state_hash: Option<u32> }
```

- `None` on ordinary server ticks and on **all** client-predicted ticks.
- `Some(hash)` every `STATE_HASH_INTERVAL_TICKS` server ticks, where
  `hash = state_hash(region_state_after_applying_this_tick)`.

The field is transport metadata only: it never influences `handle_event`, the
sim, or the undo log. Client and server binaries ship together, so the bincode
layout change needs no migration. `u32` is wasm32-safe.

New tunable in `crates/game/src/lib.rs` (same style as
`FAR_SNAPSHOT_MIN_INTERVAL_TICKS`):

```rust
/// Every this-many ticks a region stamps a full-state crc32 onto its Tick
/// event; subscribed clients verify and crash on mismatch.
pub const STATE_HASH_INTERVAL_TICKS: usize = 10; // ~5 s at TICK_RATE = 500
```

### Server side (`RegionRunner::tick`)

On a hash tick: process the tick as usual, then compute
`state_hash(self.region.data())` and stamp it into the outgoing event before
`RegionOutput::EventProcessed` broadcasts it. The hash therefore describes the
state **after** tick N, carried on tick N's event. The event the server keeps
internally is unaffected (`forget_last_event` trims it regardless); only the
broadcast copy needs the stamp — if the plan finds the event is constructed
before processing, stamping the broadcast copy after processing is the
mechanism.

The server never verifies anything and never crashes — it is authoritative.

The wasm/offline `local_server` path flows through the same
`RegionInput`/`RegionOutput` seam, so it gets hash ticks with no extra work.

### Prediction matching (reconcile must not see a mispredict)

The client predicts `Tick { state_hash: None }`; the server's hash-carrying
tick arrives as `Tick { state_hash: Some(_) }`. The reconcile comparison must
treat `Tick` vs `Tick` as equal regardless of the hash field — extending the
existing identity-matching precedent in `GameEventKind::matches_prediction`
(which already relaxes equality for `EntityArrived`/`GhostUpdate`). Both
comparison sites in `Region::reconcile` must use the relaxed comparison: the
prediction-removal matcher and the full-equality mispredict check
(`event != server_event.0`), whichever ones a `Tick` can reach.

### Client-side check (`Region::reconcile`)

When the client integrates a server `Tick { state_hash: Some(h) }` at event id
N, it verifies at the moment the confirmed-through-N state exists with no
predictions applied on top:

- **Match path** (predicted tick matched, MATCH+FORGET): that boundary state is
  not materialized — the live state has predictions with ids > N applied. Undo
  the log entries for those outstanding predictions (the same machinery the
  mispredict path uses), compute `state_hash`, compare with `h`, then replay
  the predictions via the existing replay path.
- **Mispredict path** (rollback+replay already happening): the replay
  materializes confirmed-through-N naturally — run the check right after
  applying server event N, before predictions re-apply. No extra rollback.
- **Snapshot-join path**: events with `id < base_event_id` keep their existing
  early return; their hashes are skipped (that state is baked into the join
  snapshot and has no log entries to roll back).

On success, continue as today. On mismatch:

```rust
GameError::StateDesync { region: RegionId, event_id: EventId, server_hash: u32, client_hash: u32 }
```

returned from `reconcile`, propagated through the existing
`ClientUpdateEvent::GameCrash` path, terminating the client with region, event
id, and both hashes in the message.

### Open point for the planning phase

When the client has zero predictions, `reconcile` currently parks server events
in `input_buffer` and returns early. The plan must locate where parked events
are eventually applied and ensure the hash check fires there too — the
client-behind regime (see the prediction-runaway issue) is exactly where that
path runs, and a check that silently skips it would go dormant in the worst
case. The verification rule is position-based, not path-based: *whenever a
hash-carrying server tick is integrated into confirmed state, verify at that
boundary*, whichever code path integrates it.

## Cost

- **Server:** one full-`GameData` crc32 per region per interval. Whole-field
  hashing is known-hot per-write; paid once per ~5 s per region it is noise.
- **Client:** one prediction rollback+replay per region per interval on the
  match path (equivalent to one mispredict, which happens far more often
  today); free on the mispredict path.

## Testing

All in the `game` crate's existing client/server convergence style, plus one
reconcile unit test:

1. **Convergence:** run a server+client pair through several hash intervals;
   assert no `StateDesync` and that at least one hash tick actually verified
   (guard against the check silently never firing).
2. **Divergence injection:** mutate the client's region state out-of-band, run
   to the next hash tick, assert `StateDesync` with the expected region and
   event id.
3. **No spurious mispredict:** a server `Tick { state_hash: Some(_) }` matched
   against a predicted `Tick { state_hash: None }` must not trigger the
   mispredict rollback path (assert via `ReconcileStats`).
4. **Client-behind path:** feed hash ticks through the parked/`input_buffer`
   route (no predictions outstanding) and assert verification still fires.

Any test measured over 100 ms gets
`#[ignore = "slow: >100ms — run via just test-full"]` per convention.

## Rejected alternatives

- **Separate `ServerPacket::StateHash` (out-of-band delivery):** relies on
  stream ordering to bind the hash to a log position; the in-band field is
  bound by construction and flows through `input_buffer` parking naturally.
- **Opportunistic check** (verify only when no predictions are outstanding):
  zero rollback cost, but silently stops checking whenever the client predicts
  ahead — the regime where desyncs matter most.
- **Resync on mismatch:** explicitly rejected — mismatch is always a bug;
  fail fast and fix the determinism break.
