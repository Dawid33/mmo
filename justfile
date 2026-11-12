# justfile — task runner for the MMO workspace.
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
# e.g. `just world=tiny run`, `just world=small fps-steady base`. Finer control:
# export VOXEL_NEAR_RADIUS=0..3 / VOXEL_FORTRESS_RADIUS=0..3 / VOXEL_LOD_RINGS=off|small|full|"2:2,4:4"
# (see crates/client/src/world_scale.rs). Every recipe inherits it; tests
# ignore it (GameInstanceManager::new is env-free).
world := ""
export VOXEL_WORLD := world

# How far you see, in metres (empty = default: the LOD ring table's reach —
# 15,360 m at the play recipes' 256 m regions (~4,400 LOD keys), 1 km at 64/128).
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
# `procedural` = the noise world (nothing is saved). Any other value = that world file (e.g. one
# from `just forge-proto`, one finite world — by default a 16 km continent):
#   just forge-proto --seed D5C0DE021A --out target/world-forge/best
#   just world_file=target/world-forge/best.world server   # then `just client`
# Defaults to $VOXEL_WORLD_FILE. Relative paths resolve from the repo root.
# Tests never go through the server: procedural. The browser build only joins a server.
world_file := env_var_or_default("VOXEL_WORLD_FILE", "")
export VOXEL_WORLD_FILE := world_file

# The server serves stud terrain (toy bricks) only: generated from the forged world,
# or from the noise world for `world_file=procedural`, and saved in the world store.
# There is no terrain switch. A stored world last served as a voxel world (one played
# before the stud cutover) is refused at start with a message — terrain is never
# converted: re-forge (`just run --reset` for the debug world) or make a new world.
# Perf numbers recorded before the cutover are voxel numbers: do not compare across it.

# Region edge in BLOCKS (1 block = 1 m) — a BUILD-time constant
# (`game::REGION_BLOCKS`), unlike `world`: 64 = 1024-unit / 64 m regions
# (default), 128 = 2048-unit, 256 = 4096-unit, 512 = 8192-unit regions, each
# with the ring table re-cut to the same pixel budget and horizon. Nothing else
# is legal (see crates/game/src/protocol.rs). The old chunk-denominated
# VOXEL_REGION_CHUNKS / VOXEL_CELL_CHUNKS and the removed cell mode's
# VOXEL_CELL_BLOCKS fail the build. The sim chunk (`game::CHUNK_BLOCKS`, 16 m:
# one entity, one rapier body, one collider set) is a constant, not a dial.
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
# tests, check, clippy, raw cargo — stays at `test_blocks` = 64.
# An explicit `just region_blocks=N <recipe>` (or $VOXEL_REGION_BLOCKS) wins for
# every recipe.
region_blocks := env_var_or_default("VOXEL_REGION_BLOCKS", "")
test_blocks := if region_blocks == "" { "64" } else { region_blocks }
play_blocks := if region_blocks == "" { "256" } else { region_blocks }
export VOXEL_REGION_BLOCKS := test_blocks

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

# Run the Bevy client. Override `adapter` to pin a GPU, e.g. `just adapter=6700 client`.
client:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_SKIP_MENU=1
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    if [ -n "{{user}}" ]; then export VOXEL_PLAYER_NAME="{{user}}"; fi
    RUST_BACKTRACE=1 {{cargo}} run --bin client

# Debug launch: straight into a small generated world with a fortress embarked
# (`client debug embarked`, crates/client/src/debug_launch.rs), on stud terrain.
# The world is worlds/debug-embarked/ (16 km / 32 m, forged on
# first run) and is REUSED, with its fortress, on every later run; `just run --reset`
# (or deleting the directory) re-forges it and embarks afresh. A debug world last
# played as a voxel world is refused by its server (the starting screen shows the
# server log's reason): `just run --reset`. Builds both binaries — the client
# spawns the world's server itself, like a menu start.
run *args:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    if [ -n "{{user}}" ]; then export VOXEL_PLAYER_NAME="{{user}}"; fi
    {{cap}} {{cargo}} build --bin client --bin server
    RUST_BACKTRACE=1 ./target/debug/client debug embarked {{args}}

# Debug launch into the toy-brick TEST world: 3x3 regions of hand-made stud chunks, no
# store, no map, no history, no menu — a debug preset only; the real game (`run`,
# `play`, `server`) is on stud terrain by default.
# `just run-studs --procedural` plays the procedural noise world on stud terrain without
# a store (the same terrain as `just world_file=procedural server` + `just client`).
# Far terrain: the server streams a coarse height field per LOD key (`game::stud_lod`)
# and the client draws it beyond the loaded regions. Its dials are the ring dials:
# `world=tiny|small|full`, `view=<m>` (e.g. `just view=4000 run` for a cheap short view)
# and VOXEL_LOD_RINGS=off|small|full|"bound:factor,..." — on stud terrain the client
# turns whichever table that gives into the stud one (`game::stud_ring_table`: the same
# reach, cells 4x coarser, never finer than a stud or wider than 256 cells a tile).
# LOD_BUILD_THREADS sizes the server's builder pool.
# The toy world has nothing beyond its 3x3 regions, so it runs with VOXEL_LOD_RINGS=off
# (set here: the world scale is read in `main`, before the launch plugin runs).
run-studs *args:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    case " {{args}} " in
        *" --procedural "*) ;;
        *) export VOXEL_LOD_RINGS=off ;;
    esac
    export VOXEL_NEAR_RADIUS=1
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    {{cap}} {{cargo}} build --bin client --bin server
    RUST_BACKTRACE=1 ./target/debug/client debug studs {{args}}

# The prefab editor, straight in: an empty baseplate world with prefab NAME of your
# library opened (a new one when there is none; `untitled` without a name).
# `just prefab-editor --game dev-arch` edits the game's own assets/prefabs/dev-arch.prefab.ron
# (dev builds). Build mode is on: left click places, right click removes, Ctrl+S saves.
# The world has nothing beyond its 3x3 regions, so it runs like `run-studs`.
prefab-editor *args:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    export VOXEL_LOD_RINGS=off
    export VOXEL_NEAR_RADIUS=1
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    {{cap}} {{cargo}} build --bin client --bin server
    RUST_BACKTRACE=1 ./target/debug/client debug prefab {{args}}

run_two_port := "6476"

# `run` with TWO clients on one server, both in ONE fortress: client 1 is the
# debug launch in fortress mode, hosting the embarked world's server on UDP
# `run_two_port` (`--port`); once it has embarked (it logs
# `[debug] embarked: fortress site=N`), client 2 joins (`--join`) and starts an
# adventure at that fortress (`--adventure-at N`). Output is prefixed
# [client 1] / [client 2] (client 1's also in target/run-two/client1.log); the
# server log is worlds/debug-embarked/server.log. Closing client 1 ends both
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
    RUST_BACKTRACE=1 ./target/debug/client debug embarked --port "$port" {{args}} \
        > >(tee "$log" | sed -u 's/^/[client 1] /') 2>&1 &
    host=$!; pids+=("$host")
    # The world may be forged first, then client 1 embarks: wait for its fortress.
    site=""
    until [ -n "$site" ]; do
        kill -0 "$host" 2>/dev/null || exit 0
        sleep 0.5
        site=$(grep -aoE 'embarked: fortress site=[0-9]+' "$log" | head -n1 | grep -oE '[0-9]+$' || true)
    done
    echo "[run-two] client 1 holds fortress site $site; client 2 adventures there"
    # Its own player name: with client 1's, the server would see client 1's player.
    VOXEL_PLAYER_NAME="${VOXEL_PLAYER_NAME:-${USER:-player}}-2" RUST_BACKTRACE=1 \
        ./target/debug/client debug embarked --join "127.0.0.1:$port" --adventure-at "$site" \
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

# `run` in release mode: the embarked debug world (`just run-release --reset`
# starts it over).
run-release *args:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    if [ -n "{{user}}" ]; then export VOXEL_PLAYER_NAME="{{user}}"; fi
    {{cap}} {{cargo}} build --release --bin client --bin server
    RUST_BACKTRACE=1 ./target/release/client debug embarked {{args}}

# Scripted flight across region seams (release): server + client, the client
# flying a straight line under VOXEL_AUTOPILOT once the world is ready, closing
# itself after `seconds` of cruise (a seam every ~3 s at fly-sprint). Logs go to
# target/fly-probe/; prints one pair of lines per crossing — game thread
# (`region_crossing`: tick gaps, stalled/aligned home ticks, corrections,
# snapshot loads) and render (`region_crossing_render`: worst frame, longest
# sim-pose hold) — plus any main-thread stalls. e.g. `just adapter=6700 fly-probe 60`.
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
    grep -hE "event=(world_ready|autopilot_start|region_crossing|region_crossing_render|main_stall|bridge_region_spawned|region_loaded)" \
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

# Region start time + server RSS per running region, on real region threads
# (no client, no LOD): `just genesis-probe 9 30 -9,-6` starts the 3x3 around
# region (-9,-6), keeps it ticking 30 s and prints RSS. Builds at the play
# region size; serves `world_file` if set (e.g. worlds/debug-embarked/terrain.world).
genesis-probe regions="1" hold="0" center="0,0":
    VOXEL_REGION_BLOCKS={{play_blocks}} {{cap}} {{cargo}} build --release -p server --example genesis_probe
    VOXEL_WORLD_FILE="{{world_file}}" ./target/release/examples/genesis_probe {{regions}} {{hold}} {{center}}

# Micro-benchmark of TerrainPipeline::column and its share of generate_stud_region.
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

# Bake the LDraw parts in tools/ldraw-bake/parts.txt into crates/game/src/stud_catalog.rs and
# assets/studs/ (LDRAW_DIR overrides assets/ldraw, which is git-ignored). `just ldraw-bake --check` verifies.
ldraw-bake *args:
    {{cap}} {{cargo}} run -p ldraw-bake --release -- --ldraw "${LDRAW_DIR:-assets/ldraw}" {{args}}

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

# Vertical slice of the generated ground (cover, soil, forge rock layers, water) as a
# PNG in target/cross-section/, read from the region's stud ground recipes (10 px per m).
# Flags: --world <file>|procedural --region X,Z --axis x|z --at <block> --depth <m> --out <png>;
# without --world it uses worlds/default.world, and a missing file is an error
# (export one: just worldctl export --db <store> <out.world>). Built with
# `play_blocks`, so --region / --at address the same grid as `just run`.
cross-section *args:
    VOXEL_REGION_BLOCKS={{play_blocks}} {{cap}} {{cargo}} run --release -p worldgen --example cross_section -- {{args}}

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
#     (`world-map`, `cross-section`, `history`, `genesis-probe`).
# <url> = a .sqlite path or a postgres:// URL. A store a running server holds is
# refused (its lock), never waited for. Built at `play_blocks`: the store's world
# row and its world map are tied to the region size (pass region_blocks=N for others).
# Pinned to `cap` cores ON PURPOSE, like forge-proto: the forge and the tile encoding
# use every core they get, and this is batch work beside a desktop or a server.
# World-store maintenance: forge a world into a store, export its forge file.
worldctl *args:
    VOXEL_REGION_BLOCKS={{play_blocks}} {{cap}} {{cargo}} run --release -p world-store --bin worldctl -- {{args}}

# Forge a world straight into a store: `just forge-db worlds/w.sqlite --size 32768 --cell 16`.
forge-db db *args:
    just region_blocks={{play_blocks}} worldctl forge --db {{db}} {{args}}

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

# ---- WASM / browser -------------------------------------------------------

# The server the browser build's main menu prefills in Join server (the deployed
# game's hostname: the WebTransport certificate is Let's Encrypt's for that name,
# so the bare deploy_host IP would fail the browser's TLS check). Baked into
# `web-release` / `deploy` bundles as VOXEL_WEB_SERVER; `just web` leaves it unset,
# so a dev page prefills 127.0.0.1 (the local `just server`).
web_server := "orion.factoriosurvivalworld.com"

# The login service the browser page should use (`just login=http://127.0.0.1:8790 web`):
# `web` prints the page URL with `?login=<url>`, which makes the join-only menu offer
# Play online. Empty = the plain Join-server menu.
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

# Forward the deploy box's k3s API to localhost:6443 (kubeconfig context `orion`).
tunnel:
    scripts/tunnel.sh {{deploy_host}}

# ---- Dev Postgres (deploy/dev) ---------------------------------------------

# Start the dev Postgres on 127.0.0.1:55432 and wait until it is healthy.
pg-up:
    docker compose -f deploy/dev/compose.yml up -d --wait
    @echo "dev database: {{dev_database_url}}"

# Stop the dev Postgres; its data stays in the mmo-dev-pg volume.
pg-down:
    docker compose -f deploy/dev/compose.yml down

# Delete the dev Postgres data and start again empty.
pg-reset:
    docker compose -f deploy/dev/compose.yml down -v
    docker compose -f deploy/dev/compose.yml up -d --wait

# Run psql inside the dev Postgres: `just pg-psql -c 'select version()'`.
[positional-arguments]
pg-psql *args:
    docker compose -f deploy/dev/compose.yml exec -T postgres psql -U mmo -d mmo "$@"

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
# (VOXEL_LOGIN_* env, database target/dev-login/login.sqlite); `just login world list`,
# `just login account verify a@b.c`, ... Built capped, then run.
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

# The dev account `dev-login` / `run-online` ensure (verified). Local dev database
# (target/dev-login/login.sqlite) only — not a secret.
dev_email := "dev@localhost"
dev_password := "dev-password"

# How long `run-online` waits for the login service and the server to come up.
online_wait_secs := "180"

# What `dev-login` and `run-online` need in the dev login database: a signing key,
# the world `dev` (127.0.0.1:6466 / :6467) and the verified account `dev_email`. The world
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
    printf '%s\n' "{{dev_password}}" | $login account ensure {{dev_email}} --verified

# A dev server that requires login, with the login service beside it: ensures a signing
# key, the world `dev` and the dev account (`dev_email` / `dev_password`), runs `login serve`
# in the background and the dev server (VOXEL_AUTH=jwt, play region size) in the
# foreground; SIGTERMs the login service on exit.
# `just with_web=1 dev-login` also serves the browser page (wasm-server-runner, :1334)
# and prints its `?login=` URL (log target/dev-login/web.log).
dev-login: _dev-login-setup
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
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
# `online_wait_secs`) the client in the foreground as `client debug online`: it logs in
# as `dev_email`, joins world `dev` with the ticket's JWT and embarks (returning to the
# account's fortress if it has one). Closing the client SIGTERMs the server (it saves
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
    VOXEL_LOGIN_URL=$login_url VOXEL_LOGIN_EMAIL={{dev_email}} VOXEL_LOGIN_PASSWORD={{dev_password}} \
        RUST_BACKTRACE=1 ./target/debug/client debug online

# ---- Split roles: world coordinator + region host (R2b) ---------------------------------

# The world as two processes on the dev Postgres world: `coordinator` (history, claims,
# world requests; no client edge, no regions) and `host` (regions, the client edge, save
# cuts) dialing it over the service link. Dev secrets live under the git-ignored
# target/dev-r2/: the shared service token (`service-token`) and the coordinator's
# certificate pin (`coordinator.cert-sha256`, its cert and private key beside it).
dev_r2_dir := "target/dev-r2"

# Make target/dev-r2/service-token (random, mode 0600) unless it exists.
_dev-r2-token:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p {{dev_r2_dir}}
    if [ ! -s {{dev_r2_dir}}/service-token ]; then
        (umask 077; head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' >{{dev_r2_dir}}/service-token)
        echo "made {{dev_r2_dir}}/service-token"
    fi

# The world coordinator alone (VOXEL_ROLE=coordinator) for the dev Postgres world
# (`just pg-up` first): serves hosts on 127.0.0.1:6480 and writes its pin to
# target/dev-r2/coordinator.cert-sha256. Play region size.
coordinator: _dev-r2-token
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    {{cap}} {{cargo}} build --bin server
    VOXEL_ROLE=coordinator VOXEL_WORLD_DB={{dev_database_url}} \
        VOXEL_SERVICE_TOKEN_FILE={{dev_r2_dir}}/service-token \
        VOXEL_COORDINATOR_CERT_HASH_FILE={{dev_r2_dir}}/coordinator.cert-sha256 \
        RUST_BACKTRACE=1 ./target/debug/server

# A region host (VOXEL_ROLE=host) of the dev Postgres world: host `ordinal` of `hosts`
# (static ownership), dialing the coordinator at 127.0.0.1:6480 (started first or later —
# it waits for the pin); clients connect on 127.0.0.1:6466 as with `just server`.
# Local player names unless VOXEL_AUTH=jwt is set (see `dev-r2`; with jwt the host also
# takes the login service's SpawnPlayer / Kick on 127.0.0.1:6490). Play region size.
# R2 runs ONE host per world: keep the defaults. `ordinal` != 0 / `hosts` > 1 is R3 work —
# the coordinator serves host ordinal 0 only, and a second host would also bind 6466.
host ordinal="0" hosts="1": _dev-r2-token
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    {{cap}} {{cargo}} build --bin server
    VOXEL_ROLE=host VOXEL_OWNERSHIP=static VOXEL_HOST_ORDINAL={{ordinal}} VOXEL_HOSTS={{hosts}} \
        VOXEL_WORLD_DB={{dev_database_url}} \
        VOXEL_SERVICE_TOKEN_FILE={{dev_r2_dir}}/service-token \
        VOXEL_COORDINATOR_CERT_HASH_FILE={{dev_r2_dir}}/coordinator.cert-sha256 \
        RUST_BACKTRACE=1 ./target/debug/server

# The split world with login: the dev Postgres (`pg-up`), the `dev-login` setup and login
# service, the coordinator (log target/dev-r2/coordinator.log) and, once it has written
# its pin, host 0 in the foreground (VOXEL_AUTH=jwt, world `dev`). The host listens for the
# login service's push on 127.0.0.1:6490 (pin target/dev-r2/host-0.cert-sha256); once it has
# written that pin, world `dev` is re-registered with the link, so joins push `SpawnPlayer`
# and the host admits a ticket only against it. SIGTERMs the coordinator and the login
# service on exit. Connect with `just run-online`-style login
# (`client debug online`) or the login screens. The first start on an empty dev database
# forges the world in the coordinator, which can take longer than the `online_wait_secs`
# (180 s) wait for its pin: then rerun (the world is kept), or raise it once with
# `just online_wait_secs=900 dev-r2`.
dev-r2: pg-up _dev-login-setup _dev-r2-token
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    {{cap}} {{cargo}} build --bin server
    pin={{dev_r2_dir}}/coordinator.cert-sha256
    host_pin={{dev_r2_dir}}/host-0.cert-sha256
    rm -f "$pin" "$host_pin"
    VOXEL_SERVICE_TOKEN_FILE={{dev_r2_dir}}/service-token \
        ./target/debug/login serve >target/dev-login/login.log 2>&1 </dev/null &
    lpid=$!
    cpid=""
    # Once the host has written its login-link pin: world `dev` gets the link.
    (
        until [ -s "$host_pin" ]; do sleep 0.5; done
        ./target/debug/login world add dev --name "Dev world" --quic 127.0.0.1:6466 --wt https://127.0.0.1:6467/ \
            --link 127.0.0.1:6490 --link-cert "$(cat "$host_pin")" >/dev/null
        echo "dev-r2: world dev registered with the host's login link (127.0.0.1:6490)"
    ) &
    rpid=$!
    trap 'echo "dev-r2: stopping the coordinator and the login service"; kill -TERM $cpid $lpid $rpid 2>/dev/null; wait $cpid $lpid 2>/dev/null || true' EXIT
    VOXEL_ROLE=coordinator VOXEL_WORLD_DB={{dev_database_url}} \
        VOXEL_SERVICE_TOKEN_FILE={{dev_r2_dir}}/service-token VOXEL_COORDINATOR_CERT_HASH_FILE="$pin" \
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
    VOXEL_ROLE=host VOXEL_OWNERSHIP=static VOXEL_HOST_ORDINAL=0 VOXEL_HOSTS=1 \
        VOXEL_WORLD_DB={{dev_database_url}} \
        VOXEL_SERVICE_TOKEN_FILE={{dev_r2_dir}}/service-token VOXEL_COORDINATOR_CERT_HASH_FILE="$pin" \
        VOXEL_HOST_LINK_BIND=127.0.0.1:6490 VOXEL_HOST_LINK_CERT_HASH_FILE="$host_pin" \
        VOXEL_AUTH=jwt VOXEL_WORLD_ID=dev VOXEL_LOGIN_URL=http://127.0.0.1:8790 \
        RUST_BACKTRACE=1 ./target/debug/server

# Run exactly the tests a change touches, by crate + cargo-test arguments:
#   just test-one game "--test it stud_lod::"
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

# Region threading: a player walking across the seam between two regions that
# run on different threads. Two layers, both tagged slow (so smoke / test-one /
# test skip them and only the long `test-full` sweep ran them):
#   - game `handoff_schedule`: the walk there and back under SEEDED scheduling
#     jitter (permuted tick order, late inputs, missed ticks) — a failure
#     names a seed that replays exactly — plus the rest of the game's seam
#     tests: the manager's handoff (`handoff_manager`), the halo, intents,
#     mining / placing / chests / piles across a seam, slimes, people and
#     bodies crossing, body arrivals (`seam_tests` below);
#   - server `threaded_world`: the same walk on real OS threads. Serial on
#     purpose: each test already spawns 9+ region threads.
# ~80 s–2 min warm (game ~45 s, the real-thread layer ~35 s, measured 2026-11-07). Run after
# touching handoff, the WorldManager, region_threads or anything that crosses
# a seam. The client-side twin (bit-exact convergence under late scheduling)
# needs a Bevy-sized test binary, so it stays an explicit opt-in:
#   just test-one client "--test it -- --include-ignored crossing::crossing_converges_under_late_region"
seam_tests := "handoff_schedule:: handoff_manager:: halo:: across_seam across_the_seam crosses_seam intents::intent_to_unloaded_owner mining::cooldown_does_not_cross item_piles::pickup_from_neighbour_pile item_piles::deposit_outside_region handoff_state::dynamic_arrival handoff_state::replayed_dynamic_arrival handoff_state::sleeping_body_arrives_asleep"
test-threads:
    {{cap}} {{cargo}} test -p game --test it -- --include-ignored {{seam_tests}}
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
# Local deployment environment: a single-node k0s cluster in docker (the same
# distribution as the planned single-node k0s host), configured by the
# OpenTofu roots under deploy/envs/local. Everyday work does NOT need it —
# `run` / `run-durable` stay native; this is for exercising the deployment
# (Agones game servers, CloudNativePG, storage). Design + findings:
# docs/superpowers/specs/2026-09-22-local-deployment-environment-design.md.
tofu := "tofu"
dev_cluster := "mmo-k0s"
k0s_image := "docker.io/k0sproject/k0s:v1.35.8-k0s.1"
dev_kubeconfig := justfile_directory() / "target/dev-cluster/kubeconfig"

# Start (or create) the local k0s node. Kubelet eviction thresholds are
# ABSOLUTE: the default "<15% disk free" taints the node disk-pressure on a
# large, fairly full laptop disk and nothing schedules.
dev-cluster-up:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! docker start {{dev_cluster}} >/dev/null 2>&1; then
        docker run -d --name {{dev_cluster}} --hostname {{dev_cluster}} --privileged --cgroupns=host \
            -v {{dev_cluster}}-data:/var/lib/k0s -v {{dev_cluster}}-openebs:/var/openebs \
            -p 127.0.0.1:16443:6443 {{k0s_image}} \
            k0s controller --enable-worker --no-taints \
            '--kubelet-extra-args=--eviction-hard=nodefs.available<5Gi,imagefs.available<5Gi,memory.available<500Mi --eviction-minimum-reclaim=nodefs.available=1Gi' >/dev/null
    fi
    mkdir -p "$(dirname {{dev_kubeconfig}})"
    until docker exec {{dev_cluster}} k0s kubeconfig admin > {{dev_kubeconfig}}.tmp 2>/dev/null; do sleep 2; done
    sed 's#server: https://.*:6443#server: https://127.0.0.1:16443#' {{dev_kubeconfig}}.tmp > {{dev_kubeconfig}} && rm {{dev_kubeconfig}}.tmp
    until KUBECONFIG={{dev_kubeconfig}} kubectl get nodes 2>/dev/null | grep -q ' Ready'; do sleep 2; done
    echo "k0s node ready at $(docker inspect -f '{{{{range .NetworkSettings.Networks}}{{{{.IPAddress}}{{{{end}}' {{dev_cluster}}) (game server ports are reachable there); KUBECONFIG={{dev_kubeconfig}}"

# Stop the node, keeping its state (volumes {{dev_cluster}}-data / -openebs).
dev-cluster-down:
    docker stop {{dev_cluster}}

# DESTRUCTIVE: remove the node and its volumes (database included) and the
# local OpenTofu state that described them.
dev-cluster-destroy:
    -docker rm -f {{dev_cluster}}
    -docker volume rm {{dev_cluster}}-data {{dev_cluster}}-openebs
    rm -f deploy/envs/local/platform/terraform.tfstate* deploy/envs/local/world/terraform.tfstate* {{dev_kubeconfig}}

# Apply the local environment: operators first (their CRDs must exist before
# the world root can plan its custom resources), then the world.
deploy-local: dev-cluster-up
    TF_VAR_kubeconfig={{dev_kubeconfig}} {{tofu}} -chdir=deploy/envs/local/platform init -input=false
    TF_VAR_kubeconfig={{dev_kubeconfig}} {{tofu}} -chdir=deploy/envs/local/platform apply -input=false -auto-approve
    TF_VAR_kubeconfig={{dev_kubeconfig}} {{tofu}} -chdir=deploy/envs/local/world init -input=false
    TF_VAR_kubeconfig={{dev_kubeconfig}} {{tofu}} -chdir=deploy/envs/local/world apply -input=false -auto-approve

# Show what deploy-local would change, without changing anything.
deploy-local-plan: dev-cluster-up
    TF_VAR_kubeconfig={{dev_kubeconfig}} {{tofu}} -chdir=deploy/envs/local/platform init -input=false
    TF_VAR_kubeconfig={{dev_kubeconfig}} {{tofu}} -chdir=deploy/envs/local/platform plan -input=false
    TF_VAR_kubeconfig={{dev_kubeconfig}} {{tofu}} -chdir=deploy/envs/local/world init -input=false
    TF_VAR_kubeconfig={{dev_kubeconfig}} {{tofu}} -chdir=deploy/envs/local/world plan -input=false

# kubectl against the local cluster, e.g. `just kube get gs -A`.
[positional-arguments]
kube *args:
    KUBECONFIG={{dev_kubeconfig}} kubectl "$@"

# ---------------------------------------------------------------------------
# The orion k3s server (Hetzner 5.9.7.138, kube context `orion`): OpenTofu roots
# under deploy/envs/orion — `bootstrap` (the S3 state bucket), `platform`
# (cert-manager, Traefik settings), `monitoring` (Grafana, Prometheus + Thanos, Loki, Tempo,
# Pyroscope, Alloy; data in S3), `picoshare` (file sharing at
# https://share.factoriosurvivalworld.com). State lives in S3 (deploy/envs/orion/backend.s3.tfbackend,
# key orion/<root>.tfstate); AWS credentials come from the default chain
# (`AWS_PROFILE=…` to pick a profile). Every recipe pins the `orion` context,
# never the current one. Plan before every apply. Design:
# docs/superpowers/plans/2026-11-06-k3s-observability-stack-handoff.md.
orion_context := "orion"

# `tofu init` an orion root (bootstrap | platform | monitoring | picoshare) against its S3 state.
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
# fresh server: platform, then monitoring and picoshare (either order).
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

# Worldgen suite: pure-function generation, the forge, forged worlds, the tile map.
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
