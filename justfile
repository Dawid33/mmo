# justfile — task runner for the voxel MMO workspace.
# Run `just` (or `just --list`) to see available recipes.
#
# Machine-specific bits are variables you can override on the command line, e.g.
#   just cargo=$CARGO_CLIF build   # use the Cranelift wrapper instead of stable cargo
#   just adapter=6700 client       # pin wgpu to the RX 6700 XT eGPU

# Cargo command used by all build/run/check recipes. Defaults to stable `cargo`.
# For faster dev builds, opt into the Cranelift codegen wrapper by pointing this at
# cargo-clif — export CARGO_CLIF, or override per-invocation:
#   just cargo=~/Software/rustc_codegen_cranelift/dist/cargo-clif build
cargo := env_var_or_default("CARGO_CLIF", "cargo")

# Substring passed to WGPU_ADAPTER_NAME to select a GPU (empty = wgpu default).
adapter := ""

# World size the client subscribes to (empty = production `full`). The server
# only generates what clients request, so this shrinks both sides:
#   tiny  = 1 simulated region, no LOD rings      (cheapest world that loads)
#   small = 1 simulated region + one factor-2 ring (24 LOD keys)
#   full  = 3x3 simulated regions + all five rings (~570 LOD keys)
# e.g. `just world=tiny run`, `just world=small render-bench`. Finer control:
# export VOXEL_NEAR_RADIUS=0..3 / VOXEL_LOD_RINGS=off|small|full|"2:2,4:4"
# (see crates/client/src/world_scale.rs). Every recipe inherits it; tests
# ignore it (GameInstanceManager::new is env-free).
world := ""
export VOXEL_WORLD := world

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

# Every recipe inherits these: cargo (and the cargo-clif wrapper) reads
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
    RUST_BACKTRACE=1 {{cargo}} run --bin server

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
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    RUST_BACKTRACE=1 {{cargo}} run --bin client

# Run server + client together. Both are built up front (no build-lock race
# between two `cargo run`s), the server starts first in the background, and
# closing the client window (or Ctrl-C) stops both.
run:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    {{cap}} {{cargo}} build --bin client --bin server
    RUST_BACKTRACE=1 ./target/debug/server &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    RUST_BACKTRACE=1 ./target/debug/client

# Release-mode variants: production performance. Undo verification defaults to
# Delta in every build; set HASH_MODE=full (or off) on any recipe to override,
# e.g. `HASH_MODE=full just run` when chasing a footprint/capture gap. Use
# perf-client / perf-server when you also want a profile out of the run.

# Run the QUIC server in release mode.
server-release:
    RUST_BACKTRACE=1 {{cargo}} run --release --bin server

# Run the Bevy client in release mode.
client-release:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    RUST_BACKTRACE=1 {{cargo}} run --release --bin client

# Run server + client together in release mode (same shape as `run`).
run-release:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    {{cap}} {{cargo}} build --release --bin client --bin server
    RUST_BACKTRACE=1 ./target/release/server &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    RUST_BACKTRACE=1 ./target/release/client

# Interactive play-test: build, run server + client, and tee both to
# target/play-test/*.log so the run can be reviewed for errors afterwards.
# Close the client window (or Ctrl-C) to stop both; on exit it prints a
# panic/error summary and the log paths.
play-test:
    #!/usr/bin/env bash
    set -uo pipefail
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

# Time one region's collider genesis build + determinism check.
collider-bench:
    {{cargo}} run --release -p worldgen --example collider_profile

# Micro-benchmark of TerrainPipeline::column and its share of generate_region.
bench-column:
    {{cargo}} run --release -p worldgen --example column_bench

# Headless region-load cost breakdown (gen/derive/serde/mesh).
bench-region:
    {{cap}} {{cargo}} test -p worldgen --release --test region_load_cost -- --ignored --nocapture

# Self-screenshot probe: the pulled-quad render path draws real terrain.
pull-smoke:
    {{cargo}} run -p client --example pull_smoke

# Production render benchmark: real server seed + full LOD table, fixed
# 1920x1080 no-vsync client, three repeated JSON runs by default.
# Pair modes: per-chunk+raster, direct+raster, gpu+raster, gpu+brick.
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
            far="raster"
            ;;
        direct|direct+raster)
            near="direct"
            far="raster"
            ;;
        gpu|gpu+raster)
            near="gpu"
            far="raster"
            ;;
        gpu+brick)
            near="gpu"
            far="brick"
            ;;
        *)
            echo "invalid backend '$selected_backend' (expected per-chunk+raster, direct+raster, gpu+raster, or gpu+brick)" >&2
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
        VOXEL_FAR_RENDER="$far" \
        VOXEL_CLIENT_WORKER_THREADS="$selected_threads" \
        TOKIO_WORKER_THREADS="$selected_threads" \
        RUST_BACKTRACE=1 \
            ./target/release/client
    done

# ---- WASM / browser -------------------------------------------------------

# Start `just server` first, or append ?offline in the browser for single-player.
# Needs: rustup target add wasm32-unknown-unknown; cargo install wasm-server-runner.
# Serve the wasm client via wasm-server-runner (open http://127.0.0.1:1334/).
web:
    WASM_SERVER_RUNNER_CUSTOM_INDEX_HTML=crates/client/index.html \
        {{cargo}} run -p client --target wasm32-unknown-unknown

# Needs: wasm-bindgen-cli and wasm-opt (binaryen) on PATH.
# Build the optimized wasm release bundle into public/ (see scripts/web_release.rs).
web-release:
    {{cap}} {{cargo}} build -p client --profile wasm-release --target wasm32-unknown-unknown
    wasm-bindgen --out-name client --out-dir public --target web \
        target/wasm32-unknown-unknown/wasm-release/client.wasm
    wasm-opt -O -ol 100 -s 100 -o public/client_bg.wasm public/client_bg.wasm

# ---- Test -----------------------------------------------------------------

# Scoped per-crate on purpose: `cargo test --workspace` fails building the
# vendored nalgebra fork's own tests.
#
# Tests measured >100ms (2026-07-08, debug build) carry
# `#[ignore = "slow: >100ms — run via just test-full"]`, so the default run is
# the fast feedback loop and `test-full` adds them back via --include-ignored.
# Tag new slow tests the same way; re-measure with:
#   RUSTC_BOOTSTRAP=1 cargo test -p <crate> -- -Zunstable-options --report-time

# The routine check, hard-limited to `smoke_timeout` seconds (default 60). Even
# the "quick" per-crate suites below link ~30 game test binaries and ~9
# Bevy-sized client ones, which is far too slow for a while-iterating loop. So
# smoke runs two game test binaries (undo/rollback invariants, region
# park/restore) and the server lib tests, and only TYPE-CHECKS the client
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
    step {{cargo}} test -q -p game --test simple --test region_runner
    step {{cargo}} test -q -p server --lib
    step {{cargo}} check -q -p client --lib --bins --tests
    echo ">> smoke ok in $(( $(date +%s) - deadline + {{smoke_timeout}} ))s"

# Run exactly the tests a change touches, by crate + cargo-test arguments:
#   just test-one game "--test lod_streaming pristine"
#   just test-one client "--lib world_scale::"
# Naming `--lib` / `--test <target>` matters: it skips building (and linking)
# every other test binary in the crate.
test-one crate *args:
    {{cap}} {{cargo}} test -p {{crate}} {{args}}

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
    {{cap}} {{cargo}} test -p server

# Worldgen suite: pure-function generation + the dev disk cache.
test-worldgen:
    {{cap}} {{cargo}} test -p worldgen

# Everything, slow correctness tests included — run before calling work done.
# Still skips wall-clock perf gates, evidence-only tests, and the explicit
# region-load benchmark; those retain their dedicated recipes.
test-full:
    {{cap}} {{cargo}} test -p game -- --include-ignored --test-threads={{test_threads}} --skip perf_evidence_journal_scales_with_activity --skip perf_evidence_hash_modes
    {{cap}} {{cargo}} test -p client -- --include-ignored --test-threads={{test_threads}} --skip region_tick_cost_fits_keepup_budget --skip soak_realtime_keepup
    {{cap}} {{cargo}} test -p server -- --include-ignored --test-threads={{test_threads}}
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
    {{cap}} {{cargo}} build --release --bin client --bin server
    scripts/perf_capture.sh "{{adapter}}" "{{world}}"

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
# The feature build lands in target/release/client like any other (the next plain release
# recipe relinks it); it is built under `cap` and launched directly, never via `cargo run`.
#
# Per-ECS-system frame breakdown as text (which system eats the frame).
trace-chrome:
    #!/usr/bin/env bash
    set -uo pipefail
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    mkdir -p target/trace
    json=target/trace/client.json
    {{cap}} {{cargo}} build --release --bin server
    {{cap}} {{cargo}} build --release --bin client --features trace_chrome
    RUST_BACKTRACE=1 ./target/release/server > target/trace/server.log 2>&1 &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    sleep 1
    echo ">> tracing -> $json  (play for ~30s, then CLOSE THE WINDOW — Ctrl-C truncates it)"
    TRACE_CHROME="$json" RUST_BACKTRACE=1 ./target/release/client
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
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    mkdir -p target/trace
    json=target/trace/client.json
    {{cap}} {{cargo}} build --release --bin server
    {{cap}} {{cargo}} build --release --bin client --features trace_chrome
    RUST_BACKTRACE=1 ./target/release/server > target/trace/server.log 2>&1 &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    sleep 1
    echo ">> tracing the join -> $json  (wait for the world to load, then CLOSE THE WINDOW)"
    TRACE_CHROME="$json" RUST_BACKTRACE=1 ./target/release/client
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
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    RUST_BACKTRACE=1 {{cargo}} run --bin server &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    PYROSCOPE=1 RUST_BACKTRACE=1 {{cargo}} run --bin client --features pyroscope

# Server instrumented with pyroscope, client plain alongside; pushes to :4040 (`just profile-up` first).
profile-server:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    RUST_BACKTRACE=1 {{cargo}} run --bin client &
    client_pid=$!
    trap 'kill "$client_pid" 2>/dev/null || true' EXIT
    PYROSCOPE=1 RUST_BACKTRACE=1 {{cargo}} run --bin server --features pyroscope

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
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    feat="tracy"
    if [ -n "{{detailed}}" ]; then feat="tracy-detailed"; fi
    {{cap}} {{cargo}} build --bin server
    RUST_BACKTRACE=1 {{cargo}} run --bin server &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    RUST_BACKTRACE=1 {{cargo}} run --bin client --profile profiling --features "$feat"

# Record a perf profile of the client and open hotspot (see scripts/perf.sh).
# Uses the `profiling` cargo profile (release + debug info): dev's opt-level 1
# distorts hot loops ~2.5x, so debug captures don't represent production.
perf:
    {{cap}} {{cargo}} build --bin client --profile profiling
    sudo perf script record -F9997 --call-graph dwarf,16384 -e cpu-clock ./target/profiling/client
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
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    {{cap}} {{cargo}} build --bin client --bin server --profile profiling
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

# ---- Housekeeping ---------------------------------------------------------

# Remove build artifacts.
clean:
    {{cargo}} clean
