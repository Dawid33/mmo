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
# Run the first-party test suites (rollback + client headless).
test: test-game test-client

# Rollback/undo invariants and seeded randomized rollback.
test-game:
    cargo test -p game

# Client headless suite (sim-bridge, conversion, interpolation, meshing).
test-client:
    cargo test -p client

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
    docker-compose up -d

# Stop the profiling stack.
profile-down:
    docker-compose down

# Record a perf profile of the client and open hotspot (see scripts/perf.sh).
perf:
    sudo perf script record -F9997 --call-graph dwarf,16384 -e cpu-clock ./target/debug/client
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
perf-client:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    {{cargo}} build --bin client
    RUST_BACKTRACE=1 {{cargo}} run --bin server &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT
    sudo -E perf record {{perf_flags}} ./target/debug/client
    sudo chmod +r perf.data
    perf script --no-inline | inferno-collapse-perf > perf-folded-client.txt
    echo "wrote perf-folded-client.txt — paste it into an LLM to find hot paths"

# Profile the server while the client runs; Ctrl-C the server to stop recording.
perf-server:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    {{cargo}} build --bin server
    RUST_BACKTRACE=1 {{cargo}} run --bin client &
    client_pid=$!
    trap 'kill "$client_pid" 2>/dev/null || true' EXIT
    sudo -E perf record {{perf_flags}} ./target/debug/server
    sudo chmod +r perf.data
    perf script --no-inline | inferno-collapse-perf > perf-folded-server.txt
    echo "wrote perf-folded-server.txt — paste it into an LLM to find hot paths"

# ---- Housekeeping ---------------------------------------------------------

# Remove build artifacts.
clean:
    cargo clean
