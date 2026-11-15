---
name: laptop-power-clamp
description: MEASURED 2026-09-19 — on the ~88 W USB-C PD source this laptop clamps all cores to ~400–700 MHz under multi-threaded load; perf numbers are several× slow and noisy
metadata:
  type: project
---

The dev laptop (i7-12700H, T600 + eGPU RX 6700) was powered by a USB-C PD source at 19.5 V × 4.5 A ≈ 88 W (`/sys/class/power_supply/ucsi-source-psy-USBC000:001`), not its own adapter. Measured: 16 busy threads ⇒ every core ~400–700 MHz at 57 °C (not thermal); a single busy P-core reaches only 2.0–3.1 GHz of 4.6 GHz. Bursts run fast, then the clamp bites: a region build took 2.3 s right after a compile and 0.6 s after a pause, same binary. RAPL limits read 80/200 W and governor/turbo look normal — the clamp is firmware/EC-side, invisible in those knobs.

On 2026-09-20 the same port negotiated 19.5 V × 6.66 A ≈ 130 W and 16 busy threads held ~3.5 GHz — unclamped; the `full` join went 66 s → 20.5 s partly for that reason (region genesis upstream-vs-patched measured that day: 1277 → 231 ms, clean). So the state varies day to day: always check before comparing captures across sessions.

**Why:** it explained a 5× gap between headless (2 s) and live (10 s) region genesis, and it inflates every multi-threaded measurement (joins, LOD workers vs region threads, builds) and exaggerates contention.

**How to apply:** before trusting absolute timings, check `scaling_cur_freq` under load and the power source; compare ratios at the same scale, not seconds; don't time anything right after a compile. All 2026-09-19 numbers in `docs/superpowers/specs/2026-09-19-client-perf-findings.md` were taken in this state ([[perf-capture-tooling-landed]]). Same family of "check the machine first" as [[egpu-adapter-selection]].
