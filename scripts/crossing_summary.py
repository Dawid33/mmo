#!/usr/bin/env python3
"""Reduce a crossing probe's client log (`just fly-probe` / `just shuttle-probe`)
to one table: per metric, the distribution over every crossing.

Reads the `[perf] event=region_crossing` (game thread) and
`region_crossing_render` (render; only its `after:` half) lines, `main_stall`,
the autopilot's own events, and — when the client ran with PERF_LOG=1 — the
`[client-motion]` chunk-crossing lines.

Usage: crossing_summary.py [--by KEY] [--p95] <client.log>

  --by KEY  also split the game-thread crossings by the value of KEY on their line
            (e.g. `--by host_flip`: crossings between two hosts' regions vs within one).
  --p95     per group (or over all crossings): the p95 of correction_max, corrections
            and confirm_ms only.
"""
import argparse
import re

ANSI = re.compile(r"\x1b\[[0-9;]*m")
KV = re.compile(r"(\w+)=(-?[0-9.]+)")

GAME_KEYS = [
    ("tick_gap_max_ms", "longest gap between home ticks (ms)"),
    ("home_stalled_ticks", "home ticks sat out"),
    ("corrections", "home corrections"),
    ("correction_max", "largest correction (u)"),
    ("snapshots", "snapshots loaded"),
    ("snapshot_ms", "snapshot load time (ms)"),
    ("confirm_ms", "server PlayerRegion confirm (ms)"),
    ("window_ms", "window reported (ms)"),
]
RENDER_KEYS = [
    ("frame_max_ms", "worst frame (ms)"),
    ("long_frames", "frames > 25 ms"),
    ("hold_max_ms", "longest sim-pose hold (ms)"),
]


def pct(values, q):
    s = sorted(values)
    return s[min(len(s) - 1, int(round(q * (len(s) - 1))))]


def row(label, values):
    if not values:
        return f"  {label:<36} {'-':>8}"
    return (
        f"  {label:<36} {pct(values, 0.5):>8.1f} {pct(values, 0.95):>8.1f} "
        f"{max(values):>8.1f} {sum(values):>9.1f}"
    )


P95_KEYS = ["correction_max", "corrections", "confirm_ms"]


def crossing_lines(lines):
    """The key=value pairs of every game-thread `event=region_crossing` line."""
    out = []
    for raw in lines:
        line = ANSI.sub("", raw)
        if "event=region_crossing " in line:
            out.append({k: float(v) for k, v in KV.findall(line.split("event=region_crossing", 1)[1])})
    return out


def group_by(crossings, key):
    """Crossings by the value of `key` (as text; `-` when a line lacks it)."""
    groups = {}
    for c in crossings:
        value = c.get(key)
        label = "-" if value is None else (str(int(value)) if value == int(value) else str(value))
        groups.setdefault(label, []).append(c)
    return groups


def p95_by(crossings, key=None):
    """{group: {metric: p95}} for the P95_KEYS (one group "all" without `key`); a
    metric no crossing of a group has (or only as -1, "never") is left out."""
    groups = group_by(crossings, key) if key else {"all": crossings}
    out = {}
    for label, members in sorted(groups.items()):
        out[label] = {}
        for metric in P95_KEYS:
            values = [m[metric] for m in members if metric in m and m[metric] >= 0]
            if values:
                out[label][metric] = pct(values, 0.95)
    return out


def print_groups(crossings, key, p95_only):
    groups = group_by(crossings, key) if key else {"all": crossings}
    head = f"  {'metric (per crossing)':<36} {'p50':>8} {'p95':>8} {'max':>8} {'sum':>9}"
    for label, members in sorted(groups.items()):
        print(f"\nregion crossings, {key}={label}: n={len(members)}" if key else f"\nregion crossings: n={len(members)}")
        if p95_only:
            for metric, value in p95_by(members)["all"].items():
                print(f"  p95 {metric:<32} {value:>8.2f}")
            continue
        print(head)
        for k, label_text in GAME_KEYS:
            print(row(label_text, [g[k] for g in members if k in g and g[k] >= 0]))


def main(path, by=None, p95_only=False):
    game, render, stalls, motion, auto, faults = [], [], [], [], {}, []
    for raw in open(path, errors="replace"):
        line = ANSI.sub("", raw)
        if "DeviceLost" in line or "panicked at" in line:
            faults.append(line.strip()[:160])
        if "event=region_crossing_render" in line:
            after = line.split("after:", 1)[1].split("| before:", 1)[0]
            render.append({k: float(v) for k, v in KV.findall(after)})
        elif "event=region_crossing " in line:
            game.append({k: float(v) for k, v in KV.findall(line.split("event=region_crossing", 1)[1])})
        elif "event=main_stall" in line:
            m = re.search(r"system=(\S+) ms=([0-9.]+)", line)
            if m:
                stalls.append((m.group(1), float(m.group(2))))
        elif "[client-motion]" in line:
            m = re.search(
                r"chunk crossings=(\d+) frame avg=([0-9.]+)ms max=([0-9.]+)ms .*"
                r"crossing avg=([0-9.]+)u max=([0-9.]+)u .*"
                r"sim pose stepped back n=(\d+) sum=[0-9.]+u max=([0-9.]+)u",
                line,
            )
            if m:
                motion.append(tuple(float(x) for x in m.groups()))
        for ev in ("autopilot_start", "autopilot_shuttle", "autopilot_done", "world_ready"):
            if f"event={ev} " in line or line.rstrip().endswith(f"event={ev}"):
                auto[ev] = line.split(f"event={ev}", 1)[1].strip()

    print(f"crossing summary: {path}")
    if faults:
        # A lost GPU (on this box: the eGPU's Thunderbolt link, see pciehp in
        # dmesg) ends the run early and freezes the camera first — the numbers
        # before it are real, the run is just short.
        print(f"  !! RUN ABORTED: {faults[0]}")
    for ev in ("autopilot_shuttle", "autopilot_done"):
        if ev in auto:
            print(f"  {ev}: {auto[ev]}")
    head = f"  {'metric (per crossing)':<36} {'p50':>8} {'p95':>8} {'max':>8} {'sum':>9}"
    if game and (by or p95_only):
        print_groups(game, by, p95_only)
        if p95_only:
            return
    elif game:
        print(f"\nregion crossings (game thread): n={len(game)}")
        print(head)
        for key, label in GAME_KEYS:
            print(row(label, [g[key] for g in game if key in g and g[key] >= 0]))
    if render:
        print(f"\nregion crossings (render, window after the flip): n={len(render)}")
        print(head)
        for key, label in RENDER_KEYS:
            print(row(label, [r[key] for r in render if key in r]))
    by_system = {}
    for system, ms in stalls:
        by_system.setdefault(system, []).append(ms)
    print(f"\nmain-thread stalls > 8 ms: n={len(stalls)}")
    for system, values in sorted(by_system.items(), key=lambda kv: -max(kv[1])):
        print(f"  {system:<36} n={len(values):<5} p50={pct(values, 0.5):.1f} max={max(values):.1f} ms")
    if motion:
        crossings = sum(m[0] for m in motion)
        print(f"\nchunk crossings ([client-motion] 5 s windows, PERF_LOG=1): n={int(crossings)}")
        print(f"  {'metric (per window)':<36} {'p50':>8} {'p95':>8} {'max':>8}")
        for i, label in [
            (1, "crossing-frame avg (ms)"),
            (2, "crossing-frame max (ms)"),
            (4, "camera trail at crossing, max (u)"),
            (5, "sim pose steps against heading *"),
            (6, "largest such step (u)"),
        ]:
            v = [m[i] for m in motion]
            print(f"  {label:<36} {pct(v, 0.5):>8.1f} {pct(v, 0.95):>8.1f} {max(v):>8.1f}")
        print("  * rollback corrections — plus, under the shuttle, its own reversals")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(usage=__doc__)
    parser.add_argument("log")
    parser.add_argument("--by", help="split game-thread crossings by this key's value")
    parser.add_argument("--p95", action="store_true", help="only the p95 of correction_max, corrections, confirm_ms")
    args = parser.parse_args()
    main(args.log, args.by, args.p95)
