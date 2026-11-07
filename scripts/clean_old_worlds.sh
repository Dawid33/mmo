#!/usr/bin/env bash
# Lists, and with --yes deletes, the save files a world written before the
# world store (terrain.sqlite) left behind under worlds/:
#   *.regions/ and *.players/ directories, *.history and *.history.bak files,
#   and any *.region.bak. Nothing else is touched: forge files (*.world),
#   world.json, preview.png, server logs, and anything outside worlds/.
# The store's own files (terrain.sqlite, terrain.sqlite.history.bak, ...) are
# excluded by name: *.history.bak would otherwise match the store's history
# backup, which is live data.
# Without --yes it only prints what it would delete, with sizes.
set -euo pipefail

usage() { echo "usage: scripts/clean_old_worlds.sh [--yes]"; }

yes=0
for arg in "$@"; do
    case "$arg" in
        --yes) yes=1 ;;
        -h | --help) usage; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; usage >&2; exit 2 ;;
    esac
done

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

if [ ! -d worlds ]; then
    echo "no worlds/ directory; nothing to clean"
    exit 0
fi

paths=()
while IFS= read -r -d '' p; do
    paths+=("$p")
done < <(
    find worlds -maxdepth 2 \
        \( -type d \( -name '*.regions' -o -name '*.players' \) ! -name '*sqlite*' -print0 -prune \) \
        -o \
        \( -type f \( -name '*.history' -o -name '*.history.bak' -o -name '*.region.bak' \) \
            ! -name '*sqlite*' -print0 \)
)

if [ "${#paths[@]}" -eq 0 ]; then
    echo "nothing to clean under worlds/"
    exit 0
fi

for p in "${paths[@]}"; do
    printf '%8s  %s\n' "$(du -sh -- "$p" | cut -f1)" "$p"
done
echo "total: $(du -shc -- "${paths[@]}" | tail -n 1 | cut -f1)"

if [ "$yes" -eq 0 ]; then
    echo "re-run with --yes to delete"
    exit 0
fi

for p in "${paths[@]}"; do
    rm -rf -- "$p"
done
echo "deleted ${#paths[@]} path(s)"
