#!/usr/bin/env python3
"""Reduce a crossing probe's client log (`just fly-probe` / `just shuttle-probe`)
to one table: per metric, the distribution over every crossing.

Reads the `[perf] event=region_crossing` (game thread) and
`region_crossing_render` (render; only its `after:` half) lines, `main_stall`,
the autopilot's own events, and — when the client ran with PERF_LOG=1 — the
`[client-motion]` chunk-crossing lines.

Usage: crossing_summary.py [--by KEY] [--p95] <client.log>
       crossing_summary.py --go-nogo --thread-log <client.log> --host-log <client.log>
                           [--server-log <host-n.log>]... [--aux-log <coordinator/login.log>]...
                           [--stack-log <stack.log> [--stack-stop-line N]] [--stack-alive 0|1]
                           [--stack-exit N]
                           [--min-crossings N]

  --by KEY  also split the game-thread crossings by the value of KEY on their line
            (e.g. `--by host_flip`: crossings between two hosts' regions vs within one).
  --p95     per group (or over all crossings): the p95 of correction_max, corrections
            and confirm_ms only.
  --go-nogo the R3 go / no-go (`just go-nogo-r3`, the plan's decision 13): both client
            logs' p95 tables by host_flip, then one verdict line, `VERDICT: GO`,
            `VERDICT: NO-GO` or `VERDICT: INVALID` (see `verdict`, `invalid_reasons`);
            exit 0 GO, 1 NO-GO, 2 INVALID, 3 no verdict (a usage error or a failure).
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
    """Nearest rank: the sorted value at index round(q * (n - 1)) — with n = 20 the
    p95 is the second-largest value, with n <= 11 it is the largest."""
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
# The script's exit code per verdict (3: no verdict, the script itself failed).
EXIT_CODES = {"GO": 0, "NO-GO": 1, "INVALID": 2}
EXIT_NO_VERDICT = 3
# What `scripts/dev_r3.sh` says when a member of the stack ended on its own (or a host
# restarted: "exited 5 ... starting it again"), or a stop needed a KILL or could not
# confirm a member gone.
STACK_FAILURES = ("exited", "giving up", "stopped (failed)", "ended (", "killing ", "could not confirm",
                  "still running")
# After the recipe's own stop: what a clean stop never says (a host that could not save its
# leave cut, e.g. one that hung, is KILLed at the deadline).
STACK_STOP_FAILURES = ("killing ", "could not confirm", "still running", "stopped (failed)")
# What a host or the coordinator logs when a host's land is taken over (a run with one is INVALID).
SUCCESSION_MARKS = ("[server] succession ", "declaring them dead")
# A host's last `[server] window=` line may be at most this many seconds older than the
# end of the host-seam shuttle (`autopilot_done`): 3 report windows (5 s each).
HOST_SILENT_LIMIT_S = 15
# Without a common clock: a host with fewer window lines than this share of its busiest
# peer's stopped reporting (hung) during the run.
HOST_WINDOW_SHARE_MIN = 0.5
# simplelog's default stamp on a server line (UTC, `HH:MM:SS`), and Bevy's (RFC 3339 UTC).
SERVER_CLOCK = re.compile(r"^(\d{2}):(\d{2}):(\d{2}) ")
CLIENT_CLOCK = re.compile(r"\d{4}-\d{2}-\d{2}T(\d{2}):(\d{2}):(\d{2})(?:\.\d+)?Z")
# What a client says when its session ended or a host refused it.
CLIENT_SESSION_LOST = ("[session] session ended", "refused the connection")


def clean(lines):
    return [ANSI.sub("", raw) for raw in lines]


def server_lines(lines):
    """The key=value pairs of every `[server] window=` counters line."""
    return [dict(KV.findall(line)) for line in clean(lines) if "[server] window=" in line]


def conservation_problems(client_logs, server_logs):
    """What breaks conservation in a run, as text (empty: it held). `client_logs` and
    `server_logs` (the hosts) map a name to the log's lines. Client: a
    `[client-state-hash]` line with `mismatched` > 0. Hosts: a `[server] window=` line with
    `items_destroyed` or `intent_replies_lost` > 0, or a host whose LAST TWO such lines
    both have `peer_unreceived` > 0 (one sample also counts frames still in flight, e.g. a
    cut's mark; a host with a single line is judged on it)."""
    problems = []
    for name, lines in client_logs.items():
        for line in clean(lines):
            if "[client-state-hash]" in line:
                kv = dict(KV.findall(line))
                if float(kv.get("mismatched", 0)) > 0:
                    problems.append(f"{name}: state-hash mismatch: {line.strip()[:160]}")
    for name, lines in server_logs.items():
        windows = server_lines(lines)
        for kv in windows:
            for key in ("items_destroyed", "intent_replies_lost"):
                if float(kv.get(key, 0)) > 0:
                    problems.append(f"{name}: {key}={kv[key]} on a [server] line")
        last = windows[-2:]
        if last and all(float(kv.get("peer_unreceived", 0)) > 0 for kv in last):
            values = ", ".join(kv["peer_unreceived"] for kv in last)
            problems.append(f"{name}: peer_unreceived={values} on its last [server] line(s)")
    return problems


def clock_s(pattern, line):
    """The second of the (UTC) day a log line was written at, or None."""
    m = pattern.search(line)
    return None if m is None else int(m.group(1)) * 3600 + int(m.group(2)) * 60 + int(m.group(3))


def seconds_before(earlier, later):
    """How many seconds `earlier` lies before `later` (seconds of day; a day wrap between
    them is taken as the shorter way round)."""
    return (later - earlier + 43200) % 86400 - 43200


def silent_hosts(client_logs, server_logs):
    """Hosts that stopped reporting (`[server] window=` lines) before the end of the run —
    hung, not dead: `(method, reasons)`. With a common clock (the server's simplelog
    `HH:MM:SS` and the client's RFC 3339, both UTC): every host's last window line must be at
    most HOST_SILENT_LIMIT_S before the last `autopilot_done` of the host run (else of any
    client). Without one: every host needs at least HOST_WINDOW_SHARE_MIN of its busiest
    peer's count of window lines."""
    reasons = []
    ends = []
    for name in ("host run", *[n for n in client_logs if n != "host run"]):
        for line in clean(client_logs.get(name, [])):
            if "event=autopilot_done" in line:
                at = clock_s(CLIENT_CLOCK, line)
                if at is not None:
                    ends.append(at)
        if ends:
            break
    lasts = {}
    for name, lines in server_logs.items():
        stamps = [clock_s(SERVER_CLOCK, line) for line in clean(lines) if "[server] window=" in line]
        lasts[name] = stamps[-1] if stamps else None
    if ends and lasts and all(at is not None for at in lasts.values()):
        end = ends[-1]
        for name, last in lasts.items():
            gap = seconds_before(last, end)
            if gap > HOST_SILENT_LIMIT_S:
                reasons.append(f"{name}: its last [server] line is {gap} s before the shuttle ended "
                               f"(> {HOST_SILENT_LIMIT_S} s): the host stopped reporting (hung?)")
        return "clock", reasons
    counts = {name: len(server_lines(lines)) for name, lines in server_logs.items()}
    busiest = max(counts.values(), default=0)
    for name, n in counts.items():
        if n and n < HOST_WINDOW_SHARE_MIN * busiest:
            reasons.append(f"{name}: {n} [server] lines against its peer's {busiest}: the host stopped "
                           "reporting (hung?)")
    return "counts", reasons


def invalid_reasons(client_logs, server_logs, aux_logs=None, stack_lines=None, stack_alive=True, stack_exit=0,
                    stack_after=None):
    """What makes a run unfit to judge (empty: fit).
    - `server_logs` (each host): started more than once (a restart rolls the world back),
      no `[server] window=` counters line in that host's log, `panicked at`;
    - `aux_logs` (the coordinator, the login service): `panicked at`;
    - `stack_lines` (the supervisor's output up to the recipe's own stop): a member that
      exited, a restart that gave up, a KILL, a failed stop (`STACK_FAILURES`); `stack_alive`
      False: the supervisor was gone before the recipe stopped it (the stack fell over);
    - `stack_exit` (the supervisor's status after the recipe's stop; None: unknown) not 0,
      or `stack_after` (its output after the stop) with a KILL or an unconfirmed member
      (`STACK_STOP_FAILURES`): the stack did not stop cleanly — a host may have hung;
    - a host that stopped reporting before the end (`silent_hosts`);
    - each client: no `[client-state-hash]` line (nothing checked), a panic or a lost
      GPU, no `autopilot_done` (the shuttle did not run its time), or its session ended
      or a host refused it before `autopilot_done`."""
    reasons = []
    for name, lines in server_logs.items():
        text = clean(lines)
        starts = sum(1 for line in text if "[server] role:" in line)
        if starts > 1:
            reasons.append(f"{name}: the host started {starts} times (restarted during the run)")
        if not any("[server] window=" in line for line in text):
            reasons.append(f"{name}: no [server] counters line: conservation unchecked on that host")
    for name, lines in {**server_logs, **(aux_logs or {})}.items():
        # A takeover moves the land, so the seam the autopilot picked is not between the
        # hosts it assumed: every succession line a host prints starts `[server] succession `
        # (world_manager/succession.rs), the coordinator's declaration says `declaring them dead`
        # (coordinator/succession.rs).
        taken = next((line for line in clean(lines) if any(w in line for w in SUCCESSION_MARKS)), None)
        if taken:
            reasons.append(f"{name}: a succession happened (the land moved during the run): {taken.strip()[:160]}")
    for name, lines in {**server_logs, **(aux_logs or {})}.items():
        panic = next((line for line in clean(lines) if "panicked at" in line), None)
        if panic:
            reasons.append(f"{name}: panicked: {panic.strip()[:160]}")
    for line in clean(stack_lines or []):
        if any(word in line for word in STACK_FAILURES):
            reasons.append(f"the stack, before the recipe stopped it: {line.strip()[:160]}")
    if not stack_alive:
        reasons.append("the stack's supervisor had ended before the recipe stopped it (a member fell over)")
    if stack_exit not in (None, 0):
        reasons.append(f"the stack did not stop cleanly (supervisor exit {stack_exit}): a host may have hung")
    for line in clean(stack_after or []):
        if any(word in line for word in STACK_STOP_FAILURES):
            reasons.append(f"the stack did not stop cleanly: {line.strip()[:160]}")
    reasons.extend(silent_hosts(client_logs, server_logs)[1])
    for name, lines in client_logs.items():
        text = clean(lines)
        if not any("[client-state-hash]" in line for line in text):
            reasons.append(f"{name}: no [client-state-hash] line: the state hash was never checked")
        fault = next((line for line in text if "DeviceLost" in line or "panicked at" in line), None)
        if fault:
            reasons.append(f"{name}: run aborted: {fault.strip()[:160]}")
        done = next((i for i, line in enumerate(text) if "event=autopilot_done" in line), None)
        if done is None:
            reasons.append(f"{name}: no autopilot_done: the shuttle did not run its time")
        lost = next((line for line in text[: done if done is not None else len(text)]
                     if any(word in line for word in CLIENT_SESSION_LOST)), None)
        if lost:
            reasons.append(f"{name}: the session ended early: {lost.strip()[:160]}")
    return reasons


def p95_of(crossings, metric):
    values = [c[metric] for c in crossings if metric in c and c[metric] >= 0]
    return pct(values, 0.95) if values else 0.0


def side_known(c):
    return c.get("from_host", -1) >= 0 and c.get("to_host", -1) >= 0


def seam_groups(thread_crossings, host_crossings):
    """The thread group: the thread run's crossings whose two sides are known (`from_host`,
    `to_host` >= 0) and the same host; the host group: the host run's crossings whose sides
    are known and differ. `unknown`: how many crossings of both runs had a side unknown
    (-1, or missing) — reported, never grouped (the client's `host_flip` counts those as
    flips)."""
    thread = [c for c in thread_crossings if side_known(c) and c["from_host"] == c["to_host"]]
    host = [c for c in host_crossings if side_known(c) and c["from_host"] != c["to_host"]]
    unknown = sum(1 for c in thread_crossings + host_crossings if not side_known(c))
    return {"thread": thread, "host": host, "unknown": unknown}


def verdict(thread_crossings, host_crossings, conservation, invalid, min_crossings=MIN_CROSSINGS):
    """Decision 13 over one run: `(word, reasons, rows)`. Groups: `seam_groups`.
    In order: any `invalid` reason -> INVALID; any `conservation` problem -> NO-GO,
    whatever the timing; fewer than `min_crossings` in a group -> INVALID; then GO iff for
    every GO_NOGO_KEYS metric host p95 <= GO_NOGO_RATIO * thread p95 + GO_NOGO_SLACK.
    `rows`: (metric, thread p95, host p95, limit or None, ok or None)."""
    groups = seam_groups(thread_crossings, host_crossings)
    thread, host = groups["thread"], groups["host"]
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
        for name, group in (("thread seam (one host)", thread), ("host seam (two hosts)", host))
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


def go_nogo(thread_log, host_log, server_logs, aux_logs=(), stack_log=None, stack_stop_line=None,
            stack_alive=True, min_crossings=MIN_CROSSINGS, stack_exit=None):
    """Prints both runs' p95 tables by host_flip, the facts and the verdict; returns the
    word. `stack_stop_line`: how many lines of `stack_log` were written before the
    recipe's own stop (None: all of them)."""
    read = lambda path: open(path, errors="replace").readlines()  # noqa: E731
    clients = {"thread run": read(thread_log), "host run": read(host_log)}
    servers = {path: read(path) for path in server_logs}
    aux = {path: read(path) for path in aux_logs}
    stack = read(stack_log) if stack_log else []
    stack_after = []
    if stack_stop_line is not None:
        stack, stack_after = stack[:stack_stop_line], stack[stack_stop_line:]
    for name, path in (("thread run", thread_log), ("host run", host_log)):
        print(f"\n== {name}: {path}")
        main(path, "host_flip", True)
    for path, lines in servers.items():
        starts = sum(1 for line in clean(lines) if "[server] role:" in line)
        print(f"host log {path}: started {starts} time(s){' (RESTARTED)' if starts > 1 else ''}")
    thread_c, host_c = crossing_lines(clients["thread run"]), crossing_lines(clients["host run"])
    groups = seam_groups(thread_c, host_c)
    print(
        f"seam groups: thread (one known host) n={len(groups['thread'])}, host (two known hosts) "
        f"n={len(groups['host'])}, a side unknown (never grouped) n={groups['unknown']}"
    )
    method, _ = silent_hosts(clients, servers)
    print(f"host liveness checked by: {'the common UTC clock' if method == 'clock' else 'window-line counts (no clock)'}")
    word, reasons, rows = verdict(
        thread_c,
        host_c,
        conservation_problems(clients, servers),
        invalid_reasons(clients, servers, aux, stack, stack_alive, stack_exit, stack_after),
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
    import sys
    import traceback

    parser = argparse.ArgumentParser(usage=__doc__)
    parser.add_argument("log", nargs="?")
    parser.add_argument("--by", help="split game-thread crossings by this key's value")
    parser.add_argument("--p95", action="store_true", help="only the p95 of correction_max, corrections, confirm_ms")
    parser.add_argument("--go-nogo", action="store_true", help="the R3 go / no-go verdict over two client logs")
    parser.add_argument("--thread-log", help="--go-nogo: the client log of the thread-seam shuttle")
    parser.add_argument("--host-log", help="--go-nogo: the client log of the host-seam shuttle")
    parser.add_argument("--server-log", action="append", default=[], help="--go-nogo: a host's log (repeat)")
    parser.add_argument("--aux-log", action="append", default=[],
                        help="--go-nogo: the coordinator's or login service's log (repeat; panics only)")
    parser.add_argument("--stack-log", help="--go-nogo: the dev_r3.sh supervisor's output")
    parser.add_argument("--stack-stop-line", type=int, help="--go-nogo: stack-log lines written before the stop")
    parser.add_argument("--stack-alive", type=int, choices=[0, 1], default=1,
                        help="--go-nogo: 1 if the supervisor still ran when the recipe stopped it")
    parser.add_argument("--stack-exit", type=int,
                        help="--go-nogo: the supervisor's exit status after the recipe's stop (not 0: INVALID)")
    parser.add_argument("--min-crossings", type=int, default=MIN_CROSSINGS)
    try:
        args = parser.parse_args()
    except SystemExit as e:
        # Usage errors are no verdict (argparse's own 2 would read as INVALID).
        sys.exit(0 if e.code == 0 else EXIT_NO_VERDICT)
    if args.go_nogo:
        if not (args.thread_log and args.host_log):
            print("--go-nogo needs --thread-log and --host-log", file=sys.stderr)
            sys.exit(EXIT_NO_VERDICT)
        try:
            word = go_nogo(args.thread_log, args.host_log, args.server_log, args.aux_log, args.stack_log,
                           args.stack_stop_line, bool(args.stack_alive), args.min_crossings, args.stack_exit)
        except Exception:  # noqa: BLE001 — any failure is "no verdict", never a verdict's code
            traceback.print_exc()
            sys.exit(EXIT_NO_VERDICT)
        sys.exit(EXIT_CODES[word])
    else:
        if not args.log:
            parser.error("a client log is needed")
        main(args.log, args.by, args.p95)
