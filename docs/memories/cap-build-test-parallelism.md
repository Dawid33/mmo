---
name: cap-build-test-parallelism
description: Builds/tests must be held to ~80% of CPU (16 of 20 cores) via core pinning — a job count alone does not cap CPU
metadata:
  type: feedback
---

Hold compilation and test runs to ~80% of logical cores (16 of 20 on this box). The user interrupted a full-parallelism `cargo check` on 2026-09-19, then asked "are you running benchmarks?" when the supposedly capped build still pegged every core.

**Why:** the user works on the desktop (VS Code, Firefox) while background jobs compile. `CARGO_BUILD_JOBS=16` was NOT enough: each rustc runs multi-threaded codegen at 100-200% CPU, so 16 jobs gave load average 20 on 20 cores. Only pinning worked (`taskset -a -cp 0-15` on the live cargo + rustc pids freed cores 16-19).

**How to apply:** go through `just` — its `cap` variable prefixes every build/test/check/clippy with `nice -n 10 taskset -c 0-15` (derived from `cpu_percent := "80"`); `cargo run` is deliberately not pinned. For any raw cargo call (yours or a subagent's) prefix `nice -n 10 taskset -c 0-15` by hand; `.cargo/config.toml` `jobs = -4` alone does not cap CPU. If asked whether something heavy is running, check `ps --sort=-pcpu` before answering. See also [[smoke-tests-not-full-suites]].
