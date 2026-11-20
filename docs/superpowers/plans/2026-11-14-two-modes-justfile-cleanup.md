# Two Launch Modes: Justfile, Scripts and Dev-Mode Cleanup — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The game is started in exactly two ways — single player (`just run` / `play`: the client starts a loopback-only server) and multiplayer on Kubernetes (orion, and locally its k3d twin from the R5 plan) — and the justfile, `scripts/` and the launch code hold nothing that serves a third: no standalone dev server, no home-grown multi-process stack.

**Architecture:** Five phases, each leaving the tree working. Phase 0 deletes what nothing uses. Phase 1 makes single player self-sufficient and enforced (loopback bind, scrubbed child environment) and moves every probe / perf recipe onto it, which removes the standalone dev server on 6466 and the `VOXEL_SKIP_MENU` bypass. Phase 2 is R5's k3d twin (built by `plans/2026-11-15-r5-k8s.md`, not here) plus one convenience recipe on top. Phase 3, gated on R5's hand checks, deletes every native multiplayer recipe, the supervisor script and the server code only they used. Phase 4 splits the justfile into imported files. Phases 0, 1 and 4 do not wait for R5.

**Tech Stack:** `just` 1.42 (`import`), bash, Rust (`crates/server`, `crates/client`, `crates/login`), k3d (through R5's recipes), docker compose (kept only for the test Postgres and the site's dev Keycloak, `deploy/dev/compose.yml`).

**Spec:** none — the design decisions are in "Decisions" below. Depends on `docs/superpowers/plans/2026-11-15-r5-k8s.md` (in progress). Inventory surveyed 2026-11-14 at 147 recipes / 2,234 justfile lines; by 2026-11-15 R5 had grown it to 165 / 2,543, so **every `justfile:<line>` below is stale — find recipes by name**.

## Where we are (surveyed 2026-11-14, R5 status 2026-11-15)

Today there are **five** ways to run the game, not two:

| # | Mode | How it is started | Stays? |
|---|------|-------------------|--------|
| 1 | Single player: client spawns `server` with `VOXEL_SINGLE_PLAYER=1` on a free loopback port | `just play`, `just run`, `run-two`, `*-release` | **yes — mode A** |
| 2 | Standalone dev server on 127.0.0.1:6466, local auth, reached by `VOXEL_SKIP_MENU=1` | `server`, `client`, `play-test`, `web`, `fly-probe`, `shuttle-probe`, `render-bench`, `diag`, `perf-capture`, `fps-steady`, `gpu-measure`, `trace-chrome`, `trace-join`, `profile-*`, `perf*` (22 recipes, 3 scripts) | no — folds into 1 |
| 3 | Native multi-process multiplayer: login + coordinator + hosts as local processes, compose Postgres + Keycloak | `dev-login`, `run-online`, `coordinator`, `host`, `dev-r2`, `dev-r3`, `go-nogo-r3`, `login*`, `scripts/dev_r3.sh` | no — deleted in Phase 3; local multiplayer is the k3d twin (mode B) |
| 4 | Public web build: the `all`-role server with **local auth on 0.0.0.0** in docker compose on the orion box | `deploy`, `deploy-build`, `deploy-push`, `deploy-logs`, `deploy/web/`, `scripts/deploy_push.sh` | no — superseded by R5's `world-images-build` / `orion-world-push` / `orion-world-update`; R5 Phase D step 15 stops it on the box, Task 11 deletes `just deploy` and its files |
| 5 | Local k0s-in-docker cluster (Agones + CNPG; never ran a game role) | `dev-cluster-*`, `deploy-local*`, `kube`, `deploy/modules/platform` | no — removed by R5 Task 13, replaced by the k3d twin |

Facts the tasks rest on:

- **Nothing enforces "no external connections" in single player.** The server binds `VOXEL_BIND_ADDR` (default 127.0.0.1, `crates/server/src/lib.rs:33-44`) in every mode, and `LocalHost::start_on` (`crates/client/src/local_host.rs:142-150`) spawns the child with the client's whole environment — an exported `VOXEL_BIND_ADDR=0.0.0.0`, `VOXEL_ROLE` or `VOXEL_AUTH` reaches it.
- Multiplayer runs on Kubernetes only: orion's k3s, and locally the k3d twin R5 builds (`twin-up|images|apply|check|down|kube`, `world-*`). Decided 2026-11-15, replacing two earlier drafts of this plan (minikube; a consolidated native `run-mp`). R5 itself still needs the native stack until its Phase D steps 1-2 are done, which is Phase 3's gate.
- No CI. No Rust test starts the `server` binary; the integration tests build `WorldIngress` / `WorldManager` in-process, so removing mode 2 and 3 **recipes** breaks no test.
- `TODO.md` still owes human live checks that name `just dev-r3 2`, `just run-online`, `just run-two` (lines 149, 159-162, 194).

## Decisions

Mine unless marked. **D1 and D4 need your answer before the task that depends on them**; the rest are calls I made and you can overrule.

- **D1 (yours, before Phase 1) — `run-two`.** Two clients in one single-player world over loopback (`--port` / `--join`). Loopback is not "external", so I keep it as a debug launch. If "single player" should mean exactly one client, say so and Task 4 also removes `--port` / `--join` and `run-two`.
- **D2 — the public web build / `just deploy`: superseded by R5** (decided 2026-11-15). R5's world recipes ship the game; its Phase D step 15 stops the compose `server` and `mmo-web`; Task 11 then removes `just deploy`, `deploy-build`, `deploy-push`, `deploy-logs`, `orion-login-token-push`, `deploy/web/` and `scripts/deploy_push.sh`.
- **D3 — `go-nogo-r3`: deleted with the rest** ("all the home-grown multi-process mp"). Its 2026-11-13 verdicts stay recorded in TODO.md; a host-seam measurement on the twin, if ever wanted, is new work.
- **D4 (yours, before Task 9) — the browser client in dev.** `just web` (wasm-server-runner on :1334 plus a dev server on 6466) goes; the page then runs only from the twin's `web` image at `game.localhost:8480`, so every wasm change costs a `twin-images` round. If that loop is too slow, keep a `web` recipe that serves the page with `?login=http://127.0.0.1:8790` against the twin's port-forwarded login service — say so and Task 9 rewrites `web` instead of deleting it.
- The dev Postgres and the dev Keycloak stay, as test and website fixtures only: `pg-*` for `test-pg`, `test-login-pg`, `test-r2`, `test-r3`, `test-r4`; `kc-*` for `site-dev`, `site-theme-check`.
- `just login` (the native login service and its operator commands on a local sqlite) goes with the stacks: operator commands run in a pod (`orion-login …`, `twin-kube -n login exec …`).
- `run-release` / `play-release` stay (one extra recipe each beats a `profile=` variable everyone must relearn). `server`, `server-release`, `client`, `client-release`, `play-test`, `build-stable` go.
- Recipe names are kept wherever the recipe survives; nothing is renamed for taste.

## Global Constraints

- No git worktrees; work on `develop` in this checkout. **R5 is being implemented in this same checkout right now**: the tree has other uncommitted work in `crates/`, `deploy/`, `justfile` and `CLAUDE.md`. Every commit here stages only its own hunks (partial staging with `-U3` + `--recount`, never `--unidiff-zero`; check `git diff --cached` before committing), never `-A`, and no task here edits a path R5's "File structure" table claims until R5 has committed it.
- The root `justfile` stays canonical: a recipe and what CLAUDE.md says about it change in the same commit.
- Never run whole game/server/client suites as a routine check: `just smoke`, plus `just test-one <crate> "<filter>"` for touched tests. Running client tests needs the user's OK first (CLAUDE.md) — Tasks 3 and 4 name the exact filters to ask about.
- Ask before launching benchmarks. Tasks 5 and 6 change perf recipes; verifying them means one short run each — ask first, compare ratios within one region size.
- Play recipes build at `play_blocks` (256); tests/check at 64. Never a literal region edge.
- `deploy/**/*.tfvars`, `.env`: never committed, never printed.
- Destructive steps outside git (docker volumes, a root-owned file, anything on the orion box) are marked **USER** and are run by the user or after an explicit yes.

## Review Focus

1. A stale shell export (`VOXEL_BIND_ADDR`, `VOXEL_ROLE=host`, `VOXEL_AUTH=jwt`, `VOXEL_WORLD_DB=postgres://…`) when starting single player — the child must still be a loopback, `all`-role, sqlite, local-auth server. Task 3.
2. A perf / probe recipe run while another single-player world is open — each must use its own world directory and a free port, never 6466. Task 5.
3. A probe recipe whose client dies early — the child server must still exit (stdin EOF) and the recipe must not hang on a `wait`. Task 5.
4. `perf-server`: the server now starts as the client's child, so the profiler must wrap the child (`VOXEL_SERVER_BIN`) and keep its stdin pipe. Task 6.
5. A doc or script that still names a deleted recipe — Task 13's grep must come back empty.

---

## Phase 0 — delete what nothing uses

### Task 1: Dead files and broken references

**Files:**
- Delete: `scripts/script1.sh`, `scripts/script2.sh`, `scripts/dump.sh`, `scripts/show.sh`, `scripts/open_file.sh`, `scripts/kitty`, `scripts/kitty.conf` (2024 leftovers from another project), `scripts/run.sh`, `scripts/build.sh`, `scripts/perf.sh` (duplicate `just run` / `build` / `perf`), `AGENTS.md` (2026-07, superseded by CLAUDE.md, lists 5 crates), `.agents/` (empty)
- Modify: `.gitignore` (add `__pycache__/`), `justfile` (the `perf` recipe's "see scripts/perf.sh" comment; `build-stable`; header comment lines 7-9 that say "no nightly" — CLAUDE.md pins a dated nightly; the orphan "Server address (informational…)" comment above `_default`)
- Modify: `TODO.md:14` (`just prefab-editor`), `TODO.md:217` (`just run-studs`), `crates/worldgen/tests/client_region_load_cost.rs:6,24` (`just bench-client-region` → the recipe that runs it today, `bench-region`, after checking its body), `docs/memories/MEMORY.md:45` + `docs/memories/tracy-headless-export.md:17` (`tracy-capture`), `docs/memories/vendored-deps-update-investigation.md:31` (`determinism-wasm-next`)
- Create: recipe `scripts-test` in `justfile`: `python3 -m unittest discover -s scripts/tests -v` (the test of `crossing_summary.py` exists and nothing runs it)

- [ ] **Step 1:** `git rm` the tracked files above; `rm -rf scripts/__pycache__ scripts/tests/__pycache__ .agents`.
- [ ] **Step 2:** Delete recipe `build-stable`; fix the justfile comments named above; add `scripts-test`.
- [ ] **Step 3:** Fix each broken reference: where a current recipe does the job, name it; otherwise remove the sentence. Do not invent recipes.
- [ ] **Step 4:** Verify. Run: `just --list >/dev/null && just scripts-test && just smoke`. Expected: unittest `OK`, `>> smoke ok`.
- [ ] **Step 5:** Commit: `chore: drop dead scripts and recipe references nothing resolves`.
- [ ] **Step 6 (USER):** `sudo rm perf.data` (6.5 GB, root-owned, git-ignored, from 2026-07-10). Commit `scripts/tunnel.sh` if `just tunnel` should keep working for anyone else (it is untracked today).

### Task 2: The local Pyroscope stack

The k0s scaffold (`dev-cluster-*`, `deploy-local*`, `kube`, `deploy/modules/platform`, the old `deploy/envs/local/world`, the Agones `deploy/modules/world`) is **not this plan's**: R5 Task 13 removes it and puts the k3d twin in its place. Do not touch those paths here.

**Files:**
- Delete: `docker-compose.yml`, `scripts/provisioning/`, recipes `profile-up`, `profile-down`, `profile-client`, `profile-server` (local Pyroscope + Grafana, last touched 2025-12; orion has both)
- Modify: `crates/client/Cargo.toml` and `crates/server/Cargo.toml` — leave the `pyroscope` features in place (orion has a Pyroscope a deployed host can push to); only the recipes go

- [ ] **Step 1:** Confirm nothing else uses the compose file. Run: `grep -rn -E 'docker-compose\.yml|scripts/provisioning|profile-(up|down)' justfile scripts deploy crates CLAUDE.md TODO.md`. Expected: only the four recipes and the compose file itself.
- [ ] **Step 2:** Delete the files and recipes.
- [ ] **Step 3:** Verify. Run: `just --list >/dev/null && grep -n -E 'profile-(up|down|client|server)' justfile CLAUDE.md`. Expected: no output.
- [ ] **Step 4:** Commit: `chore: remove the local Pyroscope stack`.

---

## Phase 1 — single player is the only local mode

### Task 3: A single-player server is loopback-only, whatever the environment says

**Files:**
- Modify: `crates/server/src/lib.rs` (bind resolution near :33-44, the `run()` switch order near :683-740), `crates/client/src/local_host.rs:142-150`
- Test: unit tests beside each (`crates/server/src/lib.rs` `#[cfg(test)]`, `crates/client/src/local_host.rs` tests at :203-345 which already use shell-script fake servers)

**Interfaces:**
- Produces: `fn bind_ip(single_player: bool, value: Option<&str>) -> Result<IpAddr, String>` (replaces direct use of `bind_ip_from` in `run()`); `fn single_player_refusal(role: &str, hosts: u32, store_is_postgres: bool) -> Option<String>`; in the client `const CHILD_ENV_SCRUB: &[&str]`.

- [ ] **Step 1: Failing server tests** in `lib.rs`:

```rust
#[test]
fn single_player_binds_loopback_whatever_the_env_says() {
    assert_eq!(bind_ip(true, Some("0.0.0.0")).unwrap(), IpAddr::from([127, 0, 0, 1]));
    assert_eq!(bind_ip(true, None).unwrap(), IpAddr::from([127, 0, 0, 1]));
    assert_eq!(bind_ip(false, Some("0.0.0.0")).unwrap(), IpAddr::from([0, 0, 0, 0]));
}

#[test]
fn single_player_refuses_cluster_settings() {
    assert!(single_player_refusal("all", 1, false).is_none());
    assert!(single_player_refusal("host", 1, false).is_some());
    assert!(single_player_refusal("all", 2, false).is_some());
    assert!(single_player_refusal("all", 1, true).is_some());
}
```

- [ ] **Step 2:** Run: `just test-one server "--lib single_player"`. Expected: FAIL, `bind_ip` not found.
- [ ] **Step 3:** Implement both functions. In `run()`: read `VOXEL_SINGLE_PLAYER` **before** `VOXEL_ROLE` (today the role is read first, lib.rs:683, and `coordinator` returns early); a `Some(reason)` from `single_player_refusal` logs it and exits non-zero; the listeners use `bind_ip(single_player, …)`. A set-and-ignored `VOXEL_BIND_ADDR` logs one `warn!`, like the ignored `VOXEL_AUTH=jwt` does (auth_edge.rs:100-114).
- [ ] **Step 4:** Run the same command. Expected: PASS.
- [ ] **Step 5: Failing client test** in `local_host.rs`, using the existing fake-server helper: a fake server that writes `env` to its log. The test must not mutate the process environment (tests share it), so split `start_on` into a private `fn child_command(binary, terrain, port, inherited: impl Iterator<Item = (OsString, OsString)>) -> Command` that starts from `env_clear()` plus `inherited` minus the scrub list; `start_on` passes `std::env::vars_os()`. The test passes `[("VOXEL_BIND_ADDR","0.0.0.0"), ("VOXEL_ROLE","host"), ("VOXEL_AUTH","jwt"), ("PATH", <the real one>)]` and asserts the log contains `VOXEL_SINGLE_PLAYER=1`, `PATH=` and none of the three.
- [ ] **Step 6:** Implement `CHILD_ENV_SCRUB` = `VOXEL_BIND_ADDR`, `VOXEL_ROLE`, `VOXEL_AUTH`, `VOXEL_OWNERSHIP`, `VOXEL_HOSTS`, `VOXEL_HOST_ORDINAL`, `VOXEL_WT_PORT`, `VOXEL_WT_CERT`, `VOXEL_WT_KEY`, `VOXEL_WORLD_ID`, `VOXEL_LOGIN_URL`, `VOXEL_JWT_ADMISSION`, `VOXEL_SERVICE_TOKEN`, `VOXEL_SERVICE_TOKEN_FILE`, `VOXEL_WORLD_DB_DIRECT`; `child_command` drops each from `inherited` and then sets its own four variables. (A deny list, not an allow list: the child needs `PATH`, `HOME`, `RUST_BACKTRACE`, `HASH_MODE`, `VOXEL_SAVE_SECS`, the perf knobs — and Task 3's server-side refusal is the backstop for a variable this list misses.)
- [ ] **Step 7 (ask first — client test):** `just test-one client "--lib local_host"`. Expected: PASS.
- [ ] **Step 8:** `just smoke`, then commit: `fix(server,client): a single-player server binds loopback and ignores cluster settings`.

### Task 4: `client debug world` — the debug launch the probes need

The bypass (`VOXEL_SKIP_MENU`, `crates/client/src/session.rs:324-334`, `main.rs:197-207`) connects to an external server on 6466 with no embark. The probes need the same thing from a server the client owns.

**Files:**
- Modify: `crates/client/src/debug_launch.rs` (`DebugScenario` :95-102, `parse_args` :130-188, `boot` :365-449)
- Test: the `parse_args` unit tests in the same file

**Interfaces:**
- Produces: `DebugScenario::World { world: Option<PathBuf>, reset: bool }`; CLI `client debug world [--world <dir>] [--reset]`. Default directory `worlds/debug-world/`, forged on first use with the same `ForgeParams` as `debug embarked`. It inserts `PendingWorld` exactly as `Embarked` does, so the single-player child starts through `LocalHost` — and then does **not** embark: the session ends up where the bypass left it (connected, `Credential::Local`, the camera `VOXEL_CAMERA` / the autopilot selects).
- Consumes: Task 3's scrubbed `LocalHost`.

- [ ] **Step 1: Failing tests:** `parse_args(["debug","world"])` → `World { world: None, reset: false }`; `["debug","world","--world","worlds/x","--reset"]` → `World { world: Some("worlds/x".into()), reset: true }`; `["debug","world","--join","127.0.0.1:1"]` → the existing unknown-flag error.
- [ ] **Step 2 (ask first):** `just test-one client "--lib debug_launch"`. Expected: FAIL.
- [ ] **Step 3:** Implement. `DebugScenario::adventure_at` returns `None` for `World`. In `boot`, `World` shares `Embarked`'s forge-or-reuse path and stops before the embark request. `RENDER_BENCH` and `VOXEL_AUTOPILOT` must keep working under it — they currently also flip `skip_menu` (session.rs:331-332); here they only need a connected session.
- [ ] **Step 4 (ask first):** rerun. Expected: PASS. Then by hand: `just run` unchanged; `./target/debug/client debug world` lands in the world without a fortress and `worlds/debug-world/server.log` shows a loopback bind on a non-6466 port.
- [ ] **Step 5:** Commit: `feat(client): debug world — a single-player launch without embarking`.

### Task 5: Move the probe and measurement recipes onto the single-player child

**Files:**
- Modify: `justfile` recipes `fly-probe`, `shuttle-probe`, `render-bench`, `diag`, `perf-capture`, `fps-steady`, `gpu-measure`, `gpu-log`, `trace-chrome`, `trace-join`, `tracy-client`; `scripts/fps_steady.sh:19-21`, `scripts/gpu_measure.sh:19-21`, `scripts/perf_capture.sh`
- Modify: `scripts/perf_capture_report.py` / `crates/game/src/perf.rs` only if they name `target/*/server.log`

**The one change, applied to each:** drop `export VOXEL_SKIP_MENU=1`, the `./target/<profile>/server > …/server.log &`, its `server_pid` and `trap`; start the client as `./<dir>/<profile>/client debug world --world worlds/probe-<recipe>`; read the server log from `worlds/probe-<recipe>/server.log` (copy it into the recipe's `target/<recipe>/` output directory at the end, so `perf-capture`'s one-directory contract holds). Keep `{{cap}} {{cargo}} build … --bin client --bin server` — the client finds `server` beside its own executable, and for variant builds (`--target-dir {{probe_dir}}`) export `VOXEL_SERVER_BIN=$PWD/target/release/server` so the plain server is used.

- [ ] **Step 1:** Convert `fly-probe` and `shuttle-probe`. Verify (ask first — a 20 s release run): `just adapter=<yours> shuttle-probe region 20`. Expected: the `crossing_summary.py` table prints with ≥ 1 crossing, and `ss -Huln 'sport = :6466'` is empty while it runs.
- [ ] **Step 2:** Convert the rest. For each, the check is the recipe's own final summary line appearing and no process left behind: `pgrep -x server` empty after exit.
- [ ] **Step 3:** Kill a probe's client mid-run (`pkill -x client`). Expected: the recipe returns and `pgrep -x server` is empty within `STOP_GRACE` (30 s).
- [ ] **Step 4:** Commit: `refactor(just): probes and measurements start their own single-player world`.

### Task 6: `perf-server` — profile the child

**Files:**
- Create: `scripts/server_under_perf.sh` — `exec perf record $PERF_FLAGS -o "$PERF_OUT" -- "$REAL_SERVER" "$@"` (stdin is inherited, which keeps the parent-alive pipe)
- Modify: `justfile` `perf`, `perf-client`, `perf-server`

- [ ] **Step 1:** `perf-client`: `perf record` wraps `./target/profiling/client debug world --world worlds/probe-perf`; the child server is not sampled (perf follows only with `-a`/inherit — pass `--no-inherit`).
- [ ] **Step 2:** `perf-server`: run the plain client with `VOXEL_SERVER_BIN=$PWD/scripts/server_under_perf.sh REAL_SERVER=$PWD/target/profiling/server PERF_OUT=$PWD/target/perf-server.data`. It needs `perf_event_paranoid` to allow user sampling instead of today's `sudo -E perf record`; if the box refuses, the recipe prints the `sysctl` line and exits 1 rather than running the client under sudo.
- [ ] **Step 3 (ask first):** one run of each; `just perf-summary server` prints its tables. Expected: the server table's top thread is a region thread, not `client`.
- [ ] **Step 4:** Commit: `refactor(just): perf-server samples the single-player child`.

### Task 7: Remove the standalone dev server and the bypass

**Files:**
- Delete recipes: `server`, `server-release`, `client`, `client-release`, `play-test`
- Modify: `crates/client/src/session.rs:324-345` (`skip_menu_from_env`, `skip_menu`), `crates/client/src/main.rs:191-207` (the hardcoded `127.0.0.1:6466` connect), their tests; `justfile` variable `world_file` and its comment (:50-60) — a world file is now passed as `client debug world --world`, and `VOXEL_WORLD_FILE` stays an internal client→child variable only
- Modify: `crates/server/src/lib.rs:46-50` doc comments ("the dev server's QUIC port (`just run` / `just server`)")
- Modify: `CLAUDE.md` (Building and running: drop `server` + `client`, "`VOXEL_SKIP_MENU=1` connects to the dev server"), `TODO.md` mentions

**Not in this task:** `just web` still starts a server on 6466 (it goes in Task 9, D4) and `dev-login` / `run-online` / `host` use the default port; `DEFAULT_SERVER_PORT` stays until Phase 3.

- [ ] **Step 1:** Run: `grep -rn 'VOXEL_SKIP_MENU\|skip_menu' crates scripts justfile`. Expected after Tasks 5-6: only `session.rs`, `main.rs` and the five recipes above.
- [ ] **Step 2:** Delete `skip_menu_from_env` / `skip_menu` and the `if skip_menu { … 6466 … }` branch; `RENDER_BENCH` / `VOXEL_AUTOPILOT` no longer imply a launch mode.
- [ ] **Step 3:** Delete the five recipes; edit CLAUDE.md and TODO.md.
- [ ] **Step 4:** `just smoke`; `just run` and `just play` by hand. Expected: both reach the world; `grep -rn SKIP_MENU .` (outside `target/`, `docs/superpowers/`) is empty.
- [ ] **Step 5:** Commit: `refactor: one local mode — remove the standalone dev server recipes and VOXEL_SKIP_MENU`.

---

## Phase 2 — local multiplayer is R5's k3d twin

Not built here. `docs/superpowers/plans/2026-11-15-r5-k8s.md` Task 13 delivers `twin-up`, `twin-images`, `twin-apply`, `twin-check`, `twin-down`, `twin-kube` (cluster `k3d-mmo`: CloudNativePG, the login service, one world `twin` with a coordinator and two hosts on node ports 30466-30469, the page at `game.localhost:8480`), and its Task 12 the `world-*` recipes that work against it with `world_ctx=k3d-mmo world_ns=world-twin`. R5 Task 13 also removes the k0s scaffold.

### Task 8: `just run-twin` — a client in the twin, like `just run`

R5 leaves "play on the twin" as four hand commands (its Phase D step 2: `issue-refresh` in the login pod, a port-forward, two client variables). This is the one recipe that makes the twin a launch mode. **My proposal, not part of R5 — drop the task if the hand commands are enough.**

Gate: R5 Task 13 committed and `just twin-check` passing.

**Files:**
- Modify: `justfile` — recipe `run-twin`, variable `twin_dir := "target/twin"`

**Interfaces:**
- Consumes: R5's `twin-up`, `twin-images`, `twin-apply`, `twin-kube`, `world-status`, `world-ctl`; `client debug online --world twin`.
- Produces: `just run-twin` with the `user`, `adapter`, `world`, `view` dials and `images=1`.

**What it does, in order:**

1. No cluster `mmo` (`k3d cluster list`) → `twin-up twin-images twin-apply`. Cluster stopped → `k3d cluster start mmo`. `images=1` → `twin-images`, then restart the world's workloads the way `scripts/world_update.sh` does (stop, set, start — never a bare rollout: hosts are `OnDelete`).
2. `just world_ctx=k3d-mmo world_ns=world-twin world-status` says `forge` → `world-ctl forge` with the default params, as `scripts/twin_check.sh` step 2 does; wait until the coordinator and both hosts are Ready (300 s, then exit 1 naming the pod that is not).
3. Build the client at `play_blocks` (capped) — the twin's server image is built at the same dials by `world-images-build`; say so if `region_blocks` / `chunk_blocks` were overridden for one and not the other.
4. `twin-kube -n login exec deploy/login -- login account issue-refresh <user or dev>@localhost` → `target/twin/refresh-<user>` (mode 0600).
5. `twin-kube -n login port-forward svc/login 8790:80` in the background (killed on exit; refuse to start if TCP 8790 is taken, naming what holds it).
6. `VOXEL_LOGIN_URL=http://127.0.0.1:8790 VOXEL_LOGIN_REFRESH_FILE=$PWD/target/twin/refresh-<user> ./target/debug/client debug online --world twin`. Print the page's address (`http://game.localhost:8480/`) beside it for the browser.
7. On exit the cluster keeps running (`just twin-down` stops it): a second `just user=bob run-twin` is a second player.

- [ ] **Step 1:** Read R5 Task 13 as landed (`scripts/twin_check.sh`, the `twin-*` recipes) and take the exact namespace, deployment and service names from it — the ones above are from the plan text, not from code.
- [ ] **Step 2:** Write the recipe. `just --dry-run run-twin` prints the six steps; with the cluster absent and `k3d` missing from `PATH` it exits 1 naming `k3d`.
- [ ] **Step 3 (by hand — ask first, a live run):** `just run-twin` on a machine with no cluster → in the world as `dev@localhost`; cross the seam between the hosts; close the window → `pgrep -f 'port-forward svc/login'` is empty and the pods still run. `just user=bob run-twin` beside it → two players.
- [ ] **Step 4:** Commit: `feat(just): run-twin — a client in the k3d twin in one command`; add `run-twin` to CLAUDE.md's key recipes.

---

## Phase 3 — remove the home-grown multi-process multiplayer

Gate — all three, because R5 itself still uses the native stack:
- R5 Phase D step 1 passed (the R4 hand checks, which R5 runs under `just dev-r3 2`);
- R5 Phase D step 2 passed (the twin with a player);
- R5's Global Constraint "`dev-r2`, `dev-r3` and `go-nogo-r3` keep their behaviour" is void once those two are ticked — say so in R5's plan in the same commit as Task 9.

### Task 9: Delete the native stacks

**Files:**
- Delete recipes: `dev-login`, `run-online`, `coordinator`, `host`, `dev-r2`, `dev-r3`, `go-nogo-r3`, `login`, `login-keygen`, `login-world-add`, `login-world-list`, `web` (D4), private `_dev-login-setup`, `_dev-token`, `_dev-r2-token`, `_dev-hosts-check`
- Delete variables: `with_web`, `dev_email`, `dev_oidc_issuer`, `online_wait_secs`, `dev_r2_dir`, `dev_r3_dir`, `host_env_sh`, `dev_r3_launch_sh`, `go_nogo_dir`, `go_nogo_db`, `go_nogo_db_url`, `login`
- Delete: `scripts/dev_r3.sh` and its stub tests (`grep -rn 'DEV_R3_' scripts crates`); the `--go-nogo` mode of `scripts/crossing_summary.py` with its tests in `scripts/tests/test_crossing_summary.py` (the per-crossing table stays — `fly-probe` / `shuttle-probe` use it)
- Keep: `pg-up|down|reset|psql` and `dev_database_url` (`test-pg`, `test-login-pg`, `test-r2`, `test-r3`, `test-r4` need the dev Postgres), `kc-up|down` (`site-dev`, `site-theme-check`), `worldctl`, `orion-login`
- Modify: `deploy/dev/README.md` and `deploy/dev/compose.yml` comments that describe the dev stacks; `deploy/dev/keycloak/players-realm.json` stays (the site checks use it)
- Modify: `CLAUDE.md` — every `just dev-login|dev-r2|dev-r3`, `just coordinator|host`, `go-nogo-r3`, `scripts/dev_r3.sh`, `DEV_R3_*`, `target/dev-r2|dev-r3` sentence in the Auth, Role split and Many hosts bullets → the twin (`twin-*`, `run-twin`, `world-*`); the `claim` bullet's "(default of `dev-r3` / `go-nogo-r3`, and of `just host` / `just coordinator` with several hosts; one host and `dev-r2` stay `static`)" → what the world module sets
- Modify: `TODO.md` — open live checks that name `dev-r3` / `run-online` (lines 149, 160-162, 194 as of 2026-11-14) reworded to the twin or closed; DONE entries left as history
- Modify: Rust doc comments naming the old recipes or directories (`crates/server/src/roles.rs`, `login_link.rs`, `peer_link.rs`, `lease.rs`, `crates/client/src/debug_launch.rs`)
- Delete (USER, after a look): `target/dev-login`, `target/dev-r2`, `target/dev-r3`, `target/go-nogo-r3` (git-ignored state; `target/go-nogo-r3/{rounds,after}` hold the 2026-11-13 measurements TODO.md cites — copy what you want kept first), and the `mmo_go_nogo_r3` database (`just pg-psql -c 'drop database mmo_go_nogo_r3'`)

- [ ] **Step 1:** Delete and reword; then run: `grep -rn -E 'dev-r[23]|dev-login|run-online|go-nogo|dev_r3|DEV_R3_|just (coordinator|host|login)\b' justfile CLAUDE.md scripts crates deploy/dev`. Expected: no output.
- [ ] **Step 2:** `just scripts-test && just smoke`. Commit: `chore(just): local multiplayer is the k3d twin — remove the native stacks`.

### Task 10: Remove the code only the native stacks used

R5 changes the same files (its Tasks 1-3: link addresses as names, link identities from mounted files, the advertised peer address). **Read each site as it is on the day** — the line numbers below are from 2026-11-14, before R5.

**Files:**
- Modify: `crates/server/src/auth_edge.rs` (drop the `token` alias of `VOXEL_AUTH`; `local` and `jwt` remain) + the mode table in `crates/server/tests/auth_edge.rs`
- Modify: `crates/server/src/login_link.rs`, `crates/server/src/lib.rs` (drop `VOXEL_JWT_ADMISSION` / `JwtAdmission::Newest`: only `dev-login` and `run-online` ran a `jwt` host without a service token; now it refuses to start, no exception)
- Modify: `crates/server/src/peer_link.rs` (drop `VOXEL_PEER_LINK_DELAY_MS` and the held-frame path: "measurement only", and its only users were `dev-r3`'s `delay` and `go-nogo-r3`) — **unless** `grep -rn PEER_LINK_DELAY deploy crates/*/tests` shows the world module or a test sets it
- Modify: `roles.rs`, `login_link.rs`, `peer_link.rs` — any pin / identity file default still pointing under `target/dev-r2/` or `target/dev-r3/` after R5 Task 2 → no default: unset is a startup error naming the variable
- Modify: `crates/login/src/config.rs` — `VOXEL_LOGIN_DB` / `VOXEL_LOGIN_KEYS_DIR` default to `target/dev-login/…`; with `just login` gone nothing wants a repo-relative default: unset is an error naming the variable (the login tests pass their own; check `test-login-pg` first)
- Leave: `VOXEL_ROLE=all`, `VOXEL_OWNERSHIP=static|allinone` (single player and the in-process tests), `VOXEL_BIND_ADDR`, `VOXEL_SERVER_PORT`
- Modify: `crates/client/src/local_server.rs` module doc ("embedded single-player server for the wasm build" — it is the test harness's in-process server)
- Modify: `CLAUDE.md` Auth bullet (`VOXEL_AUTH=token (default, offline)` → `local`; drop the `VOXEL_JWT_ADMISSION=newest` sentence; drop `VOXEL_PEER_LINK_DELAY_MS` from the Env list)

- [ ] **Step 1: Failing tests first.** In `tests/auth_edge.rs`: `VOXEL_AUTH=token` is an error naming the valid values. In the `lib.rs` unit tests: jwt + no service token → refusal, with `VOXEL_JWT_ADMISSION=newest` set as well → still refusal. In the `roles.rs` and `login/config.rs` tests: the variable unset → an error containing its name.
- [ ] **Step 2:** `just test-one server "--test it auth_edge::"`, `just test-one server "--lib"`, `just test-one login "--lib config"`. Expected: FAIL on those.
- [ ] **Step 3:** Implement the removals.
- [ ] **Step 4:** Rerun; then, as this crosses the server's startup, each touched crate's whole non-client suite once (say so first; needs `just pg-up`): `just test-server`, `just test-r2`, `just test-r3`, `just test-r4`, `just test-login-pg`. Expected: PASS — the tests set pins and tokens explicitly through `test-hooks`; any that leaned on a default is fixed in the test.
- [ ] **Step 5:** The twin still works with the new binary: `just twin-images twin-apply twin-check`. Expected: exit 0. Commit: `refactor(server): drop the dev-only auth, delay and path fallbacks`.

### Task 11: Remove `just deploy` — superseded by R5's world recipes

`just deploy` (build the release `server` + the browser bundle, rsync them to `root@5.9.7.138:/srv/mmo`, restart the docker compose `server` and `mmo-web`) shipped the OLD public game: one `all`-role server, local auth on 0.0.0.0, at `orion.factoriosurvivalworld.com`. R5 replaces every part of it:

| `just deploy` piece | R5 replacement |
|---|---|
| `deploy-build` (release `server` + `web-release` bundle) | `world-images-build` (release `server` + `worldctl`, bundle built with no `VOXEL_WEB_SERVER` / `VOXEL_LOGIN_URL`) |
| `deploy-push` (`scripts/deploy_push.sh`: rsync + compose restart) | `orion-world-push` (images to ECR, digests printed) + `orion-world-update <server> <web>` (`scripts/world_update.sh`: stop → set images → start), `orion-world-rollback` |
| `deploy-logs` (ssh + `docker compose logs`) | `world-logs <pod>`, `world-status`, `world-metrics` |
| `deploy/web/compose.yml` (`server` + caddy `mmo-web`) | `deploy/modules/world` (coordinator, `hosts` StatefulSet, `web` = page + `/login` proxy, `client-tls`) |
| `orion-login-token-push` (service token to `/srv/mmo/secrets/` for the compose host) | the `world` module takes the login module's service token as a Secret |

Gate: R5 Phase D step 15 done — the world is played on at `game.errantry.renownandruin.com` and the compose `server` and `mmo-web` are stopped on the box. Until then `just deploy` is the only way to update the old public game; leave it working. Independent of Tasks 8-10.

**Files:**
- Delete recipes: `deploy`, `deploy-build`, `deploy-push`, `deploy-logs`, `orion-login-token-push`; variable `web_server` and its comment
- Delete recipe `web-release` **if** nothing but `deploy-build` calls it after this task (`world-images-build` runs `scripts/web_release.sh` directly) — run `grep -n 'web-release' justfile just scripts CLAUDE.md`; if kept, drop its `VOXEL_WEB_SERVER={{web_server}}`
- Delete: `deploy/web/` (`compose.yml`), `scripts/deploy_push.sh`
- Keep: `scripts/web_release.sh`, `scripts/wasm_externref_check.py` (`world-images-build` uses them), `web-check`, `tunnel` + `deploy_host` (the k3s API tunnel to the same box; check `grep -n deploy_host justfile` has no other user once the above are gone)
- Modify: `scripts/web_release.sh` — drop the `VOXEL_WEB_SERVER` input from its header and body
- Modify: `crates/client/src/frontend/mod.rs` + `crates/slint-ui/ui/…/main-menu.slint` (`JOIN_ONLY` Join-by-address → the Login screen is the page's only entry), `crates/client/src/session.rs` (`PendingServer`, `begin_join`), `crates/client/src/web_endpoint.rs` (`web_server_for`, `PinnedLocal`, `Trusted`, the build-time `VOXEL_WEB_SERVER`; `web_server_for_ticket` / `web_server_for_host` stay), `crates/server/src/webtransport.rs` (stop writing `assets/webtransport-cert-hash.json`; a ticket carries the hash) — line numbers from 2026-11-14 are stale after R5 Task 6-8: find by name
- Modify: `CLAUDE.md` — delete the "the OLD compose game on the box, `just deploy` (…)" sentence from Deployment and keep its binaryen rule on `world-images-build` ("`scripts/web_release.sh` — use the pinned binaryen under `target/tools`, never the distro's"); `TODO.md` ("the public web world" in the re-forge note)
- Not here: the box itself (compose stack, `/srv/mmo`, the proxy site, UDP 6466/6467 firewall rules) and the `orion.factoriosurvivalworld.com` record — R5 Phase D step 15 and the hosts plan's Task 6 own those

- [ ] **Step 1:** Confirm the gate: R5 Phase D step 15 is ticked in R5's plan / TODO.md.
- [ ] **Step 2:** Delete the recipes, `deploy/web/`, `scripts/deploy_push.sh`; trim `web_release.sh`. Run: `grep -rn -E '\bdeploy-(build|push|logs)\b|just deploy\b|deploy_push|deploy/web|web_server\b|VOXEL_WEB_SERVER|orion-login-token-push' justfile just scripts crates CLAUDE.md TODO.md deploy`. Expected: no output (k8s `deploy/web` references in `scripts/world_update.sh` / `twin_check.sh` are the Deployment named `web` — exclude them with `| grep -v 'deploy/web web\|rollout status deploy/web\|set image deploy/web\|image_of deploy/web'` and check what is left by eye).
- [ ] **Step 3:** Remove the join-by-address path in the client and the cert-hash file in the server. `VOXEL_BIND_ADDR` and `VOXEL_CLIENT_CERT` / `_KEY` stay — the world module sets them.
- [ ] **Step 4:** `just web-check && just slint-wasm && just client-wasm && just smoke`. Expected: all pass. `just world-images-build` still produces `target/web/`. The twin's page (`http://game.localhost:8480/` after `just twin-images twin-apply`) opens on Login.
- [ ] **Step 5:** Commit: `chore: remove just deploy — the world ships through R5's world-images-build / orion-world-push / orion-world-update`.

---

## Phase 4 — the justfile itself

### Task 12: Split into imported files

`import` (just ≥ 1.19; 1.42 here) keeps one flat recipe namespace and shares variables, so no recipe name changes and `just --list` reads as before.

**Files:**
- Modify: `justfile` → the dials (`adapter`, `world`, `view`, `user`, `region_blocks`, `chunk_blocks`, `island`, `cpu_percent`, `jobs`, `cap`), `_default`, `build`, `check`, `fmt*`, `clippy`, `clean*`, and the `import` lines. Target: under 200 lines.
- Create: `just/play.just` (run, run-two, play, *-release, the probes), `just/test.just` (smoke, test-*, determinism-wasm, slint-wasm, client-wasm, physics-next-upstream, pg-*, scripts-test, keepup-budget, soak), `just/world.just` (worldgen, forge-*, world-map, cross-section, history, worldctl, benches), `just/assets.just` (model-view, asset-viewer, creature-*, assets, *-textures, model-census), `just/perf.just` (diag, perf-*, trace-*, fps-steady, gpu-*, tracy-client, firefox-profile, render-bench), `just/web.just` (web-check, web-release, site-*, kc-*), `just/cluster.just` (twin-*, run-twin, world-*), `just/orion.just` (orion-*, tunnel)
- Modify: `CLAUDE.md` — "The root `justfile` is canonical" gains "(recipes live in `just/*.just`, imported)".

- [ ] **Step 1:** Before moving anything: `just --summary | tr ' ' '\n' | sort > $TMPDIR/recipes.before` and `just --evaluate > $TMPDIR/vars.before`.
- [ ] **Step 2:** Move recipes verbatim, comments with them; a variable used by one file only moves with it. While moving, fold a comment that restates the recipe below it (several recipes carry two doc paragraphs, e.g. `fly-probe` justfile:308-315) into one.
- [ ] **Step 3:** Verify nothing changed. Run: `just --summary | tr ' ' '\n' | sort | diff - $TMPDIR/recipes.before && just --evaluate | diff - $TMPDIR/vars.before && just --dry-run run >/dev/null && just smoke`. Expected: both diffs empty, smoke ok.
- [ ] **Step 4:** Commit: `refactor(just): split the justfile into imported files`.

### Task 13: Final sweep

- [ ] **Step 1:** Every recipe named in a doc exists. Run: `for r in $(grep -rhoE '\bjust [a-z][a-z0-9-]+' CLAUDE.md TODO.md crates scripts just justfile website docs/memories | awk '{print $2}' | sort -u); do just --summary | tr ' ' '\n' | grep -qx "$r" || echo "missing: $r"; done`. Expected: only justfile variable names used as `just name=value` (`adapter`, `world`, `view`, `user`, `region_blocks`, `chunk_blocks`, `island`, `cpu_percent`, `jobs`, `smoke_timeout`, `probe_features`).
- [ ] **Step 2:** Every script has a caller. Run: `for f in scripts/*.sh scripts/*.py; do grep -rqF "$(basename $f)" justfile just scripts crates deploy --exclude="$(basename $f)" || echo "orphan: $f"; done`. Expected: `scripts/perf_systems.py` only — give it a `perf-systems` recipe in `just/perf.just` or delete it; `scripts/orion_garage_import.sh` + `orion-garage-import` and `scripts/clean_old_worlds.sh` + `clean-old-worlds` are one-time migrations: delete both pairs once you confirm the Garage import and the world-store move are behind you.
- [ ] **Step 3:** Rewrite CLAUDE.md "Building and running" around the two modes — single player (`play`, `run`, `run-two`, `*-release`) and multiplayer (`twin-*`, `run-twin`, `world-*`; orion). Leave `docs/claude-md-full-reference-2026-11-08.md` alone (untracked, "not kept up to date" by its own description).
- [ ] **Step 4:** `just smoke`. Commit: `docs: CLAUDE.md describes two launch modes`.

## What is left when this is done

Recipes removed here: 28-29 of 165 (`build-stable`, 4 profile, 5 standalone server/client, 7 native multiplayer, `login` + 3 wrappers, `web`, 4 compose deploy + `orion-login-token-push` + possibly `web-release`) plus 4 private helpers — and 6 k0s recipes by R5; `scripts-test` and `run-twin` added. Scripts removed: 12 (the 2024 leftovers, `run.sh`, `build.sh`, `perf.sh`, `dev_r3.sh`, `deploy_push.sh`; + up to 3 one-shot migrations). Env switches removed: `VOXEL_SKIP_MENU`, `VOXEL_JWT_ADMISSION`, `VOXEL_AUTH=token`, `VOXEL_PEER_LINK_DELAY_MS`, `VOXEL_WEB_SERVER`. Directories removed: `deploy/web`, `scripts/provisioning`. Ways to start the game: `play` / `run` / `run-two` (single player) and `twin-*` + `run-twin` / orion (multiplayer).
