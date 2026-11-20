#!/usr/bin/env bash
# Run one wasm release build stage in a bounded systemd user scope. The scope
# includes every descendant, including rustc, the linker and wasm-opt workers.
set -uo pipefail

inside_scope() {
    local state_file="$1"
    shift
    printf 'started\n' >"$state_file" || return 125

    "$@"
    local status=$?

    local cgroup_path=""
    local hierarchy controllers path
    while IFS=: read -r hierarchy controllers path; do
        if [ "$hierarchy" = 0 ] && [ -z "$controllers" ]; then
            cgroup_path="$path"
            break
        fi
    done </proc/self/cgroup

    local oom=0
    local oom_kill=0
    local key value
    local events="/sys/fs/cgroup${cgroup_path}/memory.events"
    if [ -n "$cgroup_path" ] && [ -r "$events" ]; then
        while read -r key value; do
            case "$key" in
                oom) oom=$value ;;
                oom_kill) oom_kill=$value ;;
            esac
        done <"$events"
    fi
    printf 'status=%s\noom=%s\noom_kill=%s\n' "$status" "$oom" "$oom_kill" >>"$state_file"

    if [ "$status" -ne 0 ] && { [ "$oom" -gt 0 ] || [ "$oom_kill" -gt 0 ]; }; then
        echo "wasm-build-guard: $WASM_BUILD_GUARD_LABEL hit the cgroup memory limit (OOM kills=$oom_kill)." >&2
        echo "wasm-build-guard: the guarded stage failed before the release directory was replaced; raise WASM_BUILD_MEMORY_MAX/WASM_BUILD_MEMORY_SWAP_MAX or run on a larger machine." >&2
    fi
    exit "$status"
}

if [ "${1:-}" = --inside-scope ]; then
    shift
    inside_scope "$@"
    exit $?
fi

if [ "$#" -lt 3 ] || [ "$2" != -- ]; then
    echo "usage: $0 <stage> -- <command> [args ...]" >&2
    exit 64
fi

label="$1"
shift 2

if ! command -v systemd-run >/dev/null 2>&1; then
    echo "wasm-build-guard: systemd-run is required for $label; refusing to run wasm release stages without a memory cgroup." >&2
    echo "wasm-build-guard: install systemd with a working user manager, then retry." >&2
    exit 125
fi

memory_high="${WASM_BUILD_MEMORY_HIGH:-14G}"
memory_max="${WASM_BUILD_MEMORY_MAX:-16G}"
memory_swap_max="${WASM_BUILD_MEMORY_SWAP_MAX:-0}"

systemd_limit() {
    if [ "$1" = none ]; then
        printf 'infinity'
    else
        printf '%s' "$1"
    fi
}

echo "wasm-build-guard: $label: MemoryHigh=$memory_high MemoryMax=$memory_max MemorySwapMax=$memory_swap_max (user systemd scope, descendants included)"

state_file=$(mktemp "${TMPDIR:-/tmp}/mmo-wasm-build-guard.XXXXXX") ||
    { echo "wasm-build-guard: cannot create scope status file" >&2; exit 125; }
trap 'rm -f "$state_file"' EXIT

set +e
WASM_BUILD_GUARD_LABEL="$label" systemd-run \
    --user \
    --scope \
    --quiet \
    --collect \
    --description="Errantry wasm release: $label" \
    "--property=MemoryHigh=$(systemd_limit "$memory_high")" \
    "--property=MemoryMax=$(systemd_limit "$memory_max")" \
    "--property=MemorySwapMax=$(systemd_limit "$memory_swap_max")" \
    "$0" --inside-scope "$state_file" "$@"
status=$?
set -e

started=false
recorded_status=false
oom=0
oom_kill=0
while IFS='=' read -r key value; do
    case "$key" in
        started) started=true ;;
        status) recorded_status=true ;;
        oom) oom=$value ;;
        oom_kill) oom_kill=$value ;;
    esac
done <"$state_file"

if [ "$started" != true ]; then
    echo "wasm-build-guard: could not start the user systemd scope for $label (systemd-run exited $status)." >&2
    echo "wasm-build-guard: ensure the cgroup v2 user manager is available and delegated; refusing to run unbounded." >&2
    exit "$status"
fi

if [ "$recorded_status" != true ] && [ "$status" -ne 0 ]; then
    echo "wasm-build-guard: $label ended inside the scope without recording a command status; the cgroup or command likely killed the wrapper." >&2
    echo "wasm-build-guard: check MemoryMax=$memory_max and MemorySwapMax=$memory_swap_max before retrying; the build will not fall back unbounded." >&2
fi

if [ "$status" -eq 137 ] && [ "$oom_kill" -eq 0 ]; then
    echo "wasm-build-guard: $label exited by SIGKILL while scoped; the command or cgroup may have killed it." >&2
    echo "wasm-build-guard: check MemoryMax=$memory_max and MemorySwapMax=$memory_swap_max before retrying; the build will not fall back unbounded." >&2
fi

exit "$status"
