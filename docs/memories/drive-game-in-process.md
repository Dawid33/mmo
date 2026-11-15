---
name: drive-game-in-process
description: never drive the game with xdotool/X tools; simulate players in-process (sim inputs / client packets / env knobs) like the tests and autopilot
metadata:
  type: feedback
---
Don't use xdotool, wmctrl, ImageMagick `import` or other desktop tools to drive or capture the running client. Simulate player interaction in-process: send sim inputs / client packets the way the harnesses and `renderer/autopilot.rs` do, and add env knobs (e.g. `VOXEL_FULLSCREEN`, an in-process screenshot) for window state and capture.

**Why:** user correction 2026-11-05 — external desktop automation is fragile and not how this project tests; in-process drivers are reproducible.

**How to apply:** for perf scenarios that need camera motion or actions, extend the autopilot / harness input path; for screenshots use Bevy's `Screenshot` from inside the client. Related: [[perf-at-256-regions]].
