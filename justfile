# justfile — task runner for the voxel MMO workspace.
# Run `just` (or `just --list`) to see available recipes.
#
# Machine-specific bits are variables you can override on the command line, e.g.
#   just adapter=6700 client       # pin wgpu to the RX 6700 XT eGPU

# Cargo command used by all build/run/check recipes. Always plain `cargo`: the
# toolchain is pinned by rust-toolchain.toml (one stable toolchain for
# everything — no nightly, no Cranelift wrapper).
cargo := "cargo"

# Substring passed to WGPU_ADAPTER_NAME to select a GPU (empty = wgpu default).
# Defaults to $VOXEL_ADAPTER, so `export VOXEL_ADAPTER=Intel` (or 6700) in your
# shell profile pins every recipe for good. It matters on this box: unpinned,
# wgpu picks the NVIDIA T600 on the NVK driver, where the pulled-quad opaque
# pass measured 162 ms per frame against ~5 ms on the Intel iGPU (2026-09-20) —
# that, not the debug profile, is what makes a bare `just run` crawl.
adapter := env_var_or_default("VOXEL_ADAPTER", "")

# World size the client subscribes to (empty = production `full`). The server
# only generates what clients request, so this shrinks both sides:
#   tiny  = 1 simulated region, no LOD rings      (cheapest world that loads)
#   small = 1 simulated region + one factor-2 ring (24 LOD keys)
#   full  = 5x5 simulated regions + the rings beyond them (~1,060 LOD keys)
# e.g. `just world=tiny run`, `just world=small render-bench`. Finer control:
# export VOXEL_NEAR_RADIUS=0..3 / VOXEL_LOD_RINGS=off|small|full|"2:2,4:4"
# (see crates/client/src/world_scale.rs). Every recipe inherits it; tests
# ignore it (GameInstanceManager::new is env-free).
world := ""
export VOXEL_WORLD := world

# How far you see, in metres (empty = default: the LOD ring table's reach —
# 3,072 m at the play recipes' 256 m regions (624 LOD keys), 1 km at 64/128).
# Builds a ring table by the pixel-budget rule (`game::lod::ring_table_for`), and
# the exponential haze (visibility = 0.8 x reach) and camera far plane follow it:
# `just view=4000 run` for a cheap short view. An explicit VOXEL_LOD_RINGS wins.
# Haze alone: export VOXEL_FOG_VISIBILITY_M=M (2 % contrast distance; shorter =
# mistier). Browser: `?view=4000`.
view := env_var_or_default("VOXEL_VIEW_DISTANCE_M", "")
export VOXEL_VIEW_DISTANCE_M := view

# The player name on local (single-player / LAN / dev) servers — Factorio's
# model: no login, you are whoever you say (crates/client/src/player_name.rs).
# Empty = $VOXEL_PLAYER_NAME, else the saved name, else the OS user.
# `just user=bob run`; the play/run/client recipes export it as
# VOXEL_PLAYER_NAME; `web` prints the page URL with `?name=`. Online (login)
# servers ignore it.
user := ""

# World the server serves. Empty (default) = the default forged world,
# worlds/default.world, forged on first start if missing (ForgeParams::default():
# a 16 km / 32 m continent, ~5-10 s).
# `procedural` = the noise world. Any other value = that world file (e.g. one
# from `just forge-proto`, one finite world — by default a 16 km continent):
#   just forge-proto --seed D5C0DE021A --out target/world-forge/best
#   just world_file=target/world-forge/best.world server   # then `just client`
# Defaults to $VOXEL_WORLD_FILE. Relative paths resolve from the repo root.
# Tests never go through the server: procedural. The browser build only joins a server.
world_file := env_var_or_default("VOXEL_WORLD_FILE", "")
export VOXEL_WORLD_FILE := world_file

# Region edge in BLOCKS (1 block = 1 m) — a BUILD-time constant
# (`game::REGION_BLOCKS`), unlike `world`: 64 = 1024-unit / 64 m regions
# (default), 128 = 2048-unit, 256 = 4096-unit, 512 = 8192-unit regions, each
# with the ring table re-cut to the same pixel budget and horizon. Nothing else
# is legal (see crates/game/src/protocol.rs). The dial is in blocks
# so it keeps its meaning when the sim chunk (CHUNK_BLOCKS) changes size; the
# old chunk-denominated VOXEL_REGION_CHUNKS / VOXEL_CELL_CHUNKS and the removed
# cell mode's VOXEL_CELL_BLOCKS fail the build.
#   just region_blocks=128 run
# Switching recompiles `game` and everything downstream (not Bevy or the forks),
# and server + client MUST come from the same value — nothing refuses a
# mismatched pair. The default mirrors `[env]` in .cargo/config.toml so `just`
# and raw `cargo` share one build fingerprint. Per-region costs scale with
# AREA (x4 per doubling): compare perf numbers only within one region size.
#
# Two defaults: recipes that PLAY the game (server, client, run, *-release,
# web*, diag, perf-capture, fly-probe, shuttle-probe, play-test, the profilers
# trace-chrome / trace-join / perf / perf-client / perf-server / profile-* /
# tracy-client, and cross-section) build with
# `play_blocks` = 256 (256 m regions, one-region near window — see
# `DEFAULT_NEAR_RADIUS` in crates/client/src/world_scale.rs); everything else —
# tests, check, clippy, render-bench, raw cargo — stays at `test_blocks` = 64.
# An explicit `just region_blocks=N <recipe>` (or $VOXEL_REGION_BLOCKS) wins for
# every recipe.
region_blocks := env_var_or_default("VOXEL_REGION_BLOCKS", "")
test_blocks := if region_blocks == "" { "64" } else { region_blocks }
play_blocks := if region_blocks == "" { "256" } else { region_blocks }
export VOXEL_REGION_BLOCKS := test_blocks

# Sim-chunk edge in BLOCKS — a BUILD-time constant (`game::CHUNK_BLOCKS`,
# crates/game/src/block.rs; exactly 2 or 16 (the sizes with pinned
# goldens), anything else fails the build). A chunk owns one entity, one rapier body and one collider set; the
# 32³-voxel render/LOD brick does not change with it. The default mirrors
# `[env]` in .cargo/config.toml (one build fingerprint); switching recompiles
# `game` and everything downstream. Server and client must share the value.
# Default 16 (16 m chunks); 2 is the pre-2026-10-29 size, still tested:
#   just chunk_blocks=2 smoke
chunk_blocks := env_var_or_default("VOXEL_CHUNK_BLOCKS", "16")
export VOXEL_CHUNK_BLOCKS := chunk_blocks

# Spawn island size (phase 3 of the island spec): the island centred on the home
# region, as a diameter in 64 m cells — a BUILD-time constant
# (`worldgen::islands::SPAWN_ISLAND_CELLS`), 0 = the default 16 (1 km), at most
# `SPAWN_ISLAND_CELLS_MAX`. The rest of the world is quadtree-placed islands in
# open sea.
#   just island=8 run     (a 512 m island)
island := env_var_or_default("VOXEL_ISLAND_CELLS", "0")
export VOXEL_ISLAND_CELLS := island

# GPU sampled by `just gpu-log` (domain:bus:dev.func). Default is the RX 6700 XT eGPU;
# `amdgpu_top --list` prints the ids.
pci := "0000:40:00.0"

# perf sampling flags shared by the perf-client / perf-server recipes.
perf_flags := "-F9997 --call-graph dwarf,16384 -e cpu-clock"

# Share of logical cores that builds and test runs may use, so a compile or a
# test run never takes the whole machine. `jobs` is the resolved count; override
# either one: `just cpu_percent=50 build`, `just jobs=4 test`.
cpu_percent := "80"
jobs := shell('n=$(( $(nproc) * $1 / 100 )); echo $(( n > 0 ? n : 1 ))', cpu_percent)

# Every recipe inherits these: cargo reads
# CARGO_BUILD_JOBS for rustc parallelism, libtest reads RUST_TEST_THREADS for
# test threads (an explicit `--test-threads`, as in `test-full`, still wins).
# Raw `cargo` outside `just` is capped by `[build] jobs` in .cargo/config.toml.
export CARGO_BUILD_JOBS := jobs
export RUST_TEST_THREADS := jobs

# The job count alone does NOT cap CPU: each rustc runs multi-threaded codegen
# (100-200% apiece), and test binaries spawn their own pools (Bevy tasks, region
# threads), so 16 jobs still saturated all 20 cores (measured 2026-09-19). `cap`
# is the real limit — it pins the whole process tree to the first `jobs` cores
# and lowers its priority, leaving the rest free for the desktop. Prefixed to
# every build/test/check/clippy invocation; deliberately NOT to `cargo run`, so
# the game itself is never pinned or niced. Empty where `taskset` is missing.
cap := shell('if command -v taskset >/dev/null 2>&1; then echo "nice -n 10 taskset -c 0-$(( $1 - 1 ))"; fi', jobs)

# Full-suite concurrency measured 2026-07-15: three threads completed in
# ~8.3s / ~238 MiB RSS warm, while Cargo's 20-thread default took ~24.8s and
# peaked near 5.3 GiB due to Bevy/meshing contention.
test_threads := "3"

# Server address (informational; hardcoded to 127.0.0.1:6466 in the server).
_default:
    @just --list


# Dev Postgres for the world store (deploy/dev/compose.yml, `just pg-up`). The
# database `mmo`; tests use the maintenance database and create their own.
dev_database_url := "postgres://mmo:mmo@127.0.0.1:55432/mmo"

# ---- Build ----------------------------------------------------------------

# Build the whole workspace (all bins) with the configured cargo (stable by default).
build:
    {{cap}} {{cargo}} build --workspace --bins

# Build with stable cargo always, ignoring any Cranelift override.
build-stable:
    {{cap}} {{cargo}} build --workspace --bins

# Type-check the whole workspace without producing binaries.
check:
    {{cap}} {{cargo}} check --workspace

# ---- Run ------------------------------------------------------------------

# Run the QUIC server (listens on 127.0.0.1:6466, WebTransport ingress on :6467).
server:
    VOXEL_REGION_BLOCKS={{play_blocks}} RUST_BACKTRACE=1 {{cargo}} run --bin server

# Wipe the dev worldgen disk cache (target/worldgen-cache). The cache
# self-invalidates when crates/worldgen or the block manifest change; use this
# after a change elsewhere (e.g. column encoding in `game`) alters generated
# output without touching those inputs. WORLDGEN_CACHE=0/1 forces it off/on.
worldgen-cache-clear:
    rm -rf target/worldgen-cache

# Run the Bevy client. Override `adapter` to pin a GPU, e.g. `just adapter=6700 client`.
client:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_SKIP_MENU=1
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    if [ -n "{{user}}" ]; then export VOXEL_PLAYER_NAME="{{user}}"; fi
    RUST_BACKTRACE=1 {{cargo}} run --bin client

# Debug launch: straight into a small generated world, adventuring at the first populated site
# (`client debug world`, crates/client/src/debug_launch.rs; `--site N` picks the site). The world is
# worlds/debug-world/ (16 km / 32 m, forged on first run) and is REUSED, with its player,
# on every later run; `just run --reset` (or deleting the directory) re-forges it and starts
# afresh. `just run --reset --prefab dev-hut`
# stamps a prefab beside the landing of the adventure site's region only (needs it never saved, so `--reset`; more
# `--prefab`s join with `;`); `VOXEL_PREFAB_DIRS=prefab-imports/statuen just run --reset --prefab "statuen-06;statuen-07"`
# adds a prefab directory. Builds both binaries — the client spawns the world's server itself,
# like a menu start.
run *args:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    if [ -n "{{user}}" ]; then export VOXEL_PLAYER_NAME="{{user}}"; fi
    {{cap}} {{cargo}} build --bin client --bin server
    RUST_BACKTRACE=1 ./target/debug/client debug world {{args}}

# A town to stand in: `run --reset --town <style>` with every sub-directory of `prefab-imports/` added
# to `VOXEL_PREFAB_DIRS` (the imported packs hold the houses tagged with a style), e.g.
# `just town medieval` or `just town medieval:2:64` (STYLE[:ARMS[:LEN]]). One town of that style is laid
# out beside the landing of the adventure site's region (the spawn region a fresh session waits in before
# the adventure starts is skipped). It passes --reset: it DELETES and re-forges `worlds/debug-world/`, the world
# `just run` reuses. Builds both binaries like `run`; more `args` follow.
town style *args:
    #!/usr/bin/env bash
    set -euo pipefail
    dirs=""
    if [ -d prefab-imports ]; then
        dirs=$(find prefab-imports -mindepth 1 -maxdepth 1 -type d | sort | paste -sd:)
    fi
    if [ -z "$dirs" ]; then
        echo "no prefab packs: prefab-imports/ has no sub-directories (the houses of a town come from imported prefabs)" >&2
        exit 1
    fi
    export VOXEL_PREFAB_DIRS="$dirs"
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    if [ -n "{{user}}" ]; then export VOXEL_PLAYER_NAME="{{user}}"; fi
    {{cap}} {{cargo}} build --bin client --bin server
    RUST_BACKTRACE=1 ./target/debug/client debug world --reset --town {{style}} {{args}}

run_two_port := "6476"

# `run` with TWO clients on one server: two players adventuring at one site. Client 1 is the
# debug launch, hosting the world's server on UDP `run_two_port` (`--port`); once its
# adventure has started (it logs `[debug] playing: adventure site=N`), client 2 joins
# (`--join`) and starts an adventure at that site (`--site N`). Output is prefixed
# [client 1] / [client 2] (client 1's also in target/run-two/client1.log); the
# server log is worlds/debug-world/server.log. Closing client 1 ends both
# (its server is single-player: it exits with its host). `just run-two --reset`
# re-forges the world first.
run-two *args:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    if [ -n "{{user}}" ]; then export VOXEL_PLAYER_NAME="{{user}}"; fi
    {{cap}} {{cargo}} build --bin client --bin server
    port={{run_two_port}}
    if [ -n "$(ss -Huln "sport = :$port")" ]; then
        echo "UDP port $port is already in use; pick another: just run_two_port=N run-two" >&2
        exit 1
    fi
    mkdir -p target/run-two
    log=target/run-two/client1.log
    : > "$log"
    pids=()
    trap 'kill "${pids[@]}" 2>/dev/null || true' EXIT
    RUST_BACKTRACE=1 ./target/debug/client debug world --port "$port" {{args}} \
        > >(tee "$log" | sed -u 's/^/[client 1] /') 2>&1 &
    host=$!; pids+=("$host")
    # The world may be forged first, then client 1 starts its adventure: wait for its site.
    site=""
    until [ -n "$site" ]; do
        kill -0 "$host" 2>/dev/null || exit 0
        sleep 0.5
        site=$(grep -aoE 'playing: adventure site=[0-9]+' "$log" | head -n1 | grep -oE '[0-9]+$' || true)
    done
    echo "[run-two] client 1 adventures at site $site; client 2 adventures there"
    # Its own player name: with client 1's, the server would see client 1's player.
    VOXEL_PLAYER_NAME="${VOXEL_PLAYER_NAME:-${USER:-player}}-2" RUST_BACKTRACE=1 \
        ./target/debug/client debug world --join "127.0.0.1:$port" --site "$site" \
        > >(sed -u 's/^/[client 2] /') 2>&1 &
    pids+=("$!")
    wait "$host" || true

# The game with its main menu: build client + server (the client spawns the
# server per world from next to its own executable, so both need the same
# region/chunk dials) and run only the client. `just adapter=6700 play`.
# New world's generator defaults to a 100 km / 16 m world: its Play forge runs
# in-process for minutes and several GB (the estimate shows beside Play);
# pick 16 km or a 32 m World cell for a quick one.
play:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    if [ -n "{{user}}" ]; then export VOXEL_PLAYER_NAME="{{user}}"; fi
    {{cap}} {{cargo}} build --bin client --bin server
    RUST_BACKTRACE=1 ./target/debug/client

# Same as `play`, release mode.
play-release:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    if [ -n "{{user}}" ]; then export VOXEL_PLAYER_NAME="{{user}}"; fi
    {{cap}} {{cargo}} build --release --bin client --bin server
    RUST_BACKTRACE=1 ./target/release/client

# Release-mode variants: production performance. Per-entry undo verification is
# Off in every build (the server's every-100-ticks state hash still catches
# desyncs); set HASH_MODE=full (or delta) on any recipe to turn it on,
# e.g. `HASH_MODE=full just run` when chasing a footprint/capture gap. Use
# perf-client / perf-server when you also want a profile out of the run.

# Run the QUIC server in release mode.
server-release:
    VOXEL_REGION_BLOCKS={{play_blocks}} RUST_BACKTRACE=1 {{cargo}} run --release --bin server

# Run the Bevy client in release mode.
client-release:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_SKIP_MENU=1
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    if [ -n "{{user}}" ]; then export VOXEL_PLAYER_NAME="{{user}}"; fi
    RUST_BACKTRACE=1 {{cargo}} run --release --bin client

# `run` in release mode: the debug world (`just run-release --reset`
# starts it over).
run-release *args:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    if [ -n "{{user}}" ]; then export VOXEL_PLAYER_NAME="{{user}}"; fi
    {{cap}} {{cargo}} build --release --bin client --bin server
    RUST_BACKTRACE=1 ./target/release/client debug world {{args}}

# Scripted flight across region seams (release): server + client, the client
# flying a straight line under VOXEL_AUTOPILOT once the world is ready, closing
# itself after `seconds` of cruise (a seam every ~3 s at fly-sprint). Logs go to
# target/fly-probe/; prints one pair of lines per crossing — game thread
# (`region_crossing`: tick gaps, stalled/aligned home ticks, corrections,
# snapshot loads) and render (`region_crossing_render`: worst frame, longest
# sim-pose hold) — plus any main-thread stalls, and the crossing timeline
# (`crossing_start` / `crossing_step` / `crossing_done` from the render thread,
# `crossing_flip` / `window_shift` / `window_loaded` / `lod_shift*` from the game
# thread: every end point in ms since the start, and what reached or left the
# GPU). e.g. `just adapter=6700 fly-probe 60`.
# Scripted straight-line flight across region seams, one report per crossing.
# Extra client features for fly-probe / shuttle-probe builds, e.g.
# `just probe_features=gpu_error_panic shuttle-probe` (panic at the failing
# Vulkan call on a device loss).
probe_features := ""

# Builds whose features change Bevy's or wgpu's OWN feature set (trace_chrome,
# tracy, pyroscope, probe features) are a whole second copy of the dependency
# tree. They build into their own target dir, one per feature set, so the main
# target/ never accumulates them and `just clean-variants` drops them all.
variant_dir := "target/variants"
probe_dir := if probe_features == "" { "target" } else { variant_dir / replace(probe_features, ",", "+") }

# Delete every feature-variant build (trace_chrome, tracy, pyroscope, probes).
clean-variants:
    rm -rf {{variant_dir}}

fly-probe seconds="45":
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_SKIP_MENU=1
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    out="target/fly-probe"; mkdir -p "$out"
    {{cap}} {{cargo}} build --release --bin server
    {{cap}} {{cargo}} build --release --bin client --features "{{probe_features}}" --target-dir {{probe_dir}}
    RUST_BACKTRACE=1 ./target/release/server > "$out/server.log" 2>&1 &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    VOXEL_AUTOPILOT=1 VOXEL_AUTOPILOT_SECONDS={{seconds}} RUST_BACKTRACE=1 \
        ./{{probe_dir}}/release/client > "$out/client.log" 2>&1 || true
    grep -h "AdapterInfo" "$out/client.log" | head -1 || true
    grep -hE "event=(world_ready|autopilot_start|region_crossing|region_crossing_render|main_stall|bridge_region_spawned|region_loaded|crossing_[a-z]+|window_shift|window_loaded|lod_shift[a-z_]*)" \
        "$out/client.log" | sed -n '/autopilot_start/,$p' || true
    echo
    scripts/crossing_summary.py "$out/client.log"
    echo "logs: $out/client.log $out/server.log"

# The same benchmark for crossing ONE boundary back and forth, fast: the client
# flies to the first boundary, then reverses `overshoot` metres past it each leg
# (VOXEL_AUTOPILOT=shuttle). `boundary=region` (default) shuttles across the seam
# between two region threads — every leg is a handoff and a near-window shift
# that releases and re-requests a row; `boundary=chunk` shuttles across a sim-chunk
# edge (CHUNK_UNITS, 256 units at 16-block chunks) inside one region (no handoff; PERF_LOG=1 adds the per-chunk-crossing
# `[client-motion]` lines). Ends with the `scripts/crossing_summary.py` table;
# logs in target/shuttle-probe/. e.g. `just adapter=6700 shuttle-probe chunk 30 0.5`.
# Back and forth across one region seam (or chunk edge), one report per crossing.
shuttle-probe boundary="region" seconds="45" overshoot="":
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_SKIP_MENU=1
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    if [ -n "{{overshoot}}" ]; then export VOXEL_AUTOPILOT_OVERSHOOT_M="{{overshoot}}"; fi
    out="target/shuttle-probe"; mkdir -p "$out"
    {{cap}} {{cargo}} build --release --bin server
    {{cap}} {{cargo}} build --release --bin client --features "{{probe_features}}" --target-dir {{probe_dir}}
    RUST_BACKTRACE=1 ./target/release/server > "$out/server.log" 2>&1 &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    VOXEL_AUTOPILOT=shuttle VOXEL_AUTOPILOT_BOUNDARY={{boundary}} VOXEL_AUTOPILOT_SECONDS={{seconds}} \
        PERF_LOG=1 RUST_BACKTRACE=1 ./{{probe_dir}}/release/client > "$out/client.log" 2>&1 || true
    grep -h "AdapterInfo" "$out/client.log" | head -1 || true
    scripts/crossing_summary.py "$out/client.log"
    echo "logs: $out/client.log $out/server.log"

# Interactive play-test: build, run server + client, and tee both to
# target/play-test/*.log so the run can be reviewed for errors afterwards.
# Close the client window (or Ctrl-C) to stop both; on exit it prints a
# panic/error summary and the log paths.
play-test:
    #!/usr/bin/env bash
    set -uo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_SKIP_MENU=1
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    log_dir="target/play-test"
    mkdir -p "$log_dir"
    srv_log="$log_dir/server.log"; cli_log="$log_dir/client.log"
    : > "$srv_log"; : > "$cli_log"

    echo ">> building workspace..."
    if ! {{cap}} {{cargo}} build --workspace --bins 2>&1 | tee "$log_dir/build.log"; then
        echo "!! BUILD FAILED — see $log_dir/build.log" >&2
        exit 1
    fi

    # Kill a PID and all its descendants (TERM, then KILL any survivors). Needed
    # because `cargo run` does NOT forward signals to the binary it spawns, so
    # killing cargo alone would orphan the server.
    kill_tree() {
        local pid=$1 sig=${2:-TERM} child
        for child in $(pgrep -P "$pid" 2>/dev/null); do kill_tree "$child" "$sig"; done
        kill "-$sig" "$pid" 2>/dev/null || true
    }

    echo ">> starting server -> $srv_log"
    # Process substitution (not a pipe) so $! is cargo's PID, not tee's.
    RUST_BACKTRACE=1 {{cargo}} run --bin server > >(tee "$srv_log") 2>&1 &
    srv_pid=$!
    stop_server() {
        kill_tree "$srv_pid" TERM
        for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$srv_pid" 2>/dev/null || return 0; sleep 0.3; done
        kill_tree "$srv_pid" KILL
    }
    trap stop_server EXIT INT TERM
    sleep 1

    echo ">> starting client -> $cli_log   (close the window or Ctrl-C to finish)"
    RUST_BACKTRACE=1 {{cargo}} run --bin client 2>&1 | tee "$cli_log"
    cli_rc=${PIPESTATUS[0]}

    echo ">> client exited (code $cli_rc) — stopping server"
    stop_server
    wait 2>/dev/null || true

    echo
    echo "=== play-test finished (client exit=$cli_rc) ==="
    echo "logs: $srv_log  $cli_log"
    if grep -n -iE 'panic|thread .* panicked|error\[|ERROR' "$srv_log" "$cli_log" >/dev/null 2>&1; then
        echo "--- possible errors (grep panic|error) ---"
        grep -n -iE 'panic|thread .* panicked|error\[|ERROR' "$srv_log" "$cli_log" || true
    else
        echo "no panics/errors matched in logs."
    fi

# Run the deterministic worldgen binary.
worldgen:
    {{cargo}} run --bin worldgen

# Time full region generation (coarse-find + voxel-smooth surfaces)
worldgen-bench:
    {{cargo}} run --release -p worldgen --bin worldgen

# Region start time + server RSS per running region, on real region threads
# (no client, no LOD): `just genesis-probe 9 30 -9,-6` starts the 3x3 around
# region (-9,-6), keeps it ticking 30 s and prints RSS. Builds at the play
# region size; serves `world_file` if set (e.g. worlds/debug-world/terrain.world).
# `people=N` spawns N people in the center region and prints its `[region …] avg= max=`
# tick metrics every 5 s: `just genesis-probe 1 30 0,0 80` = resident cost on real terrain.
genesis-probe regions="1" hold="0" center="0,0" people="0":
    VOXEL_REGION_BLOCKS={{play_blocks}} {{cap}} {{cargo}} build --release -p server --example genesis_probe
    VOXEL_WORLD_FILE="{{world_file}}" ./target/release/examples/genesis_probe {{regions}} {{hold}} {{center}} {{people}}

# Time one region's collider genesis build + determinism check.
collider-bench:
    {{cargo}} run --release -p worldgen --example collider_profile

# Micro-benchmark of TerrainPipeline::column and its share of generate_region.
bench-column:
    {{cargo}} run --release -p worldgen --example column_bench

# Look at a Blockbench model (crates/client/examples/model_view.rs): path relative
# to the repo root; it turns on a floor, hot-reloads on save; `--shot out.png`
# screenshots and exits. e.g.
#   just model-view assets/models/soldier.bbmodel
model-view path *args:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    {{cap}} {{cargo}} build --bin client --bin server --example model_view
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    ./target/debug/examples/model_view {{path}} {{args}}

# Browse every Blockbench model (crates/client/examples/asset_viewer.rs): all
# .bbmodel files listed on the left, the picked one's animations on the right
# (play, scrub, pause, speed); drag orbits, wheel zooms, saves hot-reload
# (polled). The viewers build exactly what `just run` builds (dev profile, play
# region size, client + server selected so features unify the same) plus the
# example, so they reuse its artifacts instead of a second copy in target/.
# Roots default to assets/ + the bbmodel fixtures. `--model <part of path>
# [--anim NAME] [--time S] --shot out.png` screenshots and exits. e.g.
#   just asset-viewer
#   just asset-viewer assets/models
asset-viewer *args:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    {{cap}} {{cargo}} build --bin client --bin server --example asset_viewer
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    ./target/debug/examples/asset_viewer {{args}}

# Creature models (docs/superpowers/specs/2026-10-29-creature-asset-pipeline.md):
# tools/creatures/src/<df_id>.toml → assets/models/creatures/<df_id>.bbmodel +
# the manifest creatures.json. No ids: every source. Then `just creature-check`.
creatures *ids:
    python3 tools/creatures/build.py {{ids}}

# Sources for giants and animal people whose base creature is built
# (tools/creatures/derive.py); existing ones are kept unless --force.
creature-derive *args:
    python3 tools/creatures/derive.py {{args}}

# A creature's brief from the DF roster + local raws: size, template, clips.
#   just creature-brief DOG
#   just creature-brief "giant cave spider"
creature-brief id:
    python3 tools/creatures/brief.py "{{id}}"

# A creature's review contact sheet: every clip, top-down and side views, in
# target/creature-sheets/<id>.png (builds the viewer first).
creature-sheet id:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    {{cap}} {{cargo}} build --bin client --bin server --example asset_viewer
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    python3 tools/creatures/sheet.py {{id}}

# Creature models done (approved / draft / todo per category) and the next to build.
creature-progress *args:
    python3 tools/creatures/progress.py {{args}}

# The creature lint: every manifest model through the real parser (no reader
# warnings, required clips + loop modes, budgets, rest bounds).
creature-check:
    {{cap}} {{cargo}} test -q -p bbmodel --test it creatures::

# Game models from a text prompt (tools/assets/generate.py): a Leonardo.ai picture,
# then 3D AI Studio TRELLIS.2 image-to-3D -> assets/models/<name>.glb. Costs
# credits; keys in the git-ignored repo-root .env; downloads kept in
# tools/assets/raw/ and never asked for twice. e.g.
#   just assets --list | --balance | --only archer --picture-only
assets *args:
    python3 tools/assets/generate.py {{args}}

# Regenerate the tree block textures (oak/pine/birch bark, rings, branch,
# leaves) into assets/blocks/ with Pillow; deterministic. If you add PNGs,
# also add them to EMBEDDED_BLOCK_TEXTURES in crates/client/src/renderer/mod.rs.
tree-textures:
    python3 tools/textures/tree_textures.py

# Regenerate the biome / ice / tree-species block textures (blocks.ron ids 22+;
# most derived from existing PNGs by a colour ramp) into assets/blocks/ with
# Pillow; deterministic. `--sheet` also writes target/textures-sheet.png. New
# PNGs also go in EMBEDDED_BLOCK_TEXTURES (crates/client/src/renderer/mod.rs).
block-textures *args:
    python3 tools/textures/gen.py {{args}}

# Render a prefab (a name under assets/prefabs/ or a file path) as a PNG: plan,
# south and east elevation. `just prefab-view dev-hut [-o out.png] [--scale N]`;
# default output target/prefab-view/<name>.png.
prefab-view *args:
    {{cap}} {{cargo}} run --release -p prefab-tool -- view {{args}}

# A top-down picture of a town layout from the prefab library: `just town-preview medieval --seed 1
# [--terrain seed] [--dirs a:b] [--arms N] [--len N]`; default output target/prefab-view/town-<style>-<seed>.png.
town-preview *args:
    {{cap}} {{cargo}} run --release -p prefab-tool -- town {{args}}

# Import Minecraft Sponge schematics as prefabs (run from the repo root): `just mc-import report <f.schem>...`
# (what is in it, writes nothing), `import <f.schem> [--name N]` (whole file) or `split <f.schem> [--prefix P]`
# (one prefab per build); output goes to prefab-imports/<file stem>/ (git-ignored, never assets/).
mc-import *args:
    {{cap}} {{cargo}} run --release -p mc-import-tool -- {{args}}

# Validate the prefab set as the game loads it (default assets/prefabs): one line
# per prefab, exit 1 on the first error. `just prefab-check [dir]`.
prefab-check *args:
    {{cap}} {{cargo}} run --release -p prefab-tool -- check {{args}}

# Top-down PNG map of the generated world: colour relief + 16-bit heightmap +
# terrain stats, into target/world-map/. Flags: --span <m> --size <px>
# --center <x,z> --out <path stem> --seed <hex> --relief <factor> --world <file>
# --layer rain|temperature|moisture|salinity (extra habitat PNG; with --world
# also coloured biome|temperature|precipitation|drainage|regions from the forged
# world's rasters, `<out>-<layer>.png`), e.g.
# `just world-map --span 65536 --size 2048`. `--world <file> --tiles` writes the
# in-game world map instead (every map layer, `<out>-tiles-<layer>.png`) and
# prints the WorldMap packet size; pass `region_blocks=256` for the play tiling.
world-map *args:
    {{cap}} {{cargo}} run --release -p worldgen --example world_map -- {{args}}

# Vertical slice of the generated ground (soil, forge rock layers, bedrock) as a
# PNG in target/cross-section/, read through the engine's own Column/ColumnGround.
# Flags: --world <file>|procedural --region X,Z --axis x|z --at <block> --out <png>;
# without --world it uses worlds/default.world, and a missing file is an error
# (export one: just worldctl export --db <store> <out.world>). Built with
# `play_blocks`, so --region / --at address the same grid as `just run`.
cross-section *args:
    VOXEL_REGION_BLOCKS={{play_blocks}} {{cap}} {{cargo}} run --release -p worldgen --example cross_section -- {{args}}

# Region cost and walkability of generated terrain (test-size region, procedural
# world): snapshot / chisel / collider sizes, slopes by shape and how many
# one-block rises are walkable without a jump. Args: [rx rz] (default 0 0).
quantize-probe *args:
    {{cap}} {{cargo}} run --release -p worldgen --example quantize_probe -- {{args}}

# Flags: --world <file> --region X,Z; IMPLIED_AIR=1 counts the ground under a
# column's band as unknown. Built with `play_blocks`, like cross-section.
# Render-brick census of a region: entities spawned, bricks with faces, buried ones
brick-census *args:
    VOXEL_REGION_BLOCKS={{play_blocks}} {{cap}} {{cargo}} run --release -p worldgen --example brick_census -- {{args}}

# World history: bootstrap + fast-forward a forged world's history (people, sites,
# civs, journeys; `game::history`) and print statistics and the log. `--save` writes
# <world>.history next to the world file (refuses to overwrite without `--force`),
# `--dump <file>` reads one instead. Flags: --world --civs --years --founders --seed
# --log --save --force --dump. Built with `play_blocks` like `cross-section`.
history *args:
    VOXEL_REGION_BLOCKS={{play_blocks}} {{cap}} {{cargo}} run --release -p worldgen --example history -- {{args}}

# Pre-store save files under worlds/ (*.regions/, *.players/, *.history, *.history.bak,
# *.region.bak) — the ones the world store (terrain.sqlite) replaces. Lists them with
# sizes; `--yes` deletes them. Never touches *.world, world.json, preview.png or *.sqlite*.
clean-old-worlds *args:
    scripts/clean_old_worlds.sh {{args}}

# World-store maintenance (`crates/world-store/src/bin/worldctl.rs`):
#   just worldctl forge --db <url> [--preset <name>] [--size <m>] [--cell <m>] [--seed <hex>] [--set <knob>=<value>]…
#     forges a world in process and stores it as tiles (a store that already
#     holds a world is refused); flags as `just forge-proto` takes them;
#   just worldctl retile --db <url>
#     rebuilds the tiles from the archived forge file (after a tile-format change);
#   just worldctl export --db <url> <out.world>
#     writes the stored forge file back out, for the file-based tools
#     (`world-map`, `cross-section`, `history`, `genesis-probe`);
#   just worldctl hosts <url>
#     prints one line per host row of a Postgres world (id, name, state, clean, land, seen, drain);
#   just worldctl clock-reset <url>
#     clears the world clock anchor (`dev-r3` runs it before every start);
#   just worldctl super-region <url> <n>
#     sets the super-region side (regions per side) of a world no host has claimed
#     yet; its hosts and coordinator then run with VOXEL_SUPER_REGION_REGIONS=<n>;
#   just worldctl drain <url> <host id> [--in <secs>]
#     marks a live host draining (default 300 s, at most 7 days; a mark only moves earlier):
#     it warns its players, leaves through a leave cut at the deadline and stays up unready;
#   just worldctl retire-host <url> <host id>
#     (claim ownership, Postgres) retires a host that will never return (the id as `hosts`
#     lists it): refused while it beats ("stop it first"); with other hosts alive it only
#     vouches for its death (the coordinator declares it, a running host inherits); with none
#     alive it opens the succession itself — the lowest other live-row id is the heir;
#   just worldctl rollback <url> --yes
#     (Postgres) restarts the world from its last save: the generation goes up, open
#     successions are abandoned, every running host exits 5 at its next save.
# <url> = a .sqlite path or a postgres:// URL. A store a running server holds is
# refused (its lock), never waited for. Built at `play_blocks`: the store's world
# row and its world map are tied to the region size (pass region_blocks=N for others).
# Pinned to `cap` cores ON PURPOSE, like forge-proto: the forge and the tile encoding
# use every core they get, and this is batch work beside a desktop or a server.
# World-store maintenance: forge a world into a store, export its forge file.
worldctl *args:
    VOXEL_REGION_BLOCKS={{play_blocks}} {{cap}} {{cargo}} run --release -p world-store --bin worldctl -- {{args}}

# The host rows of the dev Postgres world (`just worldctl hosts <url>`): one line per host with its id
# (issued by the store), name, state, clean flag, land, last beat and drain flag. Run while `dev-r3` /
# `dev-r2` is up, or after it. Needs `pg-up`.
world-hosts-dev:
    just worldctl hosts {{dev_database_url}}

# Mark a host of the dev Postgres world draining (`just worldctl drain <url> <id> --in <secs>`): it
# wins no new land, is not the gate for logins and is to leave within `secs` (default 300, the
# game's DRAIN_WARNING_SECS). The id is the number `world-hosts-dev` lists. A mark only moves
# earlier. Needs `pg-up`.
drain-dev id secs="300":
    just worldctl drain {{dev_database_url}} {{id}} --in {{secs}}

# Forge a world straight into a store: `just forge-db worlds/w.sqlite --size 32768 --cell 16`.
forge-db db *args:
    just region_blocks={{play_blocks}} worldctl forge --db {{db}} {{args}}

# Front / side / top orthographic PNGs of one generated tree (one pixel per
# voxel, coloured by part) into target/tree-view/. Flags: --species
# oak|pine|birch|spruce|acacia|palm|jungle|cactus --seed N --age N (0..=100) --out <path stem> --scale N; --grid
# writes 3×3 sheets (front + top) of seeds N..N+8 instead; --time N prints the
# skeleton + rasterise cost per tree over N seeds.
tree-view *args:
    {{cap}} {{cargo}} run --release -p worldgen --example tree_view -- {{args}}

# Feature census of Blockbench/Bedrock model files (.bbmodel, .geo.json,
# .animation.json): which format features each file uses, so a model loader
# knows what it must handle. Defaults to the local test corpus in
# target/test-models/ (gitignored; see its MANIFEST.tsv for sources/licences).
model-census *paths="target/test-models":
    scripts/bbmodel_census.py {{paths}}

# World forge prototype (F1): simulate a whole finite world offline (default: a
# 16 km continent at 32 m cells, ~5 s; `--set layout=0` for the one island) —
# geology-controlled stream-power erosion, orographic rain, depression filling,
# planed valley floors, spill-level lakes — and write relief / rivers / geology
# / rain maps, a 16-bit heightmap, river+lake objects (JSON), drainage
# statistics and the WORLD FILE `<stem>.world` into target/world-forge/ (serve it
# with `just world_file=<path> server` + `just client`). Never changes procedural output. Flags:
# --cell <m> --iters <n> --seed <hex> --out <stem> --size --dt --uplift --k
# --diffusivity --no-geology --no-climate, --preset <name> (a map-generator preset
# from forge/presets.rs, e.g. "Frozen north"; give it before any --set),
# --set <knob>=<value>, and --search <n>
# to forge n seeds at 32 m cells and rank them (generate-and-test). e.g.
# `just forge-proto --cell 16` for the full-detail world, `just forge-proto
# --cell 64 --size 100352` for a quick look at a ~100 km continent,
# `just forge-proto --search 64` (ranks seeds by `forge::quality::world_score`:
# land share, snow peaks, mountain/forest regions, a Strahler-4 river, 10+ biomes,
# a glacier under a pole, regions under the cap). Writes -biome/-temperature/
# -drainage/-regions PNGs next to the relief ones.
forge-proto *args:
    {{cap}} {{cargo}} run --release -p worldgen --example forge_proto -- {{args}}

# Water prototype (prototypes/water/, standalone, no engine deps): bodies + frontier
# CA vs cell-only CA. Plan: docs/superpowers/plans/2026-09-22-water-prototype.md.
water-proto-test *args:
    cd prototypes/water && {{cap}} cargo test --release {{args}}

# Scenario table for both water models (a TIMING run: ask before running on the shared box).
# `just water-proto --golden` prints only final state hashes (not a timing run).
water-proto *args:
    cd prototypes/water && {{cap}} cargo build --release --quiet --bin water_proto
    cd prototypes/water && target/release/water_proto {{args}}

# The same scenarios compiled to wasm32 and run under node; hashes must equal the native ones.
water-proto-wasm:
    cd prototypes/water && {{cap}} cargo build --release --lib --target wasm32-unknown-unknown
    cd prototypes/water && PATH=$HOME/.nvm/versions/node/v25.0.0/bin:$PATH node wasm_golden.mjs target/wasm32-unknown-unknown/release/water_proto.wasm > target/golden-wasm.txt
    cd prototypes/water && {{cap}} cargo run --release --quiet --bin water_proto -- --golden > target/golden-native.txt
    cd prototypes/water && diff target/golden-native.txt target/golden-wasm.txt && echo "golden hashes match (native == wasm32)"

# Forge speed benchmark: one forge at `size` m / `cell` m, printing wall time and
# max RSS (needs /usr/bin/time). Output goes to target/forge-bench/. Timing
# run: ask before launching the default 16 km / 16 m. e.g. `just forge-bench 4096 64`.
forge-bench size="16000" cell="16":
    mkdir -p target/forge-bench
    {{cap}} {{cargo}} build --release -p worldgen --example forge_proto
    /usr/bin/time -f 'forge-bench: wall %e s, max RSS %M KiB' target/release/examples/forge_proto --size {{size}} --cell {{cell}} --out target/forge-bench/w

# Tuning page for the world forge: every knob (crates/worldgen/src/forge/knobs.rs) as
# a slider with a live map preview, on http://127.0.0.1:<port>/. A change
# re-forges at a coarse preview cell size (64 m is about a second); "Save world
# file" forges at 16 m and writes a .world the game can load; "Copy as flags"
# gives the matching `just forge-proto --set …` line. Extra args are forge flags,
# e.g. `just forge-ui --seed D5C0DE021A`. Localhost only.
forge-ui port="8787" *args:
    {{cap}} {{cargo}} run --release -p worldgen --example forge_proto -- --serve {{port}} {{args}}

# Headless region-load cost breakdown (gen/derive/serde/mesh).
bench-region:
    {{cap}} {{cargo}} test -p worldgen --release --test region_load_cost -- --ignored --nocapture

# Self-screenshot probe: the pulled-quad render path draws real terrain. Run it
# on a real adapter after a Bevy bump (`just adapter=6700 pull-smoke`) — the
# shader tests compose against stubs and miss upstream WGSL changes.
pull-smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    {{cargo}} run -p client --example pull_smoke

# Production render benchmark: real server seed + full LOD table, fixed
# 1920x1080 no-vsync client, three repeated JSON runs by default.
# Pair modes: per-chunk+raster, direct+raster, gpu+raster (far terrain is always raster).
# The `key=value` argument spelling intentionally matches benchmark reports:
# `just render-bench backend=gpu+raster adapter=T600 repetitions=3 threads=4`.
# `threads` caps both the Bevy worker pools and Tokio server workers so the
# production-scene load leaves CPU headroom for the desktop.
render-bench backend="direct+raster" adapter="" repetitions="3" threads="4":
    #!/usr/bin/env bash
    set -euo pipefail
    backend_arg="{{backend}}"
    adapter_arg="{{adapter}}"
    repetitions_arg="{{repetitions}}"
    threads_arg="{{threads}}"
    selected_backend="${backend_arg#backend=}"
    selected_adapter="${adapter_arg#adapter=}"
    selected_repetitions="${repetitions_arg#repetitions=}"
    selected_threads="${threads_arg#threads=}"
    case "$selected_backend" in
        per-chunk|per-chunk+raster)
            near="per-chunk"
            ;;
        direct|direct+raster)
            near="direct"
            ;;
        gpu|gpu+raster)
            near="gpu"
            ;;
        *)
            echo "invalid backend '$selected_backend' (expected per-chunk+raster, direct+raster, or gpu+raster)" >&2
            exit 2
            ;;
    esac
    if ! [[ "$selected_repetitions" =~ ^[1-9][0-9]*$ ]]; then
        echo "repetitions must be a positive integer, got '$selected_repetitions'" >&2
        exit 2
    fi
    if ! [[ "$selected_threads" =~ ^[0-9]+$ ]] || [ "$selected_threads" -lt 4 ]; then
        echo "threads must be an integer >= 4, got '$selected_threads'" >&2
        exit 2
    fi
    mkdir -p target/render-bench
    {{cap}} {{cargo}} build --release --bin client --bin server
    RUST_BACKTRACE=1 TOKIO_WORKER_THREADS="$selected_threads" \
        ./target/release/server > target/render-bench/server.log 2>&1 &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    sleep 1
    for repetition in $(seq 1 "$selected_repetitions"); do
        adapter_slug="$selected_adapter"
        if [ -z "$adapter_slug" ]; then adapter_slug="default"; fi
        adapter_slug=${adapter_slug//[^[:alnum:]._-]/_}
        world_slug="{{world}}"
        if [ -n "$world_slug" ]; then world_slug="-${world_slug}"; fi
        output="target/render-bench/${selected_backend}-${adapter_slug}${world_slug}-run-${repetition}.json"
        echo ">> render benchmark ${selected_backend} repetition ${repetition}/${selected_repetitions} -> ${output}"
        if [ -n "$selected_adapter" ]; then export WGPU_ADAPTER_NAME="$selected_adapter"; else unset WGPU_ADAPTER_NAME || true; fi
        RENDER_BENCH=1 \
        RENDER_BENCH_REPETITION="$repetition" \
        RENDER_BENCH_OUTPUT="$output" \
        VOXEL_NEAR_RENDER="$near" \
        VOXEL_CLIENT_WORKER_THREADS="$selected_threads" \
        TOKIO_WORKER_THREADS="$selected_threads" \
        RUST_BACKTRACE=1 \
            ./target/release/client
    done

# ---- WASM / browser -------------------------------------------------------

# The server the browser build's main menu prefills in Join server (the deployed
# game's hostname: the WebTransport certificate is Let's Encrypt's for that name,
# so the bare deploy_host IP would fail the browser's TLS check). Baked into
# `web-release` / `deploy` bundles as VOXEL_WEB_SERVER; `just web` leaves it unset,
# so a dev page prefills 127.0.0.1 (the local `just server`).
web_server := "orion.factoriosurvivalworld.com"

# The login service the browser page should use (`just login=http://127.0.0.1:8790 web`):
# `web` prints the page URL with `?login=<url>`, which makes the join-only menu offer
# Play online — on a localhost page only (127.0.0.1 / localhost / [::1]): on any other
# host the page ignores `?login=` and uses its build-time VOXEL_LOGIN_URL. Empty = the
# plain Join-server menu.
login := ""

# The browser has no worlds of its own: it opens on the main menu, whose Join
# server connects to a running server. So this starts the dev server too (the
# same as `just server`, built with the same dials; log in target/web-server.log)
# unless one already listens on UDP 6466, serves the page, and stops the server
# (orderly: the world is saved) when the page server exits. Then press Join
# server — the field says 127.0.0.1. ?world=tiny|small|full picks the world scale;
# ?workers=0 runs the sim and meshing on the page's main thread instead of in Web
# Workers (crates/client/src/web_worker/), ?workers=N sizes the compute pool.
# Needs: rustup target add wasm32-unknown-unknown; cargo install wasm-server-runner.
# Serve the wasm client via wasm-server-runner (open http://127.0.0.1:1334/).
web:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    if ss -Huln 'sport = :6466' | grep -q .; then
        echo "web: a server already listens on UDP 6466 — using it"
    else
        {{cap}} {{cargo}} build --bin server
        echo "web: starting the dev server (log: target/web-server.log)"
        RUST_BACKTRACE=1 ./target/debug/server >target/web-server.log 2>&1 </dev/null &
        server=$!
        trap 'echo "web: stopping the dev server (saving the world)"; kill -TERM $server 2>/dev/null; wait $server 2>/dev/null || true' EXIT
    fi
    # The page's own player name (local servers): `just user=bob web` -> ?name=bob.
    name=""
    if [ -n "{{user}}" ]; then name="name=$(python3 -c 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' "{{user}}")"; fi
    if [ -n "{{login}}" ]; then
        echo "web: open http://127.0.0.1:1334/?login={{login}}${name:+&$name}"
    elif [ -n "$name" ]; then
        echo "web: open http://127.0.0.1:1334/?$name (add &login=<login service url> for Play online)"
    else
        echo "web: open http://127.0.0.1:1334/ (add ?login=<login service url> for Play online; ?name=<you> picks your player name)"
    fi
    WASM_SERVER_RUNNER_CUSTOM_INDEX_HTML=crates/client/index.html \
        {{cargo}} run -p client --target wasm32-unknown-unknown

# Type-check the client for the browser (wasm32-unknown-unknown) without
# building, bundling or serving it: the cheap answer to "does this still compile
# for the web" after touching the renderer, compute jobs or anything cfg'd on
# wasm (no wasm-bindgen, no binaryen, no server; a few minutes cold, seconds
# warm). At the default (test) region size, like `smoke`.
web-check:
    {{cap}} {{cargo}} check -p client --lib --bins --target wasm32-unknown-unknown

# The browser release bundle in target/web/ (scripts/web_release.sh): the
# `wasm-release` profile, wasm-bindgen (the Cargo.lock-matched CLI, installed under
# target/tools on first use), wasm-opt -O3 from a pinned binaryen release (also
# under target/tools; BINARYEN=none skips it), index.html, api/version and assets/. Built at the play
# region size, like the server it talks to; its Join server field prefills
# `web_server`. Serve the directory as is.
web-release:
    VOXEL_REGION_BLOCKS={{play_blocks}} VOXEL_WEB_SERVER={{web_server}} CAP="{{cap}}" scripts/web_release.sh

# ---- Deploy (the game box) ---------------------------------------------------

# The public web build on root@5.9.7.138: https://orion.factoriosurvivalworld.com/
# (deploy/web/compose.yml: the release server in a Debian container with the
# Let's Encrypt certificate of the survival-world proxy, and a file server for the
# bundle behind that proxy). Builds the browser bundle and the release server
# with the same region/chunk dials, rsyncs both to /srv/mmo and restarts the
# server (orderly: the world is saved; open tabs reload on the new version). The
# proxy site, firewall (UDP 6466/6467) and DNS record are in
# ~/Projects/mod/mods/survival-world. `just deploy-host=user@host deploy` to aim elsewhere.
deploy_host := "root@5.9.7.138"

deploy: deploy-build deploy-push

# Build only: target/web + target/release/server.
deploy-build: web-release
    VOXEL_REGION_BLOCKS={{play_blocks}} {{cap}} {{cargo}} build --release --bin server

# Ship what deploy-build made and restart (scripts/deploy_push.sh).
deploy-push:
    DEPLOY_HOST={{deploy_host}} scripts/deploy_push.sh

# Follow the deployed server's log.
deploy-logs:
    ssh {{deploy_host}} docker compose -f /srv/mmo/compose.yml logs -f --tail 200 server

# `orion` itself goes over the VPN; recipes take `orion_context=orion-ssh` with this.
# Fallback for when the VPN is down: forward the k3s API to localhost:6443 over ssh (context orion-ssh).
tunnel:
    scripts/tunnel.sh {{deploy_host}}

# ---- Operator VPN into orion (docs/superpowers/plans/2026-11-15-orion-vpn.md) --------
# WireGuard wg0 on the box (10.99.0.1, UDP 51820); this machine is 10.99.0.2 through the
# NetworkManager connection `orion-vpn` (autoconnects). Routes k3s's pod (10.42.0.0/16) and
# Service (10.43.0.0/16) networks, and *.cluster.local and *.vpn.errantry.renownandruin.com
# resolve through CoreDNS (split DNS), so a Service answers at
# http://<svc>.<namespace>.svc.cluster.local:<port> with no port-forward, and a vpn-names name
# (root deploy/envs/orion/vpn-names) at https://<name>.vpn.errantry.renownandruin.com.
# The kube context `orion` is https://10.99.0.1:6443 (tls-server-name kubernetes).

# Bring the VPN up / down on this machine.
orion-vpn-up:
    nmcli connection up orion-vpn
orion-vpn-down:
    nmcli connection down orion-vpn

# Is the VPN usable: the box, cluster DNS, the k3s API and Grafana by its Service name.
orion-vpn-status:
    #!/usr/bin/env bash
    nmcli -t -f GENERAL.STATE connection show orion-vpn 2>/dev/null | sed 's/^/orion-vpn: /' || echo "orion-vpn: no such connection"
    ping -c1 -W2 10.99.0.1 >/dev/null && echo "box 10.99.0.1: ok" || echo "box 10.99.0.1: NO ANSWER"
    getent hosts kube-dns.kube-system.svc.cluster.local >/dev/null && echo "cluster DNS: ok" || echo "cluster DNS: FAILS"
    kubectl --context {{orion_context}} get --raw /livez >/dev/null 2>&1 && echo "k3s API: ok" || echo "k3s API: FAILS"
    echo "grafana: $(curl -s -o /dev/null -w "%{http_code}" --max-time 5 http://kube-prometheus-stack-grafana.monitoring.svc.cluster.local/api/health) (000 = no answer)"
    echo "keycloak admin (https, vpn-names): $(curl -s -o /dev/null -w "%{http_code}" --max-time 5 https://keycloak.vpn.errantry.renownandruin.com/admin/master/console/) (000 = no answer)"
    echo "garage admin: $(curl -s -o /dev/null -w "%{http_code}" --max-time 5 http://garage.garage.svc.cluster.local:3903/health) (000 = no answer)"

# scripts/orion_vpn_peer.sh; writes the device's config, private key included, to target/orion-vpn/<name>.conf.
# Add another device to the VPN at 10.99.0.<octet>.
orion-vpn-peer name octet:
    VPN_HOST={{deploy_host}} scripts/orion_vpn_peer.sh {{name}} {{octet}}

# ---- Dev Postgres (deploy/dev) ---------------------------------------------

# Start the dev Postgres on 127.0.0.1:55432 and wait until it is healthy.
pg-up:
    docker compose -f deploy/dev/compose.yml up -d --wait postgres
    @echo "dev database: {{dev_database_url}}"

# Stop the dev Postgres; its data stays in the mmo-dev-pg volume.
pg-down:
    docker compose -f deploy/dev/compose.yml down

# Delete the dev Postgres data and start again empty.
pg-reset:
    docker compose -f deploy/dev/compose.yml down -v
    docker compose -f deploy/dev/compose.yml up -d --wait postgres

# Run psql inside the dev Postgres: `just pg-psql -c 'select version()'`.
[positional-arguments]
pg-psql *args:
    docker compose -f deploy/dev/compose.yml exec -T postgres psql -U mmo -d mmo "$@"

# ---- Dev Keycloak (deploy/dev) ---------------------------------------------

# Start the dev Keycloak on 127.0.0.1:8181 (8180 is orion's port-forward, `orion-keycloak-admin`) (admin / admin) and wait until it is healthy.
# Realm `players` from deploy/dev/keycloak/players-realm.json, login theme website/theme/orion.
# Its mail (email verification, password reset) lands in mailpit: http://127.0.0.1:8025.
kc-up:
    docker compose -f deploy/dev/compose.yml up -d --wait keycloak
    @echo "dev issuer: http://127.0.0.1:8181/realms/players"
    @echo "dev mail:   http://127.0.0.1:8025"

# Stop and remove the dev Keycloak and its mailpit, so the next `kc-up` imports the realm file again.
kc-down:
    docker compose -f deploy/dev/compose.yml rm -sf keycloak mailpit

# Check every login page the dev Keycloak serves is in the orion theme (`just kc-up` first).
# Env KC, SITE, SKIP_LOGIN=1: see scripts/site_theme_check.sh.
site-theme-check:
    scripts/site_theme_check.sh

# Serve the website on http://127.0.0.1:8182 against the dev Keycloak (started first), with
# production's headers and Content-Security-Policy (scripts/site_dev_server.py).
site-dev: kc-up
    python3 scripts/site_dev_server.py website/site

# Unit tests of the website's browser login module (plain node, no dependencies).
site-test:
    node --test website/tests/*.test.mjs

# ---- Test -----------------------------------------------------------------

# Scoped per-crate on purpose: first-party crates only (the vendored physics
# forks are path dependencies, tested through physics-next and
# `physics-next-upstream`), and each crate's suite is big enough on its own.
#
# Tests measured >100ms (2026-07-08, debug build) carry
# `#[ignore = "slow: >100ms — run via just test-full"]`, so the default run is
# the fast feedback loop and `test-full` adds them back via --include-ignored.
# Tag new slow tests the same way; re-measure with:
#   RUSTC_BOOTSTRAP=1 cargo test -p <crate> -- -Zunstable-options --report-time

# The routine check, hard-limited to `smoke_timeout` seconds (default 60). Even
# the "quick" per-crate suites below build every game test module and a
# Bevy-sized client test binary, which is far too slow for a while-iterating loop. So
# smoke runs two modules of the game test binary (undo/rollback invariants,
# region park/restore) and the server lib tests, TYPE-CHECKS the server's integration
# tests (all tagged slow, so nothing else would notice them rotting — a new
# `ClientPacket` variant once left one uncompilable), and only TYPE-CHECKS the client
# (lib + bin + tests): building or linking a client test binary means a full
# Bevy codegen, which can never fit the budget. Client unit tests are an
# explicit opt-in via `just test-one client "--lib <filter>"`.
smoke_timeout := "60"
smoke:
    #!/usr/bin/env bash
    set -uo pipefail
    deadline=$(( $(date +%s) + {{smoke_timeout}} ))
    step() {
        local left=$(( deadline - $(date +%s) ))
        if [ "$left" -le 0 ]; then left=1; fi
        echo ">> $*"
        timeout --foreground "$left" {{cap}} "$@"
        local rc=$?
        if [ "$rc" -eq 124 ]; then
            echo "!! smoke exceeded {{smoke_timeout}}s (cold build?). Finished crates stay cached:" >&2
            echo "!! rerun, or raise the limit once with: just smoke_timeout=600 smoke" >&2
        fi
        if [ "$rc" -ne 0 ]; then exit "$rc"; fi
    }
    step {{cargo}} test -q -p game --test it -- simple:: region_runner::
    step {{cargo}} test -q -p server --lib
    step {{cargo}} check -q -p server --tests --features test-hooks
    step {{cargo}} check -q -p client --lib --bins --tests
    echo ">> smoke ok in $(( $(date +%s) - deadline + {{smoke_timeout}} ))s"

# ---- Login service (crates/login) -------------------------------------------

# The login service / its admin commands: `just login` serves on 127.0.0.1:8790
# (VOXEL_LOGIN_* env, database target/dev-login/login.sqlite; `VOXEL_OIDC_ISSUER` turns the
# Keycloak login on, see `dev_oidc_issuer`); `just login world list`,
# `just login account issue-refresh a@b.c`, `just login account revoke a@b.c`, ... Built
# capped, then run.
login *args='serve':
    {{cap}} {{cargo}} build -p login --bin login
    ./target/debug/login {{args}}

# Make the next signing key in target/dev-login/keys (prints its kid).
login-keygen:
    just login keygen

# Register a world: `just login-world-add dev --name "Dev world" --quic 127.0.0.1:6466 --wt https://127.0.0.1:6467/`.
# `--link host:port --link-cert <sha256>` names the host's service link: joins then push
# `SpawnPlayer` there (the login service needs VOXEL_SERVICE_TOKEN[_FILE]); see `dev-r2`.
login-world-add *args:
    just login world add {{args}}

login-world-list:
    just login world list

with_web := ""

# The dev account `dev-login` / `run-online` set up (verified, no password: the client starts
# from a refresh token `login account issue-refresh` prints). Local dev database
# (target/dev-login/login.sqlite) only.
dev_email := "dev@localhost"

# The dev Keycloak's realm (`just kc-up`): what the login service started by `dev-login`, `dev-r2`
# and `dev-r3` trusts for player logins (`VOXEL_OIDC_ISSUER`). The audience (`mmo-login`) and the
# client (`game`) are the login service's defaults and the realm file's `game` client.
dev_oidc_issuer := "http://127.0.0.1:8181/realms/players"

# How long `run-online` waits for the login service and the server to come up.
online_wait_secs := "180"

# What `dev-login` and `run-online` need in the dev login database: a signing key,
# the world `dev` (127.0.0.1:6466 / :6467) and the verified account `dev_email` with a fresh refresh token in
# target/dev-login/refresh (the file `run-online`'s client keeps its login in; rewritten at every
# start, the client rotates the token in it while it runs). The world
# is (re)registered WITHOUT a host link (R2a: the jwt server checks tickets by epoch), undoing
# the link a `dev-r2` run registers.
_dev-login-setup:
    #!/usr/bin/env bash
    set -euo pipefail
    {{cap}} {{cargo}} build -p login --bin login
    login=./target/debug/login
    if ! ls target/dev-login/keys/*.pem >/dev/null 2>&1; then
        $login keygen
    fi
    $login world add dev --name "Dev world" --quic 127.0.0.1:6466 --wt https://127.0.0.1:6467/
    mkdir -p target/dev-login
    $login account issue-refresh {{dev_email}} >target/dev-login/refresh.tmp
    chmod 600 target/dev-login/refresh.tmp
    mv target/dev-login/refresh.tmp target/dev-login/refresh

# A dev server that requires login, with the login service beside it: starts the dev
# Keycloak (`kc-up`), ensures a signing key, the world `dev` and the dev account (`dev_email`,
# with a kept token), runs `login serve` (players log in with Keycloak: user `dev`, password
# `dev-password`) in the background and the dev server (VOXEL_AUTH=jwt, play region size) in the
# foreground; SIGTERMs the login service on exit.
# `just with_web=1 dev-login` also serves the browser page (wasm-server-runner, :1334)
# and prints its `?login=` URL (log target/dev-login/web.log).
dev-login: kc-up _dev-login-setup
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_OIDC_ISSUER={{dev_oidc_issuer}}
    {{cap}} {{cargo}} build --bin server
    login=./target/debug/login
    mkdir -p target/dev-login
    $login serve >target/dev-login/login.log 2>&1 </dev/null &
    lpid=$!
    trap 'echo "dev-login: stopping the login service"; kill -TERM $lpid 2>/dev/null; wait $lpid 2>/dev/null || true' EXIT
    echo "dev-login: login service on http://127.0.0.1:8790 (log: target/dev-login/login.log)"
    if [ -n "{{with_web}}" ]; then
        # the browser page beside it: wasm-server-runner on :1334 (the dev server above
        # keeps UDP 6466/6467); open the printed URL
        echo "dev-login: browser page: http://127.0.0.1:1334/?login=http://127.0.0.1:8790"
        WASM_SERVER_RUNNER_CUSTOM_INDEX_HTML=crates/client/index.html \
            {{cargo}} run -p client --target wasm32-unknown-unknown >target/dev-login/web.log 2>&1 </dev/null &
        wpid=$!
        trap 'echo "dev-login: stopping the login service and page"; kill -TERM $lpid $wpid 2>/dev/null; wait $lpid $wpid 2>/dev/null || true' EXIT
    fi
    VOXEL_AUTH=jwt VOXEL_JWT_ADMISSION=newest VOXEL_WORLD_ID=dev VOXEL_LOGIN_URL=http://127.0.0.1:8790 ./target/debug/server

# The `dev-login` setup, then the login service and the jwt dev server in the
# background (logs target/dev-login/{login,server}.log), and once both answer (at most
# `online_wait_secs`) the client in the foreground as `client debug online`: it starts from the
# kept login of `dev_email` (no screens, no password), joins world `dev` with the ticket's JWT and starts an adventure (or resumes the
# account's). Closing the client SIGTERMs the server (it saves
# the world) and the login service. Honours `adapter` / `world` like `run`; play
# region size. Refuses to start if UDP 6466 or TCP 8790 is already taken.
# Straight into the game WITH login, as the pre-created dev account (no screens).
run-online: _dev-login-setup
    #!/usr/bin/env bash
    set -euo pipefail
    if ss -Hlun 'sport = :6466' | grep -q . || ss -Hltn 'sport = :8790' | grep -q .; then
        echo "run-online: UDP 6466 or TCP 8790 is already in use (a server / dev-login running?); stop it first" >&2
        exit 1
    fi
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    {{cap}} {{cargo}} build --bin server --bin client
    mkdir -p target/dev-login
    login_url=http://127.0.0.1:8790
    ./target/debug/login serve >target/dev-login/login.log 2>&1 </dev/null &
    lpid=$!
    spid=""
    trap 'echo "run-online: stopping the server and the login service"; kill -TERM $spid $lpid 2>/dev/null; wait $spid $lpid 2>/dev/null || true' EXIT
    VOXEL_AUTH=jwt VOXEL_JWT_ADMISSION=newest VOXEL_WORLD_ID=dev VOXEL_LOGIN_URL=$login_url \
        ./target/debug/server >target/dev-login/server.log 2>&1 </dev/null &
    spid=$!
    echo "run-online: waiting for the login service ($login_url) and the server (UDP 6466)..."
    deadline=$(( SECONDS + {{online_wait_secs}} ))
    until curl -fsS -o /dev/null "$login_url/healthz" 2>/dev/null && ss -Hlun 'sport = :6466' | grep -q .; do
        if ! kill -0 $lpid 2>/dev/null; then
            echo "run-online: the login service exited; see target/dev-login/login.log" >&2; exit 1
        fi
        if ! kill -0 $spid 2>/dev/null; then
            echo "run-online: the server exited; see target/dev-login/server.log" >&2; exit 1
        fi
        if (( SECONDS >= deadline )); then
            echo "run-online: not up after {{online_wait_secs}} s; see target/dev-login/login.log and server.log" >&2; exit 1
        fi
        sleep 0.5
    done
    VOXEL_LOGIN_URL=$login_url VOXEL_LOGIN_REFRESH_FILE=$PWD/target/dev-login/refresh \
        RUST_BACKTRACE=1 ./target/debug/client debug online

# ---- Split roles: world coordinator + region host (R2b) ---------------------------------

# The world as two processes on the dev Postgres world: `coordinator` (history, claims,
# world requests; no client edge, no regions) and `host` (regions, the client edge, save
# cuts) dialing it over the service link. Dev secrets live under the git-ignored
# target/dev-r2/: the shared service token (`service-token`) and the coordinator's
# certificate pin (`coordinator.cert-sha256`, its cert and private key beside it).
dev_r2_dir := "target/dev-r2"

# Several hosts of one world (`claim` ownership: `host`, `coordinator`, `dev-r3`) keep their dev
# secrets and pins under the git-ignored target/dev-r3/ instead: the service token, the
# coordinator's pin (`coordinator.cert-sha256`) and the login link's (`login-link.cert-sha256`), and
# every host's peer-link pin (`h<index>.peer.cert-sha256`), plus `dev-r3`'s logs.
dev_r3_dir := "target/dev-r3"

# Make <dir>/service-token (random, mode 0600) unless it exists.
_dev-token dir:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p {{dir}}
    if [ ! -s {{dir}}/service-token ]; then
        (umask 077; head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' >{{dir}}/service-token)
        echo "made {{dir}}/service-token"
    fi

# Make target/dev-r2/service-token unless it exists.
_dev-r2-token: (_dev-token dev_r2_dir)

# The world coordinator alone (VOXEL_ROLE=coordinator) for the dev Postgres world
# (`just pg-up` first): serves hosts on 127.0.0.1:6480 (pin target/dev-r2|r3/coordinator.cert-sha256) and
# the login service on 127.0.0.1:6490 (pin login-link.cert-sha256 beside it; login dials it, never a
# host). World id `dev`, as every dev host and the login registration. Play region size.
# `ownership` is `claim` (default: any number of hosts, secrets and pins under target/dev-r3/) or
# `static` (ONE host, as `dev-r2`: under target/dev-r2/); give `host` the same word. The
# coordinator declares a dead host's succession only under `claim` and refuses a host whose
# VOXEL_OWNERSHIP says otherwise. It reads no host count.
coordinator ownership="claim": (_dev-token (if ownership == "static" { dev_r2_dir } else { dev_r3_dir }))
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    {{cap}} {{cargo}} build --bin server
    dir={{ if ownership == "static" { dev_r2_dir } else { dev_r3_dir } }}
    case "{{ownership}}" in
        claim | static) ;;
        *) echo "coordinator: ownership is claim or static (the argument of just coordinator), not {{ownership}}" >&2; exit 1 ;;
    esac
    VOXEL_ROLE=coordinator VOXEL_OWNERSHIP={{ownership}} VOXEL_WORLD_ID=dev VOXEL_WORLD_DB={{dev_database_url}} \
        VOXEL_SERVICE_TOKEN_FILE=$dir/service-token \
        VOXEL_COORDINATOR_CERT_HASH_FILE=$dir/coordinator.cert-sha256 \
        VOXEL_LOGIN_LINK_BIND=127.0.0.1:6490 VOXEL_LOGIN_LINK_CERT_HASH_FILE=$dir/login-link.cert-sha256 \
        RUST_BACKTRACE=1 ./target/debug/server

# The environment of dev host $1 (`host`, `dev-r3`), as `NAME=value` words: peer delay $2 ms,
# secrets and pins under $3, world store $4, ownership word $5 (`claim` or `static`, nothing else).
# The index only picks names and ports: the host is called h<index> (VOXEL_HOST_NAME: a label for
# logs and file names; a restarted process of the same name lapses its old row at once) and
# listens for clients on UDP 6466 + 10 x index (WebTransport 6467 + 10 x index) and for its
# peers on 127.0.0.1:6500 + index (pin $3/h<index>.peer.cert-sha256). Its id comes from the store.
# Hosts never bind the login link (the coordinator does). Every host advertises its own
# WebTransport URL (a ticket names the endpoint the ack gives). Under `claim` a host needs the login service
# (jwt, world `dev`) and a killed host's land goes to an heir and does not come back by itself (a
# restarted process is a NEW host and owns nothing until it is given land); `static` = one
# host (the store refuses a second), VOXEL_AUTH as set.
# Refuses (exit 1) what `_dev-hosts-check` refuses, also under `--dry-run`.
host_env_sh := '''
i=$1 dir=$3 own=$5
case "$own" in
    claim | static) ;;
    *) echo "dev host: ownership is claim or static (the last word of just host), not $own" >&2; exit 1 ;;
esac
case "$i/$2" in
    *[!0-9/]* | /* | */ | *//*) echo "dev host: index and delay are whole numbers, given positionally (just host 1 2, not delay=2)" >&2; exit 1 ;;
esac
if [ "$i" -gt 5 ]; then
    echo "dev host: need 0 <= index <= 5 (host 6's peer port would be host 4's UDP port)" >&2; exit 1
fi
printf '%s ' VOXEL_ROLE=host VOXEL_OWNERSHIP=$own VOXEL_HOST_NAME=h$i VOXEL_WORLD_ID=dev VOXEL_WORLD_DB=$4 \
    VOXEL_SERVICE_TOKEN_FILE=$dir/service-token VOXEL_COORDINATOR_CERT_HASH_FILE=$dir/coordinator.cert-sha256 \
    VOXEL_SERVER_PORT=$((6466 + 10 * i)) VOXEL_WT_PORT=$((6467 + 10 * i)) VOXEL_PEER_BIND=127.0.0.1:$((6500 + i)) \
    VOXEL_PEER_CERT_HASH_FILE=$dir/h$i.peer.cert-sha256 VOXEL_ADVERTISE_WT=https://127.0.0.1:$((6467 + 10 * i))/ \
    VOXEL_PEER_LINK_DELAY_MS=$2
if [ "$own" = claim ]; then
    printf '%s ' VOXEL_AUTH=jwt VOXEL_LOGIN_URL=http://127.0.0.1:8790
fi
'''

# Refuses bad `host` / `dev-r3` arguments before any other dependency does work: whole
# numbers given positionally (`just dev-r3 3`, not `hosts=3`), `index` < `hosts`, and
# `min_hosts` <= `hosts` <= 6 (from 7 hosts the peer ports 6500 + n meet the client ports
# 6466 + 10n).
_dev-hosts-check index hosts delay min_hosts:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! [[ "{{index}}" =~ ^[0-9]+$ && "{{hosts}}" =~ ^[0-9]+$ && "{{delay}}" =~ ^[0-9]+$ ]]; then
        echo "index, hosts and delay are whole numbers, given positionally (just dev-r3 3, just host 1 2 — not hosts=3)" >&2; exit 1
    fi
    if (( {{hosts}} < {{min_hosts}} )); then
        echo "dev-r3 runs several hosts (hosts >= 2): for one host use just dev-r2" >&2; exit 1
    fi
    if (( {{hosts}} > 6 )); then
        echo "at most 6 dev hosts on one machine: host 6's peer port (6506) is host 4's client port" >&2; exit 1
    fi
    if (( {{index}} >= {{hosts}} )); then
        echo "host index {{index}} is not below {{hosts}}" >&2; exit 1
    fi

# A region host (VOXEL_ROLE=host) of the dev Postgres world, dialing the coordinator at
# 127.0.0.1:6480 (started first or later — it waits for the pin). `index` (0..5) picks the
# host's name `h<index>` and ports only: UDP 6466 + 10 x index for clients (WebTransport
# 6467 + 10 x index) and 127.0.0.1:6500 + index for its peers; the host's id is issued by the
# store when it registers. Start as many as you like under `claim` (the default: super-regions
# claimed on demand, a killed host's land moving to an heir for good; jwt with the login
# service, pins under target/dev-r3/, see `dev-r3`, which runs them all); `static` = ONE host,
# as `dev-r2` (target/dev-r2/). The coordinator must run the same word (`coordinator`).
# `delay` holds every outgoing peer frame that many ms (measurement only). Play region size.
# e.g. `just coordinator` then `just host 0` and `just host 1`.
host index="0" delay="0" ownership="claim": (_dev-hosts-check index "6" delay "1") (_dev-token (if ownership == "static" { dev_r2_dir } else { dev_r3_dir }))
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    {{cap}} {{cargo}} build --bin server
    env {{ shell(host_env_sh, index, delay, if ownership == "static" { dev_r2_dir } else { dev_r3_dir }, dev_database_url, ownership) }} \
        RUST_BACKTRACE=1 ./target/debug/server

# The split world with login: the dev Keycloak (`kc-up`) and Postgres (`pg-up`), the `dev-login`
# setup and login service (players log in with Keycloak), the coordinator (log target/dev-r2/coordinator.log) and, once it has written
# its pin, the ONE host (`h0`, `static` ownership, VOXEL_AUTH=jwt, world `dev`) in the foreground. The
# coordinator listens for the login service on 127.0.0.1:6490 (pin target/dev-r2/login-link.cert-sha256);
# once it has written that pin, world `dev` is registered with the link (no addresses: the host's
# spawn ack names them), so joins push `SpawnPlayer` through the coordinator and the host admits a
# ticket only against it. SIGTERMs the coordinator and the login
# service on exit. Connect with the dev account's kept login and no screens:
# `VOXEL_LOGIN_URL=http://127.0.0.1:8790 VOXEL_LOGIN_REFRESH_FILE=$PWD/target/dev-login/refresh ./target/debug/client debug online`,
# or the login screens: `VOXEL_LOGIN_URL=http://127.0.0.1:8790 just play`. The first start on an empty dev database
# forges the world in the coordinator, which can take longer than the `online_wait_secs`
# (180 s) wait for its pin: then rerun (the world is kept), or raise it once with
# `just online_wait_secs=900 dev-r2`.
dev-r2: kc-up pg-up _dev-login-setup _dev-r2-token
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_OIDC_ISSUER={{dev_oidc_issuer}}
    {{cap}} {{cargo}} build --bin server
    pin={{dev_r2_dir}}/coordinator.cert-sha256
    login_pin={{dev_r2_dir}}/login-link.cert-sha256
    rm -f "$pin" "$login_pin"
    VOXEL_SERVICE_TOKEN_FILE={{dev_r2_dir}}/service-token \
        ./target/debug/login serve >target/dev-login/login.log 2>&1 </dev/null &
    lpid=$!
    cpid=""
    # Once the coordinator has written its login-link pin: world `dev` gets the link.
    (
        until [ -s "$login_pin" ]; do sleep 0.5; done
        ./target/debug/login world add dev --name "Dev world" \
            --link 127.0.0.1:6490 --link-cert "$(cat "$login_pin")" >/dev/null
        echo "dev-r2: world dev registered with the coordinator's login link (127.0.0.1:6490)"
    ) &
    rpid=$!
    trap 'echo "dev-r2: stopping the coordinator and the login service"; kill -TERM $cpid $lpid $rpid 2>/dev/null; wait $cpid $lpid 2>/dev/null || true' EXIT
    VOXEL_ROLE=coordinator VOXEL_OWNERSHIP=static VOXEL_WORLD_ID=dev VOXEL_WORLD_DB={{dev_database_url}} \
        VOXEL_SERVICE_TOKEN_FILE={{dev_r2_dir}}/service-token VOXEL_COORDINATOR_CERT_HASH_FILE="$pin" \
        VOXEL_LOGIN_LINK_BIND=127.0.0.1:6490 VOXEL_LOGIN_LINK_CERT_HASH_FILE="$login_pin" \
        ./target/debug/server >{{dev_r2_dir}}/coordinator.log 2>&1 </dev/null &
    cpid=$!
    echo "dev-r2: login on http://127.0.0.1:8790 (target/dev-login/login.log), coordinator on 127.0.0.1:6480 ({{dev_r2_dir}}/coordinator.log)"
    deadline=$(( SECONDS + {{online_wait_secs}} ))
    until [ -s "$pin" ]; do
        if ! kill -0 $cpid 2>/dev/null; then
            echo "dev-r2: the coordinator exited; see {{dev_r2_dir}}/coordinator.log" >&2; exit 1
        fi
        if (( SECONDS >= deadline )); then
            echo "dev-r2: no coordinator pin after {{online_wait_secs}} s; see {{dev_r2_dir}}/coordinator.log" >&2; exit 1
        fi
        sleep 0.5
    done
    VOXEL_ROLE=host VOXEL_OWNERSHIP=static VOXEL_HOST_NAME=h0 VOXEL_WORLD_ID=dev \
        VOXEL_WORLD_DB={{dev_database_url}} \
        VOXEL_SERVICE_TOKEN_FILE={{dev_r2_dir}}/service-token VOXEL_COORDINATOR_CERT_HASH_FILE="$pin" \
        VOXEL_ADVERTISE_WT=https://127.0.0.1:6467/ \
        VOXEL_AUTH=jwt VOXEL_LOGIN_URL=http://127.0.0.1:8790 \
        RUST_BACKTRACE=1 ./target/debug/server

# The `dev-r3` host arguments: one single-quoted env-word list per host 0..$1, joined by
# ` \` line ends (delay $2, dir $3, store $4, `host_env_sh` $5; ownership `claim`).
dev_r3_launch_sh := '''
i=0
while [ "$i" -lt "$1" ]; do
    words=$(sh -c "$5" sh "$i" "$2" "$3" "$4" claim) || exit 1
    i=$((i + 1))
    if [ "$i" -lt "$1" ]; then printf "'%s' %s\n" "$words" '\'; else printf "'%s'" "$words"; fi
done
'''

# The coordinator's env words of `dev-r3` / `go-nogo-r3` (dir $1, store $2): `claim` ownership, world `dev`,
# hosts on 127.0.0.1:6480, the login service on 127.0.0.1:6490 (its pin: $1/login-link.cert-sha256).
dev_r3_coordinator_sh := '''
printf '%s ' VOXEL_ROLE=coordinator VOXEL_OWNERSHIP=claim VOXEL_WORLD_ID=dev VOXEL_WORLD_DB=$2 \
    VOXEL_SERVICE_TOKEN_FILE=$1/service-token VOXEL_COORDINATOR_CERT_HASH_FILE=$1/coordinator.cert-sha256 \
    VOXEL_LOGIN_LINK_BIND=127.0.0.1:6490 VOXEL_LOGIN_LINK_CERT_HASH_FILE=$1/login-link.cert-sha256
'''

# One world on 2..6 region hosts with login: the dev Postgres (`pg-up`, started with
# max_prepared_transactions > 0 — several hosts stage their cuts in two phases and refuse
# to start without it), the dev Keycloak (`kc-up`), the `dev-login` setup, `worldctl clock-reset`, then
# scripts/dev_r3.sh: the login service (players log in with Keycloak), the coordinator (log target/dev-r3/coordinator.log)
# and, once it has written its pin, EVERY host h0..h<hosts-1> (`host` above: host h<n> on UDP
# 6466 + 10n, peers on 127.0.0.1:6500 + n; logs target/dev-r3/host-<n>.log). Hosts have ids
# issued by the store, not numbers: the index only names them. World `dev` is
# registered with the coordinator's login link (127.0.0.1:6490, no addresses: each host's ack
# names its endpoint) once the coordinator has written its login pin; `just world-hosts-dev` lists the host rows.
# Every host runs under `claim` ownership: a host that is killed (`kill -9`) loses its lease,
# its land moves to an heir (a succession) and does NOT come back by itself — a restarted host
# is a NEW host (its own id; the same name lapses the old row at once) and owns nothing at first.
# A host that exits 5 (EXIT_WORLD_ROLLBACK: the world went back to its last cut) or 137
# (killed, e.g. `kill -9`) — or 3 (EXIT_FENCED: replaced, e.g. a hung host that woke after its
# succession; restarted only because the hosts run `claim`) — is started again after 1 s — at
# most 3 times within 60 s, then its loop gives up; one that exits 0 is not. Any member ending otherwise (login, the coordinator, a host loop)
# takes the whole stack down, exit 1. Ctrl-C stops the hosts first and waits for them (their
# leave cut needs the coordinator; 95 s, a host's own exit bound), then the coordinator, then the login service (60 s
# each); what outlives its deadline is KILLed, and a second Ctrl-C kills at once.
# Arguments are positional: `just dev-r3 3`, `just dev-r3 2 5` (`delay` holds every peer
# frame that many ms; measurement only). A world whose super-region side was set
# (`just worldctl super-region <url> <n>`) needs VOXEL_SUPER_REGION_REGIONS=<n> in the
# environment. First start on an empty database: see `dev-r2` (`online_wait_secs`).
# Connect as for `dev-r2`: `VOXEL_LOGIN_URL=http://127.0.0.1:8790 VOXEL_LOGIN_REFRESH_FILE=$PWD/target/dev-login/refresh
# ./target/debug/client debug online` (no screens), or `VOXEL_LOGIN_URL=http://127.0.0.1:8790 just play`.
dev-r3 hosts="2" delay="0": (_dev-hosts-check "0" hosts delay "2") kc-up pg-up _dev-login-setup (_dev-token dev_r3_dir)
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    # scripts/dev_r3.sh starts the login service from this environment.
    export VOXEL_OIDC_ISSUER={{dev_oidc_issuer}}
    {{cap}} {{cargo}} build --bin server --bin worldctl
    ./target/debug/worldctl clock-reset {{dev_database_url}}
    DEV_R3_PIN_WAIT_SECS={{online_wait_secs}} exec scripts/dev_r3.sh {{dev_r3_dir}} \
        '{{ shell(dev_r3_coordinator_sh, dev_r3_dir, dev_database_url) }}' \
        {{ shell(dev_r3_launch_sh, hosts, delay, dev_r3_dir, dev_database_url, host_env_sh) }}

# `go-nogo-r3`'s own state (logs, login, pins) and its own world database on the dev Postgres.
go_nogo_dir := "target/go-nogo-r3"
go_nogo_db := "mmo_go_nogo_r3"
go_nogo_db_url := "postgres://mmo:mmo@127.0.0.1:55432/" + go_nogo_db

# The R3 go / no-go probe (the plan's decision 13). A benchmark on this box: run it on
# purpose, with nothing else heavy running. A release build at play region size; its OWN
# world database `mmo_go_nogo_r3` on the dev Postgres (started as by `pg-up`), dropped and forged
# again by `worldctl` on EVERY run (so no earlier run's hosts leave successions behind) with
# super-regions of ONE region (two hosts then own a patchwork of single regions), and its
# own login state, pins and logs under target/go-nogo-r3/: never the dev world `mmo`,
# target/dev-r3 or target/dev-login. Refuses to start while anything holds the dev stack's
# ports (a `dev-r3`, `dev-r2`, `dev-login`, `run-online` or `server` up). Runs the two-host
# stack through scripts/dev_r3.sh (release binaries, `claim` ownership like `dev-r3`; the autopilot
# picks its seams from the client's own region -> host map, never from host ids; a succession
# inside the shuttle's window moves the seam, so a host or coordinator log that shows one
# there makes the run INVALID; every peer frame held `delay` ms), then
# `client debug online` twice under the autopilot shuttle, `seconds` each: across a seam
# between two region threads of one host (`boundary=thread`), then across a seam between
# the two hosts (`boundary=host`). Stops the stack (each host's leave cut; Ctrl-C at any
# point stops it too, a second Ctrl-C kills it), then prints the adapter, region size,
# delay, hosts and host restarts, both p95 tables by host_flip and `VERDICT: GO | NO-GO |
# INVALID` (`scripts/crossing_summary.py --go-nogo`, also in target/go-nogo-r3/verdict.txt):
# GO iff the host seam's p95 of correction_max and of corrections is at most 1.25 x the
# thread seam's + 0.05 AND conservation held (no state-hash mismatch; items_destroyed and
# intent_replies_lost 0 on every [server] line; peer_unreceived not above 0 on both of a
# host's last two); INVALID when a host restarted, any server-side process panicked or
# ended, the stack fell over before the stop or did not stop cleanly (a KILL: a host may
# have hung), a host logged no counters or stopped logging them before the end, a client
# lost its session early or never had its state hash checked, or a group has < 20 crossings.
# Exit code: 0 GO, 1 NO-GO, 2 INVALID, anything else: no verdict (the recipe failed).
# A run that did not stop cleanly (e.g. a second Ctrl-C) may leave processes behind: the
# next run says so at its start (its database is forged again, so no dirty row carries over)
# — check for leftover server / login / client processes before trusting it.
# Arguments are positional: `just go-nogo-r3 5`, `just adapter=6700 go-nogo-r3 2 90`.
# Go / no-go for host seams: thread-seam vs host-seam shuttle on 2 hosts at `delay` ms.
go-nogo-r3 delay="1" seconds="60": (_dev-token go_nogo_dir / "stack")
    #!/usr/bin/env bash
    set -euo pipefail
    # Bad arguments are no verdict (3), never NO-GO (1). (The host words below are built
    # with delay 0 when it is not a number, so that this check, not just, refuses it.)
    if ! [[ "{{delay}}" =~ ^[0-9]+$ ]]; then
        echo "go-nogo-r3: delay is a whole number of ms, given positionally (just go-nogo-r3 5, not delay=5)" >&2; exit 3
    fi
    if ! [[ "{{seconds}}" =~ ^[0-9]+$ ]] || (( {{seconds}} < 10 )); then
        echo "go-nogo-r3: seconds is a whole number >= 10, given positionally (just go-nogo-r3 1 60)" >&2; exit 3
    fi
    out={{go_nogo_dir}}
    stack=$out/stack
    db={{go_nogo_db}}
    # No verdict (exit 3) unless the verdict script ran: 1 and 2 are NO-GO and INVALID.
    verdict_rc=""
    no_verdict() { local rc=${1:-$?}; if [ -z "$verdict_rc" ] && [ "$rc" -le 2 ]; then rc=3; fi; exit "$rc"; }
    trap no_verdict EXIT
    busy=""
    for p in 6466 6467 6476 6477 6480 6490 6500 6501; do
        if ss -Hlun "sport = :$p" | grep -q .; then busy="$busy udp/$p"; fi
    done
    if ss -Hltn 'sport = :8790' | grep -q .; then busy="$busy tcp/8790"; fi
    if [ -n "$busy" ]; then
        echo "go-nogo-r3: in use:$busy - a dev stack (dev-r3, dev-r2, dev-login, run-online, server) is up; stop it first" >&2
        exit 1
    fi
    echo "go-nogo-r3: uses its OWN world database '$db' on the dev Postgres (dropped and forged again on"
    echo "  every run: 1-region super-regions), login state under $out/login and"
    echo "  pins under $stack. Resets: that world's clock and the logs under $out/. Never touches the dev world"
    echo "  'mmo', target/dev-r3 or target/dev-login."
    if [ -e "$out/unclean" ]; then
        echo "go-nogo-r3: NOTE: the previous run's stack did not stop cleanly ($(cat "$out/unclean")): check for"
        echo "  leftover server / login / client processes (this run's database is forged again: no dirty row carries over)."
    fi
    # The dev Postgres (as `pg-up`, without printing its URL).
    docker compose -f deploy/dev/compose.yml up -d --wait postgres
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    {{cap}} {{cargo}} build --release --bin server --bin worldctl --bin login
    {{cap}} {{cargo}} build --release --bin client --features "{{probe_features}}" --target-dir {{probe_dir}}
    mkdir -p "$out/login" "$stack"
    rm -f "$out"/client-*.log "$out/stack.log" "$out/verdict.txt" "$stack"/host-*.log "$stack/coordinator.log"
    psql() { docker compose -f deploy/dev/compose.yml exec -T postgres psql -U mmo -v ON_ERROR_STOP=1 -tA "$@"; }
    psql -d mmo -c "DROP DATABASE IF EXISTS $db WITH (FORCE)" >/dev/null
    psql -d mmo -c "CREATE DATABASE $db" >/dev/null
    echo "go-nogo-r3: forging a world into '$db' (every run; takes minutes)"
    ./target/release/worldctl forge --db {{go_nogo_db_url}}
    ./target/release/worldctl super-region {{go_nogo_db_url}} 1
    ./target/release/worldctl clock-reset {{go_nogo_db_url}}
    export VOXEL_LOGIN_DB=$out/login/login.sqlite VOXEL_LOGIN_KEYS_DIR=$out/login/keys
    login=./target/release/login
    if ! ls "$VOXEL_LOGIN_KEYS_DIR"/*.pem >/dev/null 2>&1; then $login keygen >/dev/null; fi
    # No addresses: the script below re-adds the world with the coordinator's login link, and each host's ack names its endpoint.
    $login world add dev --name "Dev world" >/dev/null
    # A fresh login token for a probe client (its own family each time: the client rotates the
    # token in the file). Each step is its own statement; a failure ends the recipe with 3 (no verdict).
    issue_token() {
        mkdir -p "$out/login" \
            && $login account issue-refresh {{dev_email}} >"$out/login/refresh.tmp" \
            && chmod 600 "$out/login/refresh.tmp" \
            && mv "$out/login/refresh.tmp" "$out/login/refresh" \
            || { echo "go-nogo-r3: could not issue a login token" >&2; exit 3; }
    }
    # Every host and the coordinator (the world row says 1; a host refuses a mismatch).
    export VOXEL_SUPER_REGION_REGIONS=1
    stack_pid=""
    stack_rc=""
    # Running, not a zombie.
    alive() { kill -0 "$1" 2>/dev/null && [[ "$(ps -o stat= -p "$1" 2>/dev/null)" != Z* ]]; }
    stop_stack() {
        trap 'kill -TERM "$stack_pid" 2>/dev/null || true' INT TERM HUP
        if [ -n "$stack_pid" ] && alive "$stack_pid"; then
            echo "go-nogo-r3: stopping the stack (each host saves a leave cut; Ctrl-C again kills it)"
            kill -TERM "$stack_pid" 2>/dev/null || true
        fi
        if [ -n "$stack_pid" ]; then
            # The supervisor's own status (a wait cut short by a signal leaves it running).
            while :; do
                stack_rc=0
                wait "$stack_pid" 2>/dev/null || stack_rc=$?
                if ! kill -0 "$stack_pid" 2>/dev/null; then break; fi
            done
            if [ "$stack_rc" = 0 ]; then rm -f "$out/unclean"; else echo "supervisor exit $stack_rc" >"$out/unclean"; fi
        fi
        stack_pid=""
    }
    trap 'rc=$?; stop_stack; no_verdict "$rc"' EXIT
    trap 'exit 130' INT TERM HUP
    # The supervisor in a session of its own: only stop_stack signals it (once; twice kills).
    # Cleared once it stops with status 0 (stop_stack); left behind, the next run warns.
    echo "the stack did not stop" >"$out/unclean"
    DEV_R3_SERVER=./target/release/server DEV_R3_LOGIN=$login DEV_R3_LOGIN_LOG=$out/login/login.log \
        DEV_R3_PIN_WAIT_SECS={{online_wait_secs}} setsid scripts/dev_r3.sh "$stack" \
        '{{ shell(dev_r3_coordinator_sh, go_nogo_dir / "stack", go_nogo_db_url) }}' \
        {{ shell(dev_r3_launch_sh, "2", if delay =~ '^[0-9]+$' { delay } else { "0" }, go_nogo_dir / "stack", go_nogo_db_url, host_env_sh) }} \
        >"$out/stack.log" 2>&1 </dev/null &
    stack_pid=$!
    echo "go-nogo-r3: starting login, the coordinator and 2 hosts (peer delay {{delay}} ms; $out/stack.log)"
    deadline=$((SECONDS + {{online_wait_secs}} + 60))
    until grep -q "world dev registered" "$out/stack.log" 2>/dev/null \
        && ss -Hlun 'sport = :6466' | grep -q . && ss -Hlun 'sport = :6476' | grep -q .; do
        if ! alive "$stack_pid"; then
            echo "go-nogo-r3: the stack ended; see $out/stack.log and $stack/" >&2; exit 1
        fi
        if ((SECONDS >= deadline)); then
            echo "go-nogo-r3: the stack is not up in time; see $out/stack.log and $stack/" >&2; exit 1
        fi
        sleep 1
    done
    sleep 5
    run_client() {
        issue_token
        echo "go-nogo-r3: client, autopilot boundary=$1, {{seconds}} s ($out/client-$1.log)"
        VOXEL_AUTOPILOT=shuttle VOXEL_AUTOPILOT_BOUNDARY=$1 VOXEL_AUTOPILOT_SECONDS={{seconds}} \
            PERF_LOG=1 RUST_BACKTRACE=1 \
            VOXEL_LOGIN_URL=http://127.0.0.1:8790 VOXEL_LOGIN_REFRESH_FILE=$PWD/$out/login/refresh \
            timeout --foreground -k 10 $(( {{seconds}} + {{online_wait_secs}} )) \
            ./{{probe_dir}}/release/client debug online >"$out/client-$1.log" 2>&1 </dev/null \
            || echo "go-nogo-r3: the $1 client exited $?"
    }
    run_client thread
    sleep 5
    run_client host
    echo "go-nogo-r3: letting the hosts settle (15 s: their last [server] lines come after the client left)"
    sleep 15
    # Whether the stack was still whole: the supervisor running, and where its output stood.
    stack_alive=0
    if alive "$stack_pid"; then stack_alive=1; fi
    stop_line=$(wc -l <"$out/stack.log")
    stop_stack
    trap no_verdict EXIT
    trap - INT TERM HUP
    echo
    echo "== go / no-go facts"
    echo "adapter:     $(grep -h -m1 AdapterInfo "$out/client-thread.log" "$out/client-host.log" 2>/dev/null | head -1 || true)"
    echo "region size: {{play_blocks}} blocks"
    echo "peer delay:  {{delay}} ms one way (VOXEL_PEER_LINK_DELAY_MS)"
    echo "hosts:       2, super-regions of 1 region; {{seconds}} s per shuttle"
    echo "stack:       supervisor $([ "$stack_alive" = 1 ] && echo "running" || echo "ENDED EARLY") at the stop, exit $stack_rc after it"
    echo "restarts:    supervisor $(grep -c 'starting it again' "$out/stack.log" || true);" \
        "host-0 started $(grep -c '\[server\] role:' "$stack/host-0.log" || true)x," \
        "host-1 started $(grep -c '\[server\] role:' "$stack/host-1.log" || true)x (more than once: INVALID)"
    server_logs=("$stack"/host-*.log)
    echo "logs:        $out/client-thread.log $out/client-host.log ${server_logs[*]} $stack/coordinator.log"
    server_args=()
    for f in "${server_logs[@]}"; do server_args+=(--server-log "$f"); done
    # The script's own code (0 GO, 1 NO-GO, 2 INVALID, 3 none), not tee's: 3 until it ran.
    verdict_rc=3
    set +e
    scripts/crossing_summary.py --go-nogo --thread-log "$out/client-thread.log" --host-log "$out/client-host.log" \
        "${server_args[@]}" \
        --aux-log "$stack/coordinator.log" --aux-log "$out/login/login.log" \
        --stack-log "$out/stack.log" --stack-stop-line "$stop_line" --stack-alive "$stack_alive" \
        --stack-exit "${stack_rc:-1}" | tee "$out/verdict.txt"
    verdict_rc=${PIPESTATUS[0]}
    exit "$verdict_rc"

# Run exactly the tests a change touches, by crate + cargo-test arguments:
#   just test-one game "--test it lod_streaming::pristine"
#   just test-one client "--lib world_scale::"
# Naming `--lib` / `--test <target>` matters: it skips building (and linking)
# every other test binary in the crate. Most integration tests live in ONE
# binary per crate, `--test it` (tests/it.rs); filter it by file name +
# `::`. The few files that need their own process keep their own target
# (listed at the top of each tests/it.rs).
test-one crate *args:
    {{cap}} {{cargo}} test -p {{crate}} {{args}}

# Run the world-store tests against the dev Postgres (needs `just pg-up`).
test-pg *args:
    MMO_TEST_DATABASE_URL=postgres://mmo:mmo@127.0.0.1:55432/postgres just test-one world-store "{{args}}"

# Run the login tests against the dev Postgres (needs `just pg-up`).
test-login-pg *args:
    MMO_TEST_DATABASE_URL=postgres://mmo:mmo@127.0.0.1:55432/postgres just test-one login "{{args}}"

# The split-roles end-to-end tests (crates/server/tests/roles.rs, all slow): a coordinator
# and a host over a real service link on localhost, against the dev Postgres (`pg-up`).
test-r2:
    MMO_TEST_DATABASE_URL=postgres://mmo:mmo@127.0.0.1:55432/postgres just test-one server "--features test-hooks --test roles -- --include-ignored"

# The multi-host end-to-end tests (crates/server/tests/hosts.rs, all slow): a coordinator
# and several hosts over real service and peer links on localhost, against the dev
# Postgres (`pg-up`). Serial: each test already runs several hosts' region threads.
test-r3:
    MMO_TEST_DATABASE_URL=postgres://mmo:mmo@127.0.0.1:55432/postgres just test-one server "--features test-hooks --test hosts -- --include-ignored --test-threads=1"

# The takeover crash table (crates/server/tests/takeover.rs, all slow): hosts under `claim`
# with a lease shortened for the test, a real coordinator declaring successions, an heir
# adopting, every crash point of R4's table, against the dev Postgres (`pg-up`). Serial.
test-r4:
    MMO_TEST_DATABASE_URL=postgres://mmo:mmo@127.0.0.1:55432/postgres just test-one server "--features test-hooks --test takeover -- --include-ignored --test-threads=1"

# Region threading: a player walking across the seam between two regions that
# run on different threads. Two layers, both tagged slow (so smoke / test-one /
# test skip them and only the long `test-full` sweep ran them):
#   - game `handoff_schedule`: the walk there and back under SEEDED scheduling
#     jitter (permuted tick order, late inputs, missed ticks) — a failure
#     names a seed that replays exactly — plus the rest of the game's seam
#     tests: the manager's handoff (`handoff_manager`), the halo, intents,
#     mining / placing / chests / piles across a seam, slimes, people and
#     bodies crossing, body arrivals, and entities crossing between hosts over
#     the peer session (`peer_session`, `peer_schedule`) and the peer journal
#     (`peer_journal`) (`seam_tests` below;
#     `voxel_body` is its own target);
#   - server `threaded_world`: the same walk on real OS threads. Serial on
#     purpose: each test already spawns 9+ region threads.
# ~80 s–2 min warm (game ~45 s, the real-thread layer ~35 s, measured 2026-11-07). Run after
# touching handoff, the WorldManager, region_threads or anything that crosses
# a seam. The client-side twin (bit-exact convergence under late scheduling)
# needs a Bevy-sized test binary, so it stays an explicit opt-in:
#   just test-one client "--test it -- --include-ignored crossing::crossing_converges_under_late_region"
seam_tests := "handoff_schedule:: handoff_manager:: peer_session:: peer_schedule:: peer_journal:: halo:: across_seam across_the_seam crosses_seam intents::intent_to_unloaded_owner mining::cooldown_does_not_cross item_piles::pickup_from_neighbour_pile item_piles::deposit_outside_region handoff_state::dynamic_arrival handoff_state::replayed_dynamic_arrival handoff_state::sleeping_body_arrives_asleep"
test-threads:
    {{cap}} {{cargo}} test -p game --test it -- --include-ignored {{seam_tests}}
    {{cap}} {{cargo}} test -p game --test voxel_body -- --include-ignored body_leaving_region drops_travel_with_a_crossing_body replayed_voxel_body_arrival
    {{cap}} {{cargo}} test -p server --test threaded_world -- --include-ignored --test-threads=1

# Cross-target determinism: the same physics scene must end in the same state
# on native and on wasm32 (node via wasm-bindgen-test-runner) —
# crates/game/tests/wasm_determinism.rs (a region scene and an underground dig) and the
# rapier/parry forks' StateHash over every journaled structure,
# crates/physics-next/tests/state_hash_wasm.rs. The runner must match Cargo.lock's
# wasm-bindgen exactly, so the first run installs that version under
# target/tools (needs node; ~1 min cold for the wasm build).
determinism-wasm:
    #!/usr/bin/env bash
    set -euo pipefail
    ver=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' Cargo.lock)
    root="$PWD/target/tools/wasm-bindgen-$ver"
    [ -x "$root/bin/wasm-bindgen-test-runner" ] || {{cap}} cargo install --locked wasm-bindgen-cli --version "$ver" --root "$root"
    {{cap}} {{cargo}} test -p game --test wasm_determinism
    {{cap}} {{cargo}} test -p physics-next --test state_hash_wasm
    CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$root/bin/wasm-bindgen-test-runner" \
        {{cap}} {{cargo}} test -p game --test wasm_determinism --target wasm32-unknown-unknown
    CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$root/bin/wasm-bindgen-test-runner" \
        {{cap}} {{cargo}} test -p physics-next --test state_hash_wasm --target wasm32-unknown-unknown

# The Slint screens' embedded sprites draw on wasm32 (node), where Slint would
# hand them to the browser and the software renderer panicked
# (crates/slint-ui/tests/wasm_images.rs, src/wasm_images.rs). Run it after a
# Slint bump or a change to crates/slint-ui/build.rs. Same runner as
# determinism-wasm.
slint-wasm:
    #!/usr/bin/env bash
    set -euo pipefail
    ver=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' Cargo.lock)
    root="$PWD/target/tools/wasm-bindgen-$ver"
    [ -x "$root/bin/wasm-bindgen-test-runner" ] || {{cap}} cargo install --locked wasm-bindgen-cli --version "$ver" --root "$root"
    CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$root/bin/wasm-bindgen-test-runner" \
        {{cap}} {{cargo}} test -p slint-ui --test wasm_images --target wasm32-unknown-unknown

# The browser client's page → sim worker server-frame codec on wasm32 (node):
# crates/client/tests/web_frame_wasm.rs (natively: `just test-one client "--lib web_frame::"`).
# Builds the client lib for wasm32 (slow cold). Same runner as determinism-wasm.
client-wasm:
    #!/usr/bin/env bash
    set -euo pipefail
    ver=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' Cargo.lock)
    root="$PWD/target/tools/wasm-bindgen-$ver"
    [ -x "$root/bin/wasm-bindgen-test-runner" ] || {{cap}} cargo install --locked wasm-bindgen-cli --version "$ver" --root "$root"
    CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$root/bin/wasm-bindgen-test-runner" \
        {{cap}} {{cargo}} test -p client --test web_frame_wasm --target wasm32-unknown-unknown

# Upstream rapier's own determinism tests, run from the vendored rapier 0.36
# fork (docs/superpowers/plans/2026-09-22-rapier-035-upgrade.md),
# plus upstream's regression tests for the code the fork patches (shape casts,
# voxels, the character controller). Every `// mmo-fork:` patch must keep these
# green. thread_count_determinism is `parallel`-gated and we never enable that
# feature, so it is not listed.
physics-next-upstream:
    {{cap}} {{cargo}} test -p rapier3d --features enhanced-determinism,serde-serialize \
        --test snapshot_roundtrip --test snapshot_portability \
        --test simd_backend_parity --test simd_backend_determinism \
        --test issue_485_kcc_depenetration --test issue_862_kcc_impulse_point
    {{cap}} {{cargo}} test -p parry3d \
        --test issue_106_shape_cast_status_consistency --test issue_193_shape_cast_penetrating_normals \
        --test issue_26_shape_cast_target_distance --test issue_373_voxels_shape_cast_witness \
        --test issue_404_voxels_manifold_prediction --test issue_429_shape_cast_toi_accuracy \
        --test sub_shape_id --test two_sided_internal_edges --test lib

# ---------------------------------------------------------------------------
# The k3d twin: a local single-node k3s (k3d, k3s in docker; cluster `mmo`, kube
# context `k3d-mmo`) shaped like the orion server — k3d's bundled Traefik and
# local-path stay on — with the OpenTofu roots under deploy/envs/local applied:
# platform (the CloudNativePG operator) -> postgres (cluster `pg`, world `twin`) ->
# login (no ECR, no Keycloak) -> world (namespace world-twin: a coordinator, two
# hosts, toolbox, web). It runs the release images, never a client. Everyday work
# does NOT need it (`run` stays native); it proves the deployment modules and the
# server in pods (`twin-check`). k3d is a pinned binary under target/tools
# (scripts/k3d.sh); the roots' state is under target/twin/ and as throwaway as the
# cluster. The orion world recipes work on it too:
# `just world_ctx=k3d-mmo world_ns=world-twin world-status`.
tofu := "tofu"
twin_ctx := "k3d-mmo"

# Create (or start) the twin. Ports on 127.0.0.1 only: the hosts' UDP node ports
# 30466-30469 and Traefik's port 80 as 8480 (http://game.localhost:8480/). Kubelet
# eviction thresholds are ABSOLUTE: the default "<15% disk free" taints a large,
# fairly full disk with disk-pressure and nothing schedules. The kubeconfig entry is
# added to the default kubeconfig WITHOUT switching the current context.
twin-up:
    #!/usr/bin/env bash
    set -euo pipefail
    if scripts/k3d.sh cluster get mmo >/dev/null 2>&1; then
        scripts/k3d.sh cluster start mmo
    else
        scripts/k3d.sh cluster create mmo --servers 1 --agents 0 \
            --api-port 127.0.0.1:16550 \
            -p '127.0.0.1:30466-30469:30466-30469/udp@server:0' \
            -p '127.0.0.1:8480:80@loadbalancer' \
            --k3s-arg '--kubelet-arg=eviction-hard=imagefs.available<5Gi,nodefs.available<5Gi,memory.available<500Mi@server:0' \
            --k3s-arg '--kubelet-arg=eviction-minimum-reclaim=imagefs.available=1Gi,nodefs.available=1Gi@server:0' \
            --kubeconfig-update-default --kubeconfig-switch-context=false --wait
    fi
    kubectl --context {{twin_ctx}} wait --for=condition=Ready node --all --timeout=180s
    echo "twin: cluster mmo is up (context {{twin_ctx}}); next: just twin-images && just twin-apply"

# Build the release images (world-images-build, orion-login-build) as mmo/server:twin,
# mmo/web:twin and mmo/login:twin, each from a staging directory as the push scripts
# do, and import them into the twin's node (no registry).
twin-images: world-images-build orion-login-build
    #!/usr/bin/env bash
    set -euo pipefail
    stage=$(mktemp -d)
    trap 'rm -rf "$stage"' EXIT
    mkdir "$stage/server" "$stage/web" "$stage/login"
    cp target/release/server target/release/worldctl deploy/images/server/Dockerfile "$stage/server/"
    cp -R target/web "$stage/web/web"
    cp deploy/images/web/Dockerfile deploy/images/web/nginx.conf "$stage/web/"
    cp target/release/login deploy/images/login/Dockerfile "$stage/login/"
    for name in server web login; do
        echo "twin: building mmo/$name:twin"
        docker build -q -t "mmo/$name:twin" "$stage/$name" > /dev/null
    done
    scripts/k3d.sh image import mmo/server:twin mmo/web:twin mmo/login:twin -c mmo
    echo "twin: images imported; running pods keep their old image until they are re-created"

# Apply the four local roots in order (platform -> postgres -> login -> world), without
# asking: the cluster is a throwaway. Each root's .terraform lives under target/twin/.
twin-apply:
    #!/usr/bin/env bash
    set -euo pipefail
    for r in platform postgres login world; do
        echo "twin: applying deploy/envs/local/$r"
        export TF_DATA_DIR="{{justfile_directory()}}/target/twin/$r/.terraform"
        {{tofu}} -chdir=deploy/envs/local/$r init -input=false -reconfigure > /dev/null
        {{tofu}} -chdir=deploy/envs/local/$r apply -input=false -auto-approve
        if [ "$r" = postgres ]; then
            # The operator creates the database after the apply returns; pods started before
            # that exit on "database world_twin does not exist" and crash-loop until it does.
            echo "twin: waiting for the operator to create database world_twin"
            kubectl --context {{twin_ctx}} -n postgres wait database/world-twin --for=jsonpath='{.status.applied}'=true --timeout=300s
        fi
    done
    echo "twin: applied; the world waits for its forge: just twin-check (forges and checks), or just world_ctx={{twin_ctx}} world_ns=world-twin world-ctl forge"

# The twin's integration check (scripts/twin_check.sh): pods wait for a forge, serve after
# it, the hosts' PodDisruptionBudgets, survive a killed host (a new id), a clean host leave,
# a coordinator restart, a drain (world-drain: drained, not restarted) and a host brought back,
# a world stop and start; records memory. Run on a FRESH twin-apply (it forges the world). Exit 0 = pass.
twin-check:
    TWIN_CONTEXT={{twin_ctx}} scripts/twin_check.sh

# Delete the twin's cluster (its database included) and the roots' local state.
twin-down:
    -scripts/k3d.sh cluster delete mmo
    rm -f target/twin/*.tfstate target/twin/*.tfstate.backup

# kubectl against the twin, e.g. `just twin-kube get pods -A`.
[positional-arguments]
twin-kube *args:
    kubectl --context {{twin_ctx}} "$@"

# ---------------------------------------------------------------------------
# The orion k3s server (Hetzner 5.9.7.138, kube context `orion`): OpenTofu roots
# under deploy/envs/orion — `bootstrap` (the S3 state bucket), `platform`
# (cert-manager, Traefik settings), `dns` (the renownandruin.com records no module makes), `garage` (the S3-compatible object store on the
# server itself: https://s3.errantry.renownandruin.com) and `garage-buckets` (its buckets
# and access keys, through the admin API: needs `just orion-garage-admin` running),
# `monitoring` (Grafana, Prometheus + Thanos, Loki, Tempo, Pyroscope, Alloy; data in
# Garage), `picoshare` (file sharing at
# https://share.errantry.renownandruin.com), `zulip` (team chat at
# https://chat.errantry.renownandruin.com; uploads in Garage, its own PostgreSQL), `keycloak`
# (identity provider at https://auth.errantry.renownandruin.com) and `keycloak-realm` (its realm,
# clients and mail, through the admin API: needs `just orion-keycloak-admin` running), `registry`
# (the ECR repository mmo/login and the cluster's pull-only IAM user), `login` (the
# game's login service at https://login.errantry.renownandruin.com), `postgres` (the CloudNativePG cluster
# `pg` and its nightly dump to Garage), `world` (one game world: errantry) and `site` (the website at
# https://www.errantry.renownandruin.com, from website/). State lives in AWS S3 (deploy/envs/orion/backend.s3.tfbackend,
# key orion/<root>.tfstate); AWS credentials come from the default chain
# (`AWS_PROFILE=…` to pick a profile). Every recipe pins the `orion` context,
# never the current one. Plan before every apply. Design:
# docs/superpowers/plans/2026-11-06-k3s-observability-stack-handoff.md.
orion_context := "orion"

# `tofu init` an orion root (bootstrap | platform | dns | garage | garage-buckets | monitoring | picoshare | zulip | vpn-names | keycloak | keycloak-realm | registry | login | postgres | world | site) against its S3 state.
# Extra args pass through, e.g. `just orion-init monitoring -upgrade`.
[positional-arguments]
orion-init root *args:
    shift; {{tofu}} -chdir=deploy/envs/orion/{{root}} init -input=false \
        -backend-config=../backend.s3.tfbackend -backend-config=key=orion/{{root}}.tfstate "$@"

# Show what applying an orion root would change.
[positional-arguments]
orion-plan root *args:
    shift; {{tofu}} -chdir=deploy/envs/orion/{{root}} plan -input=false -var kube_context={{orion_context}} "$@"

# Apply an orion root (interactive: tofu shows the plan and asks). Order on a
# fresh server: platform, then garage -> garage-buckets -> monitoring (its data is
# in Garage; the first garage apply runs before monitoring exists, so with
# `-var metrics=false`), then picoshare (any time) and
# keycloak -> keycloak-realm -> zulip (Zulip logs in through Keycloak, and keeps its files in Garage)
# and registry -> login (the issuer comes from the realm's state, the image and the pull user from
# registry's; `just orion-login-push` BEFORE login's first apply)
# and site (the website reads the realm's issuer).
[positional-arguments]
orion-apply root *args:
    shift; {{tofu}} -chdir=deploy/envs/orion/{{root}} apply -var kube_context={{orion_context}} "$@"

# The bootstrap root has no kubernetes provider (no kube_context variable).
orion-bootstrap-plan:
    {{tofu}} -chdir=deploy/envs/orion/bootstrap plan -input=false

# kubectl against the orion server, e.g. `just orion-kube -n monitoring get pods`.
[positional-arguments]
orion-kube *args:
    kubectl --context {{orion_context}} "$@"

# Grafana without the public Ingress: http://127.0.0.1:3000 (port-forward).
orion-grafana port="3000":
    kubectl --context {{orion_context}} -n monitoring port-forward svc/kube-prometheus-stack-grafana {{port}}:80

# Print the Grafana `admin` password (from the monitoring root's state).
orion-grafana-password:
    @{{tofu}} -chdir=deploy/envs/orion/monitoring output -raw grafana_admin_password; echo

# Print PicoShare's upload passphrase (from the picoshare root's state).
orion-share-password:
    @{{tofu}} -chdir=deploy/envs/orion/picoshare output -raw shared_secret; echo

# Print a one-time link that creates a Zulip organisation (and its first owner)
# without any e-mail being sent. Needs the zulip root applied and `zulip-0` ready.
orion-zulip-realm-link:
    kubectl --context {{orion_context}} -n zulip exec zulip-0 -c zulip -- \
        runuser -u zulip -- /home/zulip/deployments/current/manage.py generate_realm_creation_link

# Send Zulip's test e-mail to an address (checks the SES / SMTP settings end to end).
orion-zulip-test-email to:
    kubectl --context {{orion_context}} -n zulip exec zulip-0 -c zulip -- \
        runuser -u zulip -- /home/zulip/deployments/current/manage.py send_test_email {{to}}

# Garage's admin API is not public: over the operator VPN it is
# http://garage.garage.svc.cluster.local:3903 (what the garage-buckets root uses).
# VPN down: this forwards it to http://127.0.0.1:3903; then plan/apply garage-buckets
# with `-var garage_admin_url=http://127.0.0.1:3903`.
orion-garage-admin:
    kubectl --context {{orion_context}} -n garage port-forward svc/garage 3903:3903

# The `garage` CLI inside the Garage pod, e.g. `just orion-garage status`,
# `just orion-garage bucket list`, `just orion-garage stats`.
[positional-arguments]
orion-garage *args:
    kubectl --context {{orion_context}} -n garage exec deploy/garage -- /garage "$@"

# One-time copy of a tenant's objects (telemetry | zulip) from its old AWS S3
# buckets into Garage; see the script. Run before the tenant's root is applied.
orion-garage-import tenant:
    TOFU={{tofu}} scripts/orion_garage_import.sh {{tenant}}

# Keycloak's admin console and admin API are not public: over the operator VPN they are
# https://keycloak.vpn.errantry.renownandruin.com/admin/ (user `admin`; what the
# keycloak-realm root uses). This forward (http://127.0.0.1:8180) serves the console only
# while the keycloak root runs WITHOUT admin_host (Keycloak is told its admin address).
orion-keycloak-admin:
    kubectl --context {{orion_context}} -n keycloak port-forward svc/keycloak 8180:8080

# Print the password of Keycloak's `admin` user (from the keycloak root's state).
orion-keycloak-admin-password:
    @{{tofu}} -chdir=deploy/envs/orion/keycloak output -raw admin_password; echo

# The login service on orion (deploy/modules/login, root deploy/envs/orion/login): the pods run
# the image mmo/login from ECR (deploy/images/login/Dockerfile: debian + the release binary;
# the registry root; one tag, `latest`, older images expire about a day after `latest` moves on).
# Releases are `kubectl set image` by digest, never an apply; the way back is
# `just orion-kube -n login rollout undo deploy/login` (the previous digest: while it is still in ECR,
# about a day, or in the node's cache).
# Release build of the login binary (what orion-login-push puts in the image). It must use the
# world's region size: the host link's hello refuses a login built at another VOXEL_REGION_BLOCKS.
orion-login-build:
    VOXEL_REGION_BLOCKS={{play_blocks}} {{cap}} {{cargo}} build --release -p login --bin login

# Build the image (labelled with the git revision), push it to ECR as mmo/login:latest and
# `kubectl set image` it on the login Deployment BY DIGEST (every push rolls out), waiting for the
# rollout (scripts/orion_login_push.sh). Before the login root exists it only pushes (the root's
# first apply starts from `latest`).
orion-login-push: orion-login-build
    KUBE_CONTEXT={{orion_context}} TOFU={{tofu}} scripts/orion_login_push.sh

# The game world's images (deploy/images/server, deploy/images/web): release `server` + `worldctl`
# and the browser bundle, both at the play region size (the size the world runs at). The bundle is
# built with no VOXEL_WEB_SERVER / VOXEL_LOGIN_URL: the web image's nginx serves it beside the
# login proxy, so the page needs neither.
world-images-build:
    #!/usr/bin/env bash
    set -euo pipefail
    # One revision for the binaries and the image labels: computed once, written beside the
    # build for scripts/orion_world_push.sh. assets/ counts (the shaders are compiled into the bundle).
    rev=$(git rev-parse --short HEAD)
    [ -z "$(git status --porcelain -- crates Cargo.lock assets)" ] || rev="$rev-dirty"
    mkdir -p target/world-images
    rm -f target/world-images/revision
    VOXEL_REGION_BLOCKS={{play_blocks}} VOXEL_BUILD_REVISION="$rev" {{cap}} {{cargo}} build --release -p server --bin server -p world-store --bin worldctl
    env -u VOXEL_WEB_SERVER -u VOXEL_LOGIN_URL VOXEL_REGION_BLOCKS={{play_blocks}} CAP="{{cap}}" scripts/web_release.sh
    printf '%s' "$rev" > target/world-images/revision

# Build both images, push them to ECR as mmo/server:latest and mmo/web:latest and print their digests
# (scripts/orion_world_push.sh). Pushes only: it never touches the cluster.
orion-world-push: world-images-build
    TOFU={{tofu}} scripts/orion_world_push.sh

# Stop-update-start release of a world (scripts/world_update.sh): every host Deployment to 0 (each host's
# leave line is printed; a host that left without a cut is WARNED about), new images by digest on
# coordinator, toolbox, every host Deployment and web, each host Deployment back to 1 (a failed run says
# how to go on: re-run it with the same two refs). The refs it replaces are recorded first in the world's ConfigMap
# release-history (what orion-world-rollback goes back to). e.g. `just orion-world-update <server-ref>
# <web-ref>` with the refs `orion-world-push` printed; `namespace=` picks the world's namespace.
orion-world-update server-digest web-digest namespace="world-errantry":
    scripts/world_update.sh {{orion_context}} {{namespace}} {{server-digest}} {{web-digest}}

# The login operator commands inside a login pod, where the database is,
# e.g. `just orion-login world list`, `just orion-login account issue-refresh <email>`.
[positional-arguments]
orion-login *args:
    kubectl --context {{orion_context}} -n login exec deploy/login -- login "$@"

# Follow the login service's log.
orion-login-logs:
    kubectl --context {{orion_context}} -n login logs -f deploy/login --tail 200

# One game world is one Kubernetes namespace. These recipes work on the world in
# `world_ctx` / `world_ns`; the twin reuses them: `just world_ctx=k3d-mmo world_ns=world-twin world-status`.
world_ctx := "orion"
world_ns := "world-errantry"

# worldctl inside the toolbox pod, where $VOXEL_WORLD_DB is. The URL is expanded in the pod and never
# printed. e.g. `just world-ctl forge --preset default`, `just world-ctl rollback --yes`. forge, retile
# and export take `--db <url>`; clock-reset, super-region, hosts, drain, retire-host and rollback take the URL first
# (the forms of worldctl's usage text). `export` writes its file inside the toolbox pod, where it is
# lost with the pod: copy it out with `kubectl cp` (a hint is printed).
[positional-arguments]
world-ctl *args:
    kubectl --context {{world_ctx}} -n {{world_ns}} exec deploy/toolbox -c toolbox -- sh -c 'cmd=$1; shift; case "$cmd" in forge|retile|export) exec worldctl "$cmd" --db "$VOXEL_WORLD_DB" "$@" ;; *) exec worldctl "$cmd" "$VOXEL_WORLD_DB" "$@" ;; esac' sh "$@"
    @if [ "$1" = export ]; then echo "world-ctl: the exported file is inside the toolbox pod; copy it out now: kubectl --context {{world_ctx}} -n {{world_ns}} cp <toolbox-pod>:<out.world> ./<out.world> -c toolbox"; fi

# The world's pods, its host Deployments (one per host, `host-<name>`), its Services' node ports, and
# each pod's /readyz body (through the API server's pod proxy to the health port 9100). A drained host
# (/readyz `drained`) is unready for good: the line says how to remove it. Waits on nothing.
world-status:
    #!/usr/bin/env bash
    set -euo pipefail
    k() { kubectl --context {{world_ctx}} -n {{world_ns}} "$@"; }
    k get pods -o wide
    echo
    k get deploy -l app.kubernetes.io/component=host -o custom-columns=HOST:.metadata.name,WANT:.spec.replicas,READY:.status.readyReplicas,IMAGE:.spec.template.spec.containers[0].image
    echo
    k get svc -o custom-columns=NAME:.metadata.name,TYPE:.spec.type,PORT:.spec.ports[*].port,NODEPORT:.spec.ports[*].nodePort
    echo
    # Only the coordinator and the hosts have the health port.
    for pod in $(k get pods -l 'app.kubernetes.io/component in (coordinator,host)' -o jsonpath='{.items[*].metadata.name}'); do
        echo "== $pod /readyz"
        # A 200's body is kubectl's output ("ok"). kubectl replaces a 503's body ("forge", "drained" or
        # "starting") with a generic error, but logs the body itself at -v=8. A pod that does not
        # answer gets the API server's Status object.
        if ! body=$(k get --raw "/api/v1/namespaces/{{world_ns}}/pods/$pod:9100/proxy/readyz" 2>/dev/null); then
            body=$(k get --raw "/api/v1/namespaces/{{world_ns}}/pods/$pod:9100/proxy/readyz" -v=8 2>&1 | sed -n 's/.*Response Body: //p' | tail -1 || true)
        fi
        case "$body" in
            ''|'{"kind":"Status"'*) echo "(no answer)" ;;
            drained)
                host=$(k get pod "$pod" -o jsonpath='{.metadata.labels.errantry/host}' 2>/dev/null || true)
                echo "drained: host ${host:-?} left the world and stays unready until removed: remove \"${host:-<name>}\" from var.hosts and apply (or \`kubectl rollout restart deployment/host-${host:-<name>}\` brings it back as a NEW host)" ;;
            *) echo "$body" ;;
        esac
        echo
    done

# A pod's /metrics through the API server's pod proxy: `host-<name>` (that host's pod), `coordinator`,
# or a pod name. e.g. `just world-metrics host-a`.
world-metrics target:
    #!/usr/bin/env bash
    set -euo pipefail
    k() { kubectl --context {{world_ctx}} -n {{world_ns}} "$@"; }
    t="{{target}}"
    case "$t" in
        coordinator) sel='app.kubernetes.io/component=coordinator' ;;
        host-*) sel="app.kubernetes.io/component=host,errantry/host=${t#host-}" ;;
        *) sel='' ;;
    esac
    pod=''
    # The pod of that label that is not being deleted; else the argument is a pod name.
    [ -z "$sel" ] || pod=$(k get pods -l "$sel" -o custom-columns=NAME:.metadata.name,DELETING:.metadata.deletionTimestamp --no-headers | awk '$2 == "<none>" { print $1; exit }')
    [ -n "$pod" ] || pod="$t"
    k get --raw "/api/v1/namespaces/{{world_ns}}/pods/$pod:9100/proxy/metrics"

# Follow a pod's log: `host-<name>` (that host's pod), `coordinator`, or a pod name.
# e.g. `just world-logs host-a`.
world-logs target:
    #!/usr/bin/env bash
    set -euo pipefail
    k() { kubectl --context {{world_ctx}} -n {{world_ns}} "$@"; }
    t="{{target}}"
    case "$t" in
        coordinator) sel='app.kubernetes.io/component=coordinator' ;;
        host-*) sel="app.kubernetes.io/component=host,errantry/host=${t#host-}" ;;
        *) sel='' ;;
    esac
    pod=''
    # The pod of that label that is not being deleted; else the argument is a pod name.
    [ -z "$sel" ] || pod=$(k get pods -l "$sel" -o custom-columns=NAME:.metadata.name,DELETING:.metadata.deletionTimestamp --no-headers | awk '$2 == "<none>" { print $1; exit }')
    [ -n "$pod" ] || pod="$t"
    k logs -f --tail 200 "$pod"

# The world's `hosts` rows (`worldctl hosts` in the toolbox): one line per row, `<id> <name> <live|dead>
# <clean|dirty> land=<n> seen=<ms|vouched> drain=<ms|->`. A host's id is issued when its pod registers:
# a restarted pod is a NEW id under the same name; an old row shows dead, and is gone after a later
# registration (once it owns nothing).
world-hosts:
    kubectl --context {{world_ctx}} -n {{world_ns}} exec deploy/toolbox -c toolbox -- sh -c 'exec worldctl hosts "$VOXEL_WORLD_DB"'

# Drain host <name> (a key of var.hosts): finds its live row in `worldctl hosts`, runs
# `worldctl drain <id> --in <secs>` (default 300; the host warns its players), follows its pod's log
# until the leave line (WORLD_DRAIN_TIMEOUT s after the deadline, default 180), prints it and the next
# step: remove "<name>" from var.hosts and apply, or `kubectl rollout restart deployment/host-<name>`
# to bring it back as a NEW host. Deletes nothing, scales nothing. Only a `claim` host acts on a mark:
# a world whose coordinator runs VOXEL_OWNERSHIP other than claim is refused up front.
world-drain name secs="300":
    #!/usr/bin/env bash
    set -euo pipefail
    k() { kubectl --context {{world_ctx}} -n {{world_ns}} "$@"; }
    name="{{name}}" secs="{{secs}}"
    case "$name" in ''|*[!a-z0-9-]*) echo "world-drain: '$name' is not a host name (a key of var.hosts)" >&2; exit 2 ;; esac
    case "$secs" in ''|*[!0-9]*) echo "world-drain: '$secs' is not a number of seconds" >&2; exit 2 ;; esac
    k get deploy "host-$name" > /dev/null || { echo "world-drain: no Deployment host-$name in {{world_ns}}; nothing changed" >&2; exit 2; }
    # Only a `claim` host acts on a mark: a `static` world (its one host) is refused up front.
    ownership=$(k get deploy/coordinator -o jsonpath='{.spec.template.spec.containers[?(@.name=="coordinator")].env[?(@.name=="VOXEL_OWNERSHIP")].value}') ||
        { echo "world-drain: cannot read the coordinator's VOXEL_OWNERSHIP; nothing changed" >&2; exit 2; }
    if [ "$ownership" != claim ]; then
        echo "world-drain: the world's ownership is '${ownership:-unset}', not claim: a static host is its world's only one and ignores a drain mark. Stop the world instead (just world-stop). Nothing changed." >&2
        exit 2
    fi
    rows=$(k exec deploy/toolbox -c toolbox -- sh -c 'exec worldctl hosts "$VOXEL_WORLD_DB"') ||
        { echo "world-drain: worldctl hosts failed; nothing changed" >&2; exit 1; }
    ids=$(printf '%s\n' "$rows" | awk -v n="$name" '$2 == n && $3 == "live" && $4 == "dirty" && $1 ~ /^[0-9]+$/ { print $1 }')
    if [ "$(printf '%s' "$ids" | grep -c .)" -ne 1 ]; then
        echo "world-drain: not exactly one live row named $name (a host that already left, or is not registered yet); nothing changed. The rows:" >&2
        printf '%s\n' "$rows" | sed 's/^/    /' >&2
        exit 1
    fi
    pod=$(k get pods -l "app.kubernetes.io/component=host,errantry/host=$name" -o custom-columns=NAME:.metadata.name,DELETING:.metadata.deletionTimestamp --no-headers | awk '$2 == "<none>" { print $1; exit }')
    [ -n "$pod" ] || { echo "world-drain: host $name has no running pod; nothing changed" >&2; exit 1; }
    log=$(mktemp)
    k logs -f --tail=0 "$pod" > "$log" 2>&1 &
    follower=$!
    trap 'kill $follower 2>/dev/null || true; rm -f "$log"' EXIT
    sleep 1
    echo "world-drain: host $name is id $ids (pod $pod); marking it to leave within $secs s"
    k exec deploy/toolbox -c toolbox -- sh -c 'exec worldctl drain "$VOXEL_WORLD_DB" "$1" --in "$2"' sh "$ids" "$secs"
    timeout=$((secs + ${WORLD_DRAIN_TIMEOUT:-180}))
    echo "world-drain: following $pod's log until it leaves (up to $timeout s; Ctrl-C stops only the wait, the drain stands)"
    deadline=$((SECONDS + timeout))
    while ! grep -qF -e '[server] drained:' -e 'left without a cut' "$log"; do
        if [ "$SECONDS" -ge "$deadline" ]; then
            echo "world-drain: after $timeout s no leave line in $pod's log; the drain stands. Look: just world_ctx={{world_ctx}} world_ns={{world_ns}} world-logs host-$name" >&2
            exit 1
        fi
        sleep 2
    done
    if grep -qF 'left without a cut' "$log"; then
        echo "world-drain: WARNING: host $name LEFT WITHOUT A CUT: $(grep -F 'left without a cut' "$log" | tail -1 | sed 's/.*\(\[server\]\)/\1/')" >&2
        echo "world-drain: the process exits and its Deployment restarts it as a NEW host: the drain is lost (the world treats id $ids as crashed)." >&2
        exit 1
    fi
    echo "world-drain: $(grep -F '[server] left the world in cut' "$log" | tail -1 | sed 's/.*\(\[server\] left the world in cut\)/\1/')"
    echo "world-drain: host $name (id $ids) is drained: /readyz says drained, it stays up until its pod is removed."
    echo "world-drain: next: remove \"$name\" from var.hosts and apply (its Deployment, Service and budget go),"
    echo "world-drain:   or bring it back as a NEW host: kubectl --context {{world_ctx}} -n {{world_ns}} rollout restart deployment/host-$name"

# Stop the world: every host Deployment (-l app.kubernetes.io/component=host) to 0 (each host asks for
# a leave cut and exits, bounded by about 90 s; a drained host just exits), wait until no host pod is
# left, print each host's leave line (WARNS when one left without a committed cut: the next start
# raises the world generation), then the coordinator to 0. Waits up to WORLD_STOP_TIMEOUT s (default 150).
world-stop:
    #!/usr/bin/env bash
    set -euo pipefail
    k() { kubectl --context {{world_ctx}} -n {{world_ns}} "$@"; }
    timeout="${WORLD_STOP_TIMEOUT:-150}"
    hostsel='app.kubernetes.io/component=host'
    # wait_gone <label selector>: no pod matches; a failed listing is retried, never read as empty.
    wait_gone() {
        local deadline=$((SECONDS + timeout)) listing left
        while :; do
            if listing=$(k get pods -l "$1" -o name 2>&1); then
                left=$(printf '%s\n' "$listing" | grep -c '^pod/' || true)
                [ "$left" -eq 0 ] && return 0
                what="$left pod(s) still there"
            else
                what="the pod listing failed ($(printf '%s' "$listing" | head -1))"
            fi
            if [ "$SECONDS" -ge "$deadline" ]; then
                echo "world-stop: after $timeout s: $what" >&2
                return 1
            fi
            sleep 2
        done
    }
    # With the coordinator already down the hosts cannot finish their leave cut: they would hang until
    # the grace period. Refuse before anything is scaled.
    coord=$(k get deploy/coordinator -o jsonpath='{.spec.replicas}') || { echo "world-stop: cannot read the coordinator; nothing changed" >&2; exit 2; }
    hostdeps=$(k get deploy -l "$hostsel" -o name) || { echo "world-stop: cannot list the host Deployments; nothing changed" >&2; exit 2; }
    hostpods=$(k get pods -l "$hostsel" -o name) || { echo "world-stop: cannot list the host pods; nothing changed" >&2; exit 2; }
    if [ "${coord:-0}" = 0 ] && printf '%s\n' "$hostpods" | grep -q '^pod/'; then
        echo "world-stop: the coordinator is at 0 replicas but host pods still exist; they cannot leave cleanly. Run \`just world-start\` first, or delete the host pods knowingly. Nothing changed." >&2
        exit 2
    fi
    # Before acting: a drained host and a host MARKED draining (not due yet) come back at the next
    # world-start as NEW hosts (a mark is lost). All warned at once, then one 10 s pause to stop.
    warned=0
    for pod in $(printf '%s\n' "$hostpods" | sed -n 's#^pod/##p'); do
        body=$(k get --raw "/api/v1/namespaces/{{world_ns}}/pods/$pod:9100/proxy/readyz" -v=8 2>&1 | sed -n 's/.*Response Body: //p' | tail -1 || true)
        [ "$body" = drained ] && { echo "world-stop: WARNING: $pod is DRAINED: world-start brings it back as a NEW host; to remove it, take its name out of var.hosts and apply" >&2; warned=1; }
    done
    if rows=$(k exec deploy/toolbox -c toolbox -- sh -c 'exec worldctl hosts "$VOXEL_WORLD_DB"' 2>/dev/null); then
        marked=$(printf '%s\n' "$rows" | awk '$3 == "live" && $4 == "dirty" && $NF != "drain=-" { print $2 " (id " $1 ")" }')
        if [ -n "$marked" ]; then
            printf '%s\n' "$marked" | sed 's/^/world-stop: WARNING: host /; s/$/ is MARKED draining: it stops now, its drain unfinished, and world-start brings it back as a NEW host with NO mark/' >&2
            warned=1
        fi
    elif printf '%s\n' "$hostpods" | grep -q '^pod/'; then
        echo "world-stop: could not read the hosts' drain marks (worldctl hosts in the toolbox). Going on." >&2
    fi
    if [ "$warned" -eq 1 ]; then
        echo "world-stop: stop here within 10 s (Ctrl-C) to act on the warnings above; nothing has changed yet." >&2
        sleep 10
    fi
    # Each host pod's log is followed until the pod ends: its leave line is read after it is gone.
    follow_dir=$(mktemp -d)
    follow_pids=()
    trap 'for p in "${follow_pids[@]}"; do kill "$p" 2>/dev/null || true; done; rm -rf "$follow_dir"' EXIT
    for pod in $(printf '%s\n' "$hostpods" | sed -n 's#^pod/##p'); do
        k logs -f --tail=20 "$pod" > "$follow_dir/$pod.log" 2>&1 &
        follow_pids+=("$!")
    done
    sleep 1
    if [ -n "$hostdeps" ]; then
        echo "world-stop: host Deployments to 0 ($(printf '%s\n' "$hostdeps" | sed 's#^deployment.apps/##' | tr '\n' ' ')), waiting until no host pod is left ($timeout s)"
        # shellcheck disable=SC2086
        k scale $hostdeps --replicas=0
    else
        echo "world-stop: the world has no host Deployment"
    fi
    wait_gone "$hostsel"
    sleep 1
    for p in "${follow_pids[@]}"; do kill "$p" 2>/dev/null || true; done
    # The leave of each host (the same lines scripts/world_update.sh reads): WARN, never abort.
    echo "world-stop: the hosts' leave:"
    [ "$(ls "$follow_dir")" ] || echo "    (no host pod was running)"
    no_cut='' unknown=''
    for f in "$follow_dir"/*.log; do
        [ -e "$f" ] || continue
        pod=$(basename "$f" .log)
        if grep -qF "left without a cut" "$f"; then
            echo "    $pod: LEFT WITHOUT A CUT: $(grep -F 'left without a cut' "$f" | tail -1 | sed 's/.*\(\[server\]\)/\1/')"
            no_cut="$no_cut $pod"
        elif grep -qF "stop signal while drained" "$f"; then
            # It left the world at its drain; world-start brings it back as a NEW host.
            echo "    $pod: drained earlier (it left the world at its drain): stopped. world-start starts it again as a NEW host; to remove it, take its name out of var.hosts and apply"
        elif grep -qF "[server] left the world in cut" "$f"; then
            echo "    $pod: $(grep -F '[server] left the world in cut' "$f" | tail -1 | sed 's/.*\(\[server\] left the world in cut\)/\1/')"
        else
            echo "    $pod: no leave line seen in its log"
            unknown="$unknown $pod"
        fi
    done
    if [ -n "$no_cut" ]; then
        echo "world-stop: ************************************************************************" >&2
        echo "world-stop: WARNING: host(s)$no_cut left WITHOUT a committed cut. The next start raises the" >&2
        echo "world-stop: world generation: the world rolls back to its last committed cut. Going on." >&2
        echo "world-stop: ************************************************************************" >&2
    fi
    if [ -n "$unknown" ]; then
        echo "world-stop: WARNING: no leave line seen for$unknown (its log could not be followed to the end); whether it left with a cut is not known. Going on." >&2
    fi
    echo "world-stop: coordinator to 0"
    k scale deploy/coordinator --replicas=0
    wait_gone 'app.kubernetes.io/component=coordinator'
    echo "world-stop: the world is down (web and toolbox keep running); \`just world-start\` brings it back"

# Start the world: coordinator to 1, wait until it is Ready, then every host Deployment
# (-l app.kubernetes.io/component=host) to 1, wait until each has readyReplicas 1 (never `rollout
# status`). A drained host (/readyz `drained`, unready for good) is not waited for: it is named, with
# how to remove it. Refused when the world has no host Deployment. WORLD_START_TIMEOUT s (default 300).
world-start:
    #!/usr/bin/env bash
    set -euo pipefail
    k() { kubectl --context {{world_ctx}} -n {{world_ns}} "$@"; }
    hostsel='app.kubernetes.io/component=host'
    names=$(k get deploy -l "$hostsel" -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}')
    count=$(printf '%s' "$names" | grep -c . || true)
    if [ "$count" -lt 1 ]; then
        echo "world-start: the world has no host Deployment ($hostsel); refusing to start" >&2
        exit 1
    fi
    echo "world-start: coordinator to 1"
    k scale deploy/coordinator --replicas=1
    k rollout status deploy/coordinator --timeout=5m
    echo "world-start: host Deployments to 1: $(printf '%s\n' "$names" | tr '\n' ' ')"
    for d in $names; do k scale "deploy/$d" --replicas=1; done
    # Not `rollout status`: a drained host is unready for good, and a host waiting for a succession
    # may be unready for minutes. Poll each Deployment's readyReplicas (WORLD_START_TIMEOUT s, default
    # 300); a failed read keeps waiting, never counts as ready. A host whose pod says `drained` is not
    # waited for.
    # readyz <pod>: the /readyz body, 200 or not (kubectl logs a 503's body at -v=8).
    readyz() { k get --raw "/api/v1/namespaces/{{world_ns}}/pods/$1:9100/proxy/readyz" -v=8 2>&1 | sed -n 's/.*Response Body: //p' | tail -1 || true; }
    timeout="${WORLD_START_TIMEOUT:-300}"
    deadline=$((SECONDS + timeout))
    while :; do
        ready=0 drained='' waiting=''
        for d in $names; do
            if r=$(k get "deploy/$d" -o jsonpath='{.status.readyReplicas}' 2>/dev/null) && [ "${r:-0}" = 1 ]; then
                ready=$((ready + 1))
                continue
            fi
            pods=$(k get pods -l "$hostsel,errantry/host=${d#host-}" -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || true)
            is_drained=0
            for p in $pods; do [ "$(readyz "$p")" = drained ] && is_drained=1; done
            if [ "$is_drained" = 1 ]; then drained="$drained ${d#host-}"; else waiting="$waiting $d"; fi
        done
        [ -z "$waiting" ] && break
        if [ "$SECONDS" -ge "$deadline" ]; then
            echo "world-start: after $timeout s: $ready of $count hosts Ready; not Ready:$waiting" >&2
            exit 1
        fi
        sleep 2
    done
    for h in $drained; do
        echo "world-start: host $h is DRAINED (it left the world; /readyz says drained) and stays unready: remove \"$h\" from var.hosts and apply, or bring it back as a NEW host: kubectl --context {{world_ctx}} -n {{world_ns}} rollout restart deployment/host-$h"
    done
    echo "world-start: the world is up ($ready of $count hosts Ready${drained:+; drained:$drained})"

# Print (never run) the command that registers this world with the login service of `world_ctx`'s
# cluster: `just orion-login world add …` on orion, `kubectl --context <ctx> -n login exec deploy/login --
# login world add …` on any other context (the twin's own login). `world add` REPLACES the world's row:
# run it with every option, or the omitted ones are lost.
# The world id is the namespace without its `world-` prefix; `name=` sets the displayed name.
# The login service links to the coordinator (coordinator.<ns>.svc.cluster.local:6490), pinned by the
# SHA-256 of the public certificate in Secret login-link-identity. No --quic / --wt: each spawn answer
# names the endpoint of the host that places the player. A name with a single quote is refused.
world-register $name="Errantry":
    #!/usr/bin/env bash
    set -euo pipefail
    k() { kubectl --context {{world_ctx}} -n {{world_ns}} "$@"; }
    ns="{{world_ns}}"
    id="${ns#world-}"
    case "$name" in *\'*) echo "world-register: the name must not contain a single quote" >&2; exit 2 ;; esac
    pin=$(k get secret login-link-identity -o jsonpath='{.data.link\.cert-sha256\.cert}' | base64 -d | sha256sum | cut -d' ' -f1)
    case "$pin" in
        *[!0-9a-f]*|'') echo "world-register: no usable link certificate in Secret login-link-identity" >&2; exit 1 ;;
    esac
    # sha256sum of nothing: the Secret or its key is missing.
    [ "$pin" != e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 ] ||
        { echo "world-register: Secret login-link-identity has no link.cert-sha256.cert" >&2; exit 1; }
    [ "${#pin}" -eq 64 ] || { echo "world-register: link pin is not 64 hex digits" >&2; exit 1; }
    echo "Run this (it replaces the row of world '$id'; nothing was changed by this recipe):"
    echo
    args="world add $id --name '$name' --link coordinator.$ns.svc.cluster.local:6490 --link-cert $pin"
    if [ "{{world_ctx}}" = "{{orion_context}}" ]; then
        echo "just orion-login $args"
    else
        echo "kubectl --context {{world_ctx}} -n login exec deploy/login -- login $args"
    fi

# Back to the release before the last `orion-world-update` (scripts/world_update.sh --rollback): the server
# and web refs that update recorded in the world's ConfigMap release-history, through the same stop →
# set images → start path, all four workloads to the same build. Refused (nothing changed) when no
# previous release is recorded or a recorded ref is not a digest. A rollback records what it replaced:
# a second one goes forward again. A database migration made by the newer build is NOT undone.
orion-world-rollback namespace="world-errantry":
    @echo "orion-world-rollback: a schema migration made by the newer build is NOT undone by this; if the older build cannot read the database, restore it: just orion-pg-restore <world> <object>"
    scripts/world_update.sh {{orion_context}} {{namespace}} --rollback

# Run the nightly database dump now (a Job from the CronJob pg-dump in the postgres namespace).
# This writes to the cluster. Watch it: `kubectl --context orion -n postgres logs -f job/<the printed name>`.
orion-pg-backup-now:
    #!/usr/bin/env bash
    set -euo pipefail
    job="pg-dump-manual-$(date -u +%Y%m%d%H%M%S)"
    kubectl --context {{orion_context}} -n postgres create job --from=cronjob/pg-dump "$job"
    echo "orion-pg-backup-now: started $job"

# PRINTS (runs nothing: a restore overwrites) the two sequences that restore one nightly dump: A, the
# drill into a SCRATCH database beside the live one, checked by its table owners; B, the live restore
# (world stopped, the live database renamed aside, recreated owned by the world's role, restored, owners
# checked, world started). world is the world id (e.g. errantry), object the dump's key under it
# (e.g. 20261115T031500Z.dump; `aws s3 ls` lists them). Restored objects are owned by role world_<id>
# (`--no-owner --role`): the world's own role can write and migrate them.
orion-pg-restore world object:
    #!/usr/bin/env bash
    set -euo pipefail
    db="world_{{world}}"
    k="kubectl --context {{orion_context}} -n postgres"
    cat <<EOF
    # Both sequences: fetch the dump (S3 endpoint and bucket: the pg-dump CronJob's S3_ENDPOINT / BUCKET;
    # Garage needs the two checksum settings), and find the primary pod of the Cluster pg.
    AWS_REQUEST_CHECKSUM_CALCULATION=when_required AWS_RESPONSE_CHECKSUM_VALIDATION=when_required \\
      aws s3 cp --endpoint-url <S3_ENDPOINT> s3://<BUCKET>/{{world}}/{{object}} ./{{world}}-{{object}}
    $k get pods -l cnpg.io/cluster=pg,cnpg.io/instanceRole=primary -o name     # = <primary-pod> below

    # ===== A. DRILL: restore into a scratch database (the world keeps running) =====
    $k exec <primary-pod> -c postgres -- createdb -O $db ${db}_restore
    $k exec -i <primary-pod> -c postgres -- pg_restore --no-owner --role=$db -d ${db}_restore < ./{{world}}-{{object}}
    # check: every table is owned by $db (one line: "$db|<count>"), and the world's tables are there
    $k exec <primary-pod> -c postgres -- psql -d ${db}_restore -Atc "select tableowner, count(*) from pg_tables where schemaname = 'public' group by 1"
    $k exec <primary-pod> -c postgres -- psql -d ${db}_restore -Atc "select version from _sqlx_migrations order by version desc limit 1"
    # done looking: drop it
    $k exec <primary-pod> -c postgres -- dropdb ${db}_restore

    # ===== B. LIVE RESTORE: the world returns to the moment of the dump =====
    # 1. stop the world (hosts leave through their leave cuts, then the coordinator)
    just world_ctx={{orion_context}} world_ns=world-{{world}} world-stop
    # 2. move the live database aside (kept for a look; "being accessed by other users": find them with
    #    select pid, application_name from pg_stat_activity where datname = '$db')
    $k exec <primary-pod> -c postgres -- psql -c "alter database $db rename to ${db}_before_restore"
    # 3. a new empty database owned by the world's role (if it says it exists, the operator recreated it
    #    already: check it is empty with step 5's first query, and go on)
    $k exec <primary-pod> -c postgres -- createdb -O $db $db
    # 4. restore into it, every object owned by the world's role
    $k exec -i <primary-pod> -c postgres -- pg_restore --no-owner --role=$db -d $db < ./{{world}}-{{object}}
    # 5. check: every table is owned by $db (one line: "$db|<count>")
    $k exec <primary-pod> -c postgres -- psql -d $db -Atc "select tableowner, count(*) from pg_tables where schemaname = 'public' group by 1"
    # 6. start the world. A dump taken while the world ran (every nightly one) holds its hosts' rows as
    #    running: this first start raises the world generation once (WorldRollback fires) — expected.
    just world_ctx={{orion_context}} world_ns=world-{{world}} world-start
    # 7. later, once the world plays: drop the old one
    $k exec <primary-pod> -c postgres -- dropdb ${db}_before_restore
    EOF

# Put the login root's service token on the box (/srv/mmo/secrets/service-token, mode 600) for the
# game host. The token travels through the pipe only.
orion-login-token-push:
    #!/usr/bin/env bash
    set -euo pipefail
    tok=$({{tofu}} -chdir=deploy/envs/orion/login output -raw service_token)
    if [ -z "$tok" ]; then
        echo "orion-login-token-push: the login root has no service token (apply it first)" >&2
        exit 1
    fi
    n=$(printf '%s' "$tok" | wc -c | tr -d ' ')
    # printf is a builtin: the token is never in a process argument list. The box installs the
    # file only if it arrived whole (its size equals the expected byte count).
    if printf '%s' "$tok" | ssh -o BatchMode=yes {{deploy_host}} "umask 077 && mkdir -p /srv/mmo/secrets && cat > /srv/mmo/secrets/service-token.new && [ \"\$(wc -c < /srv/mmo/secrets/service-token.new)\" -eq $n ] && mv -f /srv/mmo/secrets/service-token.new /srv/mmo/secrets/service-token || { rm -f /srv/mmo/secrets/service-token.new; exit 1; }"; then
        echo "orion-login-token-push: token written to {{deploy_host}}:/srv/mmo/secrets/service-token"
    else
        echo "orion-login-token-push: failed — the token on the box is unchanged" >&2
        exit 1
    fi
    unset tok

# Whole quick suites (slow tests skipped). Minutes, not seconds — reach for
# `smoke` / `test-one` while iterating and keep this for a broader sweep.
test: test-game test-client test-server test-worldgen

# Rollback/undo invariants and seeded randomized rollback (quick subset).
test-game:
    {{cap}} {{cargo}} test -p game

# Client headless suite: sim-bridge, conversion, interpolation, meshing (quick subset).
test-client:
    {{cap}} {{cargo}} test -p client

# Server suite compile-check: every server test is currently tagged slow
# (multi-second region-thread rigs), so the quick run builds them and runs 0.
test-server:
    {{cap}} {{cargo}} test -p server --features test-hooks

# Worldgen suite: pure-function generation + the dev disk cache.
test-worldgen:
    {{cap}} {{cargo}} test -p worldgen

# Everything, slow correctness tests included — run before calling work done.
# Still skips wall-clock perf gates, evidence-only tests, and the explicit
# region-load benchmark; those retain their dedicated recipes.
test-full:
    {{cap}} {{cargo}} test -p game -- --include-ignored --test-threads={{test_threads}} --skip perf_evidence_journal_scales_with_activity --skip perf_evidence_hash_modes
    {{cap}} {{cargo}} test -p client -- --include-ignored --test-threads={{test_threads}} --skip region_tick_cost_fits_keepup_budget --skip soak_realtime_keepup
    {{cap}} {{cargo}} test -p server --features test-hooks -- --include-ignored --test-threads={{test_threads}}
    {{cap}} {{cargo}} test -p worldgen -- --include-ignored --test-threads={{test_threads}} --skip region_load_cost

# Keep-up budget benchmark (release): measures the region-tick cost and
# asserts the death-spiral inequality (worst frame < tick period). Fails with
# a number when a game change makes the sim too slow for the client to keep up.
keepup-budget:
    {{cap}} {{cargo}} test -p client --release --test keepup_budget -- --ignored --nocapture

# Wall-clock keep-up soak (release, ~40s + build): real tick cadence + 60/s
# fabricated input; fails if the prediction log trends up (death-spiral
# signature). Debug builds measure but don't assert. Full output (incl. the
# reconcile tracing) goes to target/soak.log; the [client-budget] / [soak]
# summary lines are reprinted at the end so the attribution is readable.
soak:
    #!/usr/bin/env bash
    set -uo pipefail
    log=target/soak.log
    {{cap}} {{cargo}} test -p client --release --test reconcile_keepup soak_realtime_keepup -- --ignored --nocapture --test-threads=1 2>&1 | tee "$log"
    rc=${PIPESTATUS[0]}
    echo
    echo "=== soak summary (full log: $log) ==="
    cat <<'LEGEND'
    [client-budget] = game-loop wall-time attribution, one line per 5s window:
      window   actual window length (s)
      util     busy share of the window (window - idle); this harness never
               blocks the loop, so it always reads 100% here
      tick     progress_world_one_tick calls (client ticks): count n, summed
               wall time, mean, slowest single call
      recon    reconcile passes for server events; replayed = predicted ticks
               rolled back + re-simulated (separates replay cost from match cost)
      snap     region snapshot loads (teardown + Rollback clone + world insert)
      idle     time blocked waiting for events (live client select! only; 0 here)
      other    window - idle - (tick+recon+snap): loop overhead, and in this
               harness the time spent outside the manager (server sim, sleeps)
      sim      per-region Tick executions incl. reconcile replays: count n,
               ctrl = controller pass (physics; the hot part), input = input
               step, scan = boundary scan; sim n > tick n x regions => replaying
      worst    region with the highest avg per-execution cost
    [soak]     1s home-region prediction-log depth samples; a rising series is
               the death-spiral signature the test asserts against
    LEGEND
    grep -E "client-budget|\[soak\]" "$log" || true
    exit $rc

# ---- Lint / format --------------------------------------------------------

# Format all crates.
fmt:
    {{cargo}} fmt --all

# Check formatting without writing changes.
fmt-check:
    {{cargo}} fmt --all --check

# Clippy across the workspace (warnings as errors).
clippy:
    {{cap}} {{cargo}} clippy --workspace --bins -- -D warnings

# ---- Profiling ------------------------------------------------------------

# The recipes below produce PLAIN TEXT (unlike tracy/hotspot, which need a GUI), so
# their output can be pasted into a bug report or an LLM session. Use them in this order:
#
#   0. `just perf-capture` — everything below's log-based half in one self-describing
#                            directory (manifest, join timeline, RSS, budgets, diag verdict).
#   1. `just diag`         — is the frame CPU-bound or GPU-bound? Per-pass GPU ms vs frame_time.
#   2. `just gpu-log`      — second opinion from outside the process (GPU busy %), no rebuild.
#   3. `just trace-chrome` — if CPU-bound: which ECS system eats the frame.
#
# All of them run RELEASE builds. The dev profile is opt-level 1 (see the root Cargo.toml
# note: ~2.5x off release on the hot loops), so an FPS number from `just client` is not a
# measurement of anything.

# DIAG=1 adds RenderDiagnosticsPlugin (wgpu timestamp + pipeline-statistics queries):
# every 2s it prints fps / frame_time / entity_count and, per render pass,
# render/<pass>/elapsed_gpu, /elapsed_cpu and /fragment_shader_invocations.
# Read it as: sum of elapsed_gpu ~= frame_time => GPU-bound (and fragment_shader_invocations
# says whether that's overdraw); sum far below frame_time => CPU-bound, go to `just trace-chrome`.
# GPU columns need Vulkan or DX12 (this is the native path; wasm gets CPU time only).
# Override the dump period with DIAG_INTERVAL=<secs>.
#
# Frame + per-render-pass GPU/CPU timings as text -> target/diag/client.log.
diag:
    #!/usr/bin/env bash
    set -uo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_SKIP_MENU=1
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    mkdir -p target/diag
    log=target/diag/client.log
    {{cap}} {{cargo}} build --release --bin client --bin server
    RUST_BACKTRACE=1 ./target/release/server > target/diag/server.log 2>&1 &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    sleep 1
    echo ">> DIAG client -> $log  (play for ~30s, then close the window)"
    DIAG=1 RUST_BACKTRACE=1 ./target/release/client > "$log" 2>&1
    echo
    scripts/diag_summary.py "$log"
    echo
    echo "(full log: $log)"

# THE capture to hand to an analysis session. Release server + client, one client, with
# everything in one self-describing directory:
#   target/perf-capture/<utc-timestamp>-<adapter>-<world>/
#     summary.txt    READ FIRST: provenance, join timeline, resources, budgets, verdict, errors
#     manifest.json  git rev + dirty flag, profile, adapter asked for AND selected, world
#                    scale, backend/thread env knobs, host, duration, exit codes
#     client.log / server.log   full logs: `[perf] event=… t_ms=…` join timeline on both
#                    sides, `[client-budget]` (DEBUG, enabled here via PERF_LOG=1),
#                    `[server]` counters, `[region …]` tick lines, DIAG per-pass dumps
#     rss.csv        1 Hz VmRSS / VmHWM / threads / CPU ticks for both processes
#     diag.txt       scripts/diag_summary.py CPU-vs-GPU verdict
# Let the world load, play ~30 s, close the window. Or run it hands-off for a fixed time:
#   PERF_CAPTURE_SECONDS=90 just adapter=Intel world=tiny perf-capture
# PERF_CAPTURE_DIAG=0 drops the wgpu timestamp queries. Compare captures only within one
# adapter + world scale. Re-summarise an old capture: scripts/perf_capture_report.py <dir>.
#
# One-directory release perf capture (logs + manifest + RSS + verdict) -> target/perf-capture/.
perf-capture:
    VOXEL_REGION_BLOCKS={{play_blocks}} {{cap}} {{cargo}} build --release --bin client --bin server
    VOXEL_SKIP_MENU=1 VOXEL_REGION_BLOCKS={{play_blocks}} scripts/perf_capture.sh "{{adapter}}" "{{world}}"

# GPU time per render pass straight from Mesa's Intel Vulkan driver (INTEL_MEASURE), including
# the passes `diag` cannot time (Bevy's shadow cascades record no GPU timestamp, nor do the
# Slint composite or post passes) — so `diag` can call a frame "CPU-bound" while the GPU is
# in fact busy. Release server + client on the Intel adapter (`adapter=` overrides it, but
# only an Intel GPU writes the CSV), closed cleanly after `seconds` (VOXEL_EXIT_AFTER_SECS),
# then scripts/gpu_measure_summary.py: mean GPU µs per frame and per render pass over the
# last 150 frames (GPU_MEASURE_FRAMES). Output in target/gpu-measure/<label>/.
#   just gpu-measure                 # label "latest", 45 s
#   just gpu-measure shadows-off 60
#
# Steady-state frame time of the windowed release client (never fullscreen): mean / median /
# p90 of the DIAG frame_time dumps after the join (scripts/fps_steady.sh). Intel by default
# (`adapter=` overrides); extra KEY=VALUE args go to the client for A/B runs. A window is held
# to ~60 fps by the desktop compositor — for headroom raise the load with region_blocks=512.
#   just fps-steady base
#   just region_blocks=512 fps-steady c1 VOXEL_SHADOW_CASCADES=1
#
# Windowed steady-state frame time (mean/median/p90) for A/B perf runs -> target/fps-steady/.
fps-steady label *env:
    VOXEL_REGION_BLOCKS={{play_blocks}} {{cap}} {{cargo}} build --release --bin client --bin server
    WGPU_ADAPTER_NAME="{{ if adapter == "" { "Intel" } else { adapter } }}" {{ if world == "" { "" } else { "VOXEL_WORLD=" + world } }} VOXEL_REGION_BLOCKS={{play_blocks}} scripts/fps_steady.sh {{label}} {{env}}

# Per-render-pass GPU time on an Intel adapter, shadow cascades included -> target/gpu-measure/.
gpu-measure label="latest" seconds="45":
    VOXEL_REGION_BLOCKS={{play_blocks}} {{cap}} {{cargo}} build --release --bin client --bin server
    WGPU_ADAPTER_NAME="{{ if adapter == "" { "Intel" } else { adapter } }}" {{ if world == "" { "" } else { "VOXEL_WORLD=" + world } }} VOXEL_REGION_BLOCKS={{play_blocks}} scripts/gpu_measure.sh target/gpu-measure/{{label}} {{seconds}}

# The independent check on `just diag`'s GPU numbers, measured from outside the process,
# and it needs no rebuild. Sustained Graphics-Pipe ~100% => GPU-bound; low while a core is
# pegged => CPU-bound. Start the client first, then run this alongside it. Needs amdgpu_top
# (cargo install amdgpu_top) and an AMD GPU; override the target with `just pci=... gpu-log`.
#
# Sample GPU busy % / clocks / VRAM as JSON -> target/diag/gpu.json.
gpu-log:
    #!/usr/bin/env bash
    set -uo pipefail
    mkdir -p target/diag
    out=target/diag/gpu.json
    echo ">> sampling {{pci}} at 2Hz for 30s -> $out"
    amdgpu_top --pci {{pci}} -J --single -s 500 -n 60 > "$out"
    echo "wrote $out — GRBM 'Graphics Pipe' busy % is the GPU-bound tell"

# Runs the client with bevy's trace_chrome feature (a tracing span per ECS system, per
# extract step and per render-graph node), writing a Chrome Trace Event JSON, then reduces
# it to a top-N table via scripts/trace_summary.py. Use after `just diag` says CPU-bound.
# CLOSE THE WINDOW to stop — the JSON is only flushed on a clean exit, so Ctrl-C truncates it.
# The feature build lands in target/variants/trace_chrome/release/client (its own Bevy
# feature set, see `variant_dir`); it is built under `cap` and launched directly, never via `cargo run`.
#
# Per-ECS-system frame breakdown as text (which system eats the frame).
trace-chrome:
    #!/usr/bin/env bash
    set -uo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_SKIP_MENU=1
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    mkdir -p target/trace
    json=target/trace/client.json
    {{cap}} {{cargo}} build --release --bin server
    {{cap}} {{cargo}} build --release --bin client --features trace_chrome --target-dir {{variant_dir}}/trace_chrome
    RUST_BACKTRACE=1 ./target/release/server > target/trace/server.log 2>&1 &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    sleep 1
    echo ">> tracing -> $json  (play for ~30s, then CLOSE THE WINDOW — Ctrl-C truncates it)"
    TRACE_CHROME="$json" RUST_BACKTRACE=1 ./{{variant_dir}}/trace_chrome/release/client
    echo
    # Skip the first 5s: startup asset loads + pipeline compilation would otherwise
    # dominate the totals and hide the steady-state per-frame cost.
    scripts/trace_summary.py "$json" --skip-ms 5000 --top 40 --skip-cat rapier3d --skip-cat parry3d

# Length of the join window `trace-join` summarises, in ms: `just join_ms=90000 trace-join`.
join_ms := "30000"

# The opposite cut of `trace-chrome`: the same capture, but summarising ONLY the first
# `join_ms` of it — the load/join phase that the steady-state table deliberately hides.
# Read the `max ms` column: one multi-second call of a bridge system (drain_client_updates,
# mark_remesh_dirty, apply_lod_events) is a main-thread stall. Close the window once the
# world has loaded (or has visibly failed to); the trace grows ~1 MB/s, so don't linger.
# Re-reduce an existing capture without re-running:
#   scripts/trace_summary.py target/trace/client.json --until-ms 60000
#
# Per-ECS-system breakdown of the join phase only (first `join_ms` of the capture).
trace-join:
    #!/usr/bin/env bash
    set -uo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_SKIP_MENU=1
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    mkdir -p target/trace
    json=target/trace/client.json
    {{cap}} {{cargo}} build --release --bin server
    {{cap}} {{cargo}} build --release --bin client --features trace_chrome --target-dir {{variant_dir}}/trace_chrome
    RUST_BACKTRACE=1 ./target/release/server > target/trace/server.log 2>&1 &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    sleep 1
    echo ">> tracing the join -> $json  (wait for the world to load, then CLOSE THE WINDOW)"
    TRACE_CHROME="$json" RUST_BACKTRACE=1 ./{{variant_dir}}/trace_chrome/release/client
    echo
    # The rapier fork traces its solver internals: most of the file, none of the frame.
    scripts/trace_summary.py "$json" --until-ms {{join_ms}} --top 40 --skip-cat rapier3d --skip-cat parry3d

# Start Pyroscope (:4040) + Grafana (:3000). Build with `--features pyroscope` to push profiles.
profile-up:
    docker compose up -d

# Stop the profiling stack.
profile-down:
    docker compose down

# Client instrumented with pyroscope, server plain alongside; pushes to :4040 (`just profile-up` first).
profile-client:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_SKIP_MENU=1
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    RUST_BACKTRACE=1 {{cargo}} run --bin server &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    PYROSCOPE=1 RUST_BACKTRACE=1 {{cargo}} run --bin client --features pyroscope --target-dir {{variant_dir}}/pyroscope

# Server instrumented with pyroscope, client plain alongside; pushes to :4040 (`just profile-up` first).
profile-server:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_SKIP_MENU=1
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    RUST_BACKTRACE=1 {{cargo}} run --bin client &
    client_pid=$!
    trap 'kill "$client_pid" 2>/dev/null || true' EXIT
    PYROSCOPE=1 RUST_BACKTRACE=1 {{cargo}} run --bin server --features pyroscope --target-dir {{variant_dir}}/pyroscope

# Run the client with the Tracy frame profiler compiled in (bevy `trace_tracy`,
# opt-in via the client's `tracy` feature — plain builds never compile the
# agent), with a plain server alongside (the client hard-panics if nothing
# answers on :6466 within the QUIC handshake timeout). Client uses the
# `profiling` cargo profile (release + debug info) so hot loops aren't
# distorted. Connect a protocol-matching Tracy GUI while it runs
# (tracy-client-sys 0.28 bundles Tracy v0.13.1). Native only — the `tracy`
# feature must not be enabled on wasm builds. Pass `detailed` (i.e.
# `just tracy-client detailed`) to also enable bevy's `detailed_trace` —
# extra-verbose ECS/render-internal spans; much noisier and slower.
tracy-client detailed="":
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_SKIP_MENU=1
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    feat="tracy"
    if [ -n "{{detailed}}" ]; then feat="tracy-detailed"; fi
    {{cap}} {{cargo}} build --bin server
    RUST_BACKTRACE=1 {{cargo}} run --bin server &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    RUST_BACKTRACE=1 {{cargo}} run --bin client --profile profiling --features "$feat" --target-dir "{{variant_dir}}/$feat"

# Record a perf profile of the client and open hotspot (see scripts/perf.sh).
# Uses the `profiling` cargo profile (release + debug info): dev's opt-level 1
# distorts hot loops ~2.5x, so debug captures don't represent production.
perf:
    VOXEL_REGION_BLOCKS={{play_blocks}} {{cap}} {{cargo}} build --bin client --profile profiling
    sudo env VOXEL_SKIP_MENU=1 perf script record -F9997 --call-graph dwarf,16384 -e cpu-clock ./target/profiling/client
    sudo chmod +r perf.data
    hotspot

# Both perf-* recipes need inferno on PATH: `cargo install inferno`. They record
# folded stacks (perf-folded-{client,server}.txt), running the other side (like
# `just run`) to make the load representative. The folded files run to tens of MB —
# never paste or open them whole; `just perf-summary` reduces one to top-N tables.
# NOTE: `perf script --no-inline` is required — perf's inline-frame expansion drives
# an addr2line coprocess that livelocks on these Rust binaries ("could not read first
# record"), regardless of Cranelift vs stable. --no-inline skips it; function-level
# symbols still resolve from the ELF symtab, which is all folded stacks need.

# Profile the client while the server runs; close the client window to stop.
# Both sides build with the `profiling` profile (release + debug info): dev's
# opt-level 1 distorts hot loops ~2.5x, so debug captures don't represent
# production costs.
perf-client:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_SKIP_MENU=1
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    {{cap}} {{cargo}} build --bin client --bin server --profile profiling
    RUST_BACKTRACE=1 ./target/profiling/server &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    sudo -E perf record {{perf_flags}} ./target/profiling/client
    sudo chmod +r perf.data
    perf script --no-inline | inferno-collapse-perf > perf-folded-client.txt
    echo "wrote perf-folded-client.txt — too big to read whole; the reduced tables follow"
    scripts/perf_summary.py perf-folded-client.txt

# Profile the server while the client runs; Ctrl-C the server to stop recording.
# Same `profiling` profile rationale as perf-client.
perf-server:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    {{cap}} {{cargo}} build --bin client --bin server --profile profiling
    export VOXEL_SKIP_MENU=1
    RUST_BACKTRACE=1 ./target/profiling/client &
    client_pid=$!
    trap 'kill "$client_pid" 2>/dev/null || true' EXIT
    sudo -E perf record {{perf_flags}} ./target/profiling/server
    sudo chmod +r perf.data
    perf script --no-inline | inferno-collapse-perf > perf-folded-server.txt
    echo "wrote perf-folded-server.txt — too big to read whole; the reduced tables follow"
    scripts/perf_summary.py perf-folded-server.txt

# Extra args filter the tables, e.g.
#   just perf-summary client --thread game --prefix rapier3d:: --prefix parry3d::
#
# Reduce perf-folded-<side>.txt to thread / crate / self / inclusive top-N tables.
perf-summary side="client" *args:
    scripts/perf_summary.py perf-folded-{{side}}.txt {{args}}

# Reduce a Firefox Profiler capture of the web build (about:profiling or devtools →
# Performance, then Upload → Download: a processed `*.json.gz`) to text: frames, where the
# main thread went (wasm per crate / Bevy system / blocked in sync WebGL IPC / JS / GC), the
# WebGL calls, the WebGL executor thread (CanvasRenderer) and other browser threads, markers,
# memory, the client's [perf] log lines and a verdict with known Firefox costs explained.
# Extra args: --from-ms N / --to-ms N (window), --top N, --thread NAME, --logs, e.g.
#   just firefox-profile ~/Downloads/"Firefox 2026-11-02 17.15 profile.json.gz" --from-ms 5000
#
# Summarise a Firefox Profiler capture of the web build (client + WebGL).
firefox-profile profile *args:
    python3 -I scripts/firefox_profile_summary.py "{{profile}}" {{args}}

# ---- Housekeeping ---------------------------------------------------------

# Remove build artifacts.
clean:
    {{cargo}} clean

# The quest script language: its OWN Cargo workspace in quest/ (own Cargo.lock and quest/target/), so its
# builds never touch the game's. Plan: docs/superpowers/plans/2026-11-16-quest-script-language-mvp.md.
# quest-check: type-check, clippy, and the wasm32 build of the crates that must run anywhere.
quest-check:
    {{cap}} {{cargo}} clippy --manifest-path quest/Cargo.toml --workspace --all-targets -- -D warnings
    {{cap}} {{cargo}} check --manifest-path quest/Cargo.toml --workspace --exclude quest-cli --target wasm32-unknown-unknown

# quest-test "-p quest-vm --test mem" — the language's tests (all of them when no argument is given).
quest-test args="":
    {{cap}} {{cargo}} test --manifest-path quest/Cargo.toml {{ if args == "" { "--workspace" } else { args } }}

# quest-run <dir> [flags] — compile and run a quest script directory (see quest/README.md for the flags).
quest-run dir *args:
    {{cap}} {{cargo}} run -q --manifest-path quest/Cargo.toml -p quest-cli -- run {{dir}} {{args}}

# The quest corpus goldens (bytecode hash, per-tick state hash and fuel, final memory, output) recorded on
# native must be reproduced on wasm32 (node, wasm-bindgen-test-runner — from quest/'s own Cargo.lock).
quest-wasm:
    #!/usr/bin/env bash
    set -euo pipefail
    ver=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' quest/Cargo.lock)
    root="$PWD/quest/target/tools/wasm-bindgen-$ver"
    [ -x "$root/bin/wasm-bindgen-test-runner" ] || {{cap}} cargo install --locked wasm-bindgen-cli --version "$ver" --root "$root"
    {{cap}} {{cargo}} test --manifest-path quest/Cargo.toml -p quest-corpus --test determinism
    CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$root/bin/wasm-bindgen-test-runner" \
        {{cap}} {{cargo}} test --manifest-path quest/Cargo.toml -p quest-corpus --test determinism --target wasm32-unknown-unknown
