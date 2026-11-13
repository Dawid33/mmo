#!/usr/bin/env python3
"""Reduce a crossing probe's client log (`just fly-probe` / `just shuttle-probe`)
to one table: per metric, the distribution over every crossing.

Reads the `[perf] event=region_crossing` (game thread) and
`region_crossing_render` (render; only its `after:` half) lines, `main_stall`,
the autopilot's own events, and — when the client ran with PERF_LOG=1 — the
`[client-motion]` chunk-crossing lines.

Usage: crossing_summary.py [--by KEY] [--p95] <client.log>
       crossing_summary.py --go-nogo --thread-log <client.log> --host-log <client.log>
                           [--server-log <host-n.log>]... [--min-crossings N]

  --by KEY  also split the game-thread crossings by the value of KEY on their line
            (e.g. `--by host_flip`: crossings between two hosts' regions vs within one).
  --p95     per group (or over all crossings): the p95 of correction_max, corrections
            and confirm_ms only.
  --go-nogo the R3 go / no-go (`just go-nogo-r3`, the plan's decision 13): both client
            logs' p95 tables by host_flip, then one verdict line, `VERDICT: GO`,
            `VERDICT: NO-GO` or `VERDICT: INVALID` (see `verdict`).
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


# --- the R3 go / no-go (plan decision 13) -------------------------------------------

# A host flip's p95 may be at most this times the thread flip's p95 ...
GO_NOGO_RATIO = 1.25
# ... plus this much (units of the metric), per judged metric.
GO_NOGO_SLACK = 0.05
# The judged metrics (confirm_ms is shown beside them, not judged).
GO_NOGO_KEYS = ["correction_max", "corrections"]
# Fewer crossings than this in either group: too few to judge a p95 (INVALID).
MIN_CROSSINGS = 20


def clean(lines):
    return [ANSI.sub("", raw) for raw in lines]


def conservation_problems(client_logs, server_logs):
    """What breaks conservation in a run, as text (empty: it held). `client_logs` and
    `server_logs` map a name to the log's lines. Client: a `[client-state-hash]` line with
    `mismatched` > 0. Hosts: a `[server] window=` line with `items_destroyed` or
    `intent_replies_lost` > 0, or a host whose LAST such line has `peer_unreceived` > 0."""
    problems = []
    for name, lines in client_logs.items():
        for line in clean(lines):
            if "[client-state-hash]" in line:
                kv = dict(KV.findall(line))
                if float(kv.get("mismatched", 0)) > 0:
                    problems.append(f"{name}: state-hash mismatch: {line.strip()[:160]}")
    for name, lines in server_logs.items():
        last = None
        for line in clean(lines):
            if "[server] window=" not in line:
                continue
            kv = dict(KV.findall(line))
            for key in ("items_destroyed", "intent_replies_lost"):
                if float(kv.get(key, 0)) > 0:
                    problems.append(f"{name}: {key}={kv[key]} on a [server] line")
            last = kv
        if last is not None and float(last.get("peer_unreceived", 0)) > 0:
            problems.append(f"{name}: peer_unreceived={last['peer_unreceived']} at the end")
    return problems


def invalid_reasons(client_logs, server_logs):
    """What makes a run unfit to judge (empty: fit): a host that started more than once
    (a restart rolls the world back), no `[server] window=` counters line at all, a client
    log with no `[client-state-hash]` line (nothing checked), a client that panicked or
    lost its GPU."""
    reasons = []
    for name, lines in server_logs.items():
        starts = sum(1 for line in clean(lines) if "[server] role:" in line)
        if starts > 1:
            reasons.append(f"{name}: the host started {starts} times (restarted during the run)")
    if not any("[server] window=" in line for lines in server_logs.values() for line in clean(lines)):
        reasons.append("no [server] counters line in any host log: conservation unchecked")
    for name, lines in client_logs.items():
        text = clean(lines)
        if not any("[client-state-hash]" in line for line in text):
            reasons.append(f"{name}: no [client-state-hash] line: the state hash was never checked")
        fault = next((line for line in text if "DeviceLost" in line or "panicked at" in line), None)
        if fault:
            reasons.append(f"{name}: run aborted: {fault.strip()[:160]}")
    return reasons


def p95_of(crossings, metric):
    values = [c[metric] for c in crossings if metric in c and c[metric] >= 0]
    return pct(values, 0.95) if values else 0.0


def verdict(thread_crossings, host_crossings, conservation, invalid, min_crossings=MIN_CROSSINGS):
    """Decision 13 over one run: `(word, reasons, rows)`. The thread group is the thread
    run's crossings with host_flip=0, the host group the host run's with host_flip=1.
    In order: any `invalid` reason -> INVALID; any `conservation` problem -> NO-GO,
    whatever the timing; fewer than `min_crossings` in a group -> INVALID; then GO iff for
    every GO_NOGO_KEYS metric host p95 <= GO_NOGO_RATIO * thread p95 + GO_NOGO_SLACK.
    `rows`: (metric, thread p95, host p95, limit or None, ok or None)."""
    thread = [c for c in thread_crossings if c.get("host_flip") == 0]
    host = [c for c in host_crossings if c.get("host_flip") == 1]
    rows = []
    for metric in GO_NOGO_KEYS:
        t, h = p95_of(thread, metric), p95_of(host, metric)
        limit = GO_NOGO_RATIO * t + GO_NOGO_SLACK
        rows.append((metric, t, h, limit, h <= limit))
    rows.append(("confirm_ms", p95_of(thread, "confirm_ms"), p95_of(host, "confirm_ms"), None, None))
    if invalid:
        return "INVALID", list(invalid), rows
    if conservation:
        return "NO-GO", ["conservation: " + p for p in conservation], rows
    few = [
        f"{name}: {len(group)} crossings (< {min_crossings})"
        for name, group in (("thread seam (host_flip=0)", thread), ("host seam (host_flip=1)", host))
        if len(group) < min_crossings
    ]
    if few:
        return "INVALID", few, rows
    misses = [
        f"{m}: host p95 {h:.2f} > {GO_NOGO_RATIO} x thread p95 {t:.2f} + {GO_NOGO_SLACK} = {limit:.2f}"
        for m, t, h, limit, ok in rows
        if ok is False
    ]
    if misses:
        return "NO-GO", misses, rows
    return "GO", [f"thread n={len(thread)}, host n={len(host)}"], rows


def go_nogo(thread_log, host_log, server_logs, min_crossings=MIN_CROSSINGS):
    """Prints both runs' p95 tables by host_flip, the facts and the verdict; returns the word."""
    read = lambda path: open(path, errors="replace").readlines()  # noqa: E731
    clients = {"thread run": read(thread_log), "host run": read(host_log)}
    servers = {path: read(path) for path in server_logs}
    for name, path in (("thread run", thread_log), ("host run", host_log)):
        print(f"\n== {name}: {path}")
        main(path, "host_flip", True)
    for path, lines in servers.items():
        starts = sum(1 for line in clean(lines) if "[server] role:" in line)
        print(f"host log {path}: started {starts} time(s){' (RESTARTED)' if starts > 1 else ''}")
    word, reasons, rows = verdict(
        crossing_lines(clients["thread run"]),
        crossing_lines(clients["host run"]),
        conservation_problems(clients, servers),
        invalid_reasons(clients, servers),
        min_crossings,
    )
    print(f"\ngo / no-go (decision 13: host p95 <= {GO_NOGO_RATIO} x thread p95 + {GO_NOGO_SLACK}):")
    print(f"  {'metric':<16} {'thread p95':>10} {'host p95':>10} {'limit':>10}  ok")
    for metric, t, h, limit, ok in rows:
        if limit is None:
            print(f"  {metric:<16} {t:>10.2f} {h:>10.2f} {'(shown)':>10}  -")
        else:
            print(f"  {metric:<16} {t:>10.2f} {h:>10.2f} {limit:>10.2f}  {'yes' if ok else 'NO'}")
    for reason in reasons:
        print(f"  {reason}")
    print(f"VERDICT: {word}")
    return word


if __name__ == "__main__":
    parser = argparse.ArgumentParser(usage=__doc__)
    parser.add_argument("log", nargs="?")
    parser.add_argument("--by", help="split game-thread crossings by this key's value")
    parser.add_argument("--p95", action="store_true", help="only the p95 of correction_max, corrections, confirm_ms")
    parser.add_argument("--go-nogo", action="store_true", help="the R3 go / no-go verdict over two client logs")
    parser.add_argument("--thread-log", help="--go-nogo: the client log of the thread-seam shuttle")
    parser.add_argument("--host-log", help="--go-nogo: the client log of the host-seam shuttle")
    parser.add_argument("--server-log", action="append", default=[], help="--go-nogo: a host's log (repeat)")
    parser.add_argument("--min-crossings", type=int, default=MIN_CROSSINGS)
    args = parser.parse_args()
    if args.go_nogo:
        if not (args.thread_log and args.host_log):
            parser.error("--go-nogo needs --thread-log and --host-log")
        go_nogo(args.thread_log, args.host_log, args.server_log, args.min_crossings)
    else:
        if not args.log:
            parser.error("a client log is needed")
        main(args.log, args.by, args.p95)
