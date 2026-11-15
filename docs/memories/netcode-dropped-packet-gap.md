---
name: netcode-dropped-packet-gap
description: "NOT FIXED (found 2026-09-27 browser check): server send_bounded drops a packet after SEND_DEADLINE 2 s → permanent gap in a stalled client's sequenced event stream → client panics 'Server event input buffer too big'"
metadata:
  type: project
---

Found 2026-09-27 during the rapier-0.36 plan's wasm browser check (full world, debug wasm, overloaded load); pre-existing netcode, not physics. **Not fixed**; the user is to schedule it.

- **What:** `crates/server/src/lib.rs` `send_bounded` (~line 155) wraps each `sink.send_packet` in `tokio::time::timeout(SEND_DEADLINE /* 2 s */)`; on timeout it logs `send to client timed out; skipping (peer stalled or gone)` and the packet is DROPPED. Events are sequenced (`base_event_id`), so the client never receives that id.
- **Symptom:** the client's `Region` buffers every later server event behind the missing id until `input_buffer.len() > 1000` → `panic!("Server event input buffer too big.")` (`crates/game/src/region.rs` ~206). Server log shows the timeout warning ×N first.
- **Fix direction (not decided):** on a dropped packet either disconnect the client or mark it for resync (fresh snapshot / re-send from the gap), never continue the sequenced stream past a hole.

**Why:** a slow or briefly stalled client (browser tab, weak GPU, big join burst) is turned into a crash rather than a lag spike.
**How to apply:** if a client panics "input buffer too big", check the server log for the send timeout before debugging the sim. Related: [[client-prediction-runaway]], [[vendored-deps-update-investigation]].
