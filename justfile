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

# perf sampling flags shared by the perf-client / perf-server recipes.
perf_flags := "-F9997 --call-graph dwarf,16384 -e cpu-clock"

# Server address (informational; hardcoded to 127.0.0.1:6466 in the server).
_default:
    @just --list

# ---- Build ----------------------------------------------------------------

# Build the whole workspace (all bins) with the configured cargo (stable by default).
build:
    {{cargo}} build --workspace --bins

# Build with stable cargo always, ignoring any Cranelift override.
build-stable:
    cargo build --workspace --bins

# Type-check the whole workspace without producing binaries.
check:
    {{cargo}} check --workspace

# ---- Run ------------------------------------------------------------------

# Run the QUIC server (listens on 127.0.0.1:6466, WebTransport ingress on :6467).
server:
    RUST_BACKTRACE=1 {{cargo}} run --bin server

# Run the Bevy client. Override `adapter` to pin a GPU, e.g. `just adapter=6700 client`.
client:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    RUST_BACKTRACE=1 {{cargo}} run --bin client

# Run server + client together (client in background, mirrors scripts/run.sh).
run:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    RUST_BACKTRACE=1 {{cargo}} run --bin client &
    client_pid=$!
    trap 'kill "$client_pid" 2>/dev/null || true' EXIT
    RUST_BACKTRACE=1 {{cargo}} run --bin server

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

# Run server + client together in release mode.
run-release:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    RUST_BACKTRACE=1 {{cargo}} run --release --bin client &
    client_pid=$!
    trap 'kill "$client_pid" 2>/dev/null || true' EXIT
    RUST_BACKTRACE=1 {{cargo}} run --release --bin server

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
    if ! {{cargo}} build --workspace --bins 2>&1 | tee "$log_dir/build.log"; then
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
    {{cargo}} test -p worldgen --release --test region_load_cost -- --ignored --nocapture

# Self-screenshot probe: the pulled-quad render path draws real terrain.
pull-smoke:
    {{cargo}} run -p client --example pull_smoke

# ---- WASM / browser -------------------------------------------------------

# Start `just server` first, or append ?offline in the browser for single-player.
# Needs: rustup target add wasm32-unknown-unknown; cargo install wasm-server-runner.
# Serve the wasm client via wasm-server-runner (open http://127.0.0.1:1334/).
web:
    WASM_SERVER_RUNNER_CUSTOM_INDEX_HTML=crates/client/index.html \
        cargo run -p client --target wasm32-unknown-unknown

# Needs: wasm-bindgen-cli and wasm-opt (binaryen) on PATH.
# Build the optimized wasm release bundle into public/ (see scripts/web_release.rs).
web-release:
    cargo build -p client --profile wasm-release --target wasm32-unknown-unknown
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

# Quick first-party suites (slow tests skipped) — the routine while-iterating check.
test: test-game test-client test-server

# Rollback/undo invariants and seeded randomized rollback (quick subset).
test-game:
    cargo test -p game

# Client headless suite: sim-bridge, conversion, interpolation, meshing (quick subset).
test-client:
    cargo test -p client

# Server suite compile-check: every server test is currently tagged slow
# (multi-second region-thread rigs), so the quick run builds them and runs 0.
test-server:
    cargo test -p server

# Everything, slow tests included — run before calling work done. Still skips
# the wall-clock perf gates, which have their own release-mode recipes
# (`just keepup-budget`, `just soak`) or are evidence-only printouts.
test-full:
    cargo test -p game -- --include-ignored --skip perf_evidence_journal_scales_with_activity --skip perf_evidence_hash_modes
    cargo test -p client -- --include-ignored --skip region_tick_cost_fits_keepup_budget --skip soak_realtime_keepup
    cargo test -p server -- --include-ignored

# Keep-up budget benchmark (release): measures the region-tick cost and
# asserts the death-spiral inequality (worst frame < tick period). Fails with
# a number when a game change makes the sim too slow for the client to keep up.
keepup-budget:
    cargo test -p client --release --test keepup_budget -- --ignored --nocapture

# Wall-clock keep-up soak (release, ~40s + build): real tick cadence + 60/s
# fabricated input; fails if the prediction log trends up (death-spiral
# signature). Debug builds measure but don't assert. Full output (incl. the
# reconcile tracing) goes to target/soak.log; the [client-budget] / [soak]
# summary lines are reprinted at the end so the attribution is readable.
soak:
    #!/usr/bin/env bash
    set -uo pipefail
    log=target/soak.log
    cargo test -p client --release --test reconcile_keepup soak_realtime_keepup -- --ignored --nocapture --test-threads=1 2>&1 | tee "$log"
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
    cargo fmt --all

# Check formatting without writing changes.
fmt-check:
    cargo fmt --all --check

# Clippy across the workspace (warnings as errors).
clippy:
    cargo clippy --workspace --bins -- -D warnings

# ---- Profiling ------------------------------------------------------------

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
    {{cargo}} build --bin server
    RUST_BACKTRACE=1 {{cargo}} run --bin server &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    RUST_BACKTRACE=1 {{cargo}} run --bin client --profile profiling --features "$feat"

# Record a perf profile of the client and open hotspot (see scripts/perf.sh).
# Uses the `profiling` cargo profile (release + debug info): dev's opt-level 1
# distorts hot loops ~2.5x, so debug captures don't represent production.
perf:
    {{cargo}} build --bin client --profile profiling
    sudo perf script record -F9997 --call-graph dwarf,16384 -e cpu-clock ./target/profiling/client
    sudo chmod +r perf.data
    hotspot

# Both perf-* recipes need inferno on PATH: `cargo install inferno`. They record
# folded stacks (perf-folded-{client,server}.txt) to paste into an LLM for hot-path
# analysis, running the other side (like `just run`) to make the load representative.
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
    {{cargo}} build --bin client --bin server --profile profiling
    RUST_BACKTRACE=1 ./target/profiling/server &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    sudo -E perf record {{perf_flags}} ./target/profiling/client
    sudo chmod +r perf.data
    perf script --no-inline | inferno-collapse-perf > perf-folded-client.txt
    echo "wrote perf-folded-client.txt — paste it into an LLM to find hot paths"

# Profile the server while the client runs; Ctrl-C the server to stop recording.
# Same `profiling` profile rationale as perf-client.
perf-server:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    {{cargo}} build --bin client --bin server --profile profiling
    RUST_BACKTRACE=1 ./target/profiling/client &
    client_pid=$!
    trap 'kill "$client_pid" 2>/dev/null || true' EXIT
    sudo -E perf record {{perf_flags}} ./target/profiling/server
    sudo chmod +r perf.data
    perf script --no-inline | inferno-collapse-perf > perf-folded-server.txt
    echo "wrote perf-folded-server.txt — paste it into an LLM to find hot paths"

# ---- Housekeeping ---------------------------------------------------------

# Remove build artifacts.
clean:
    cargo clean
