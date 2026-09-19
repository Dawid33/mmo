#!/usr/bin/env python3
"""Reduce a Bevy Chrome-Trace capture to pasteable per-span tables.

`just trace-chrome` / `just trace-join` run the client with bevy's `trace_chrome` feature,
which writes a Chrome Trace Event JSON file — one B/E span pair per ECS system run, per
extract step and per render-graph node (plus whatever first-party or vendored crates emit:
the rapier fork traces its solver internals, which is most of the volume). The file is
hundreds of MB to several GB, so it is useless to read directly and must never be loaded
whole; this STREAMS it (one event per line, constant memory) into "where did the time go".

    scripts/trace_summary.py target/trace/client.json [--top N] [--min-ms N]
    scripts/trace_summary.py target/trace/client.json --until-ms 30000   # join phase only
    scripts/trace_summary.py target/trace/client.json --thread main      # one thread only

Two tables. Per thread: busy time (top-level spans) — which thread is the bottleneck.
Per span, sorted by SELF time: time inside the span minus its child spans on the same
thread. Inclusive totals are shown too, but they double count (a schedule span contains
every system it runs), so self time is the column that answers "who owns the frame".

Notes on reading the output:
  * Bevy runs systems in parallel across the task pool, so summed span time can exceed
    wall time. Compare within one thread (`--thread`), or spans against each other.
  * Startup-only work (asset loading, pipeline compilation) inflates totals; use --skip-ms
    to drop the first N ms of the capture if the startup burst dominates.
  * --until-ms is the opposite cut: keep ONLY spans starting in the first N ms, which is
    the load/join phase — and reading stops there, so a multi-GB trace costs only its
    first part. There the `max ms` column matters more than the mean: one multi-second
    call of a bridge system is a main-thread stall, not a per-frame cost.
  * --skip-cat drops whole categories before any accounting (substring of the event's
    `cat`, repeatable) — e.g. `--skip-cat rapier3d --skip-cat parry3d` when only the
    render side matters; it also makes a big trace read several times faster.
"""

import argparse
import collections
import json
import sys
import time

# Events are written roughly in time order, but each thread buffers a little; keep reading
# this long past --until-ms before concluding nothing earlier is still to come.
UNTIL_SLACK_US = 5_000_000.0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("trace", help="Chrome Trace Event JSON written by bevy's trace_chrome")
    ap.add_argument("--top", type=int, default=40, help="rows to print (default 40)")
    ap.add_argument("--min-ms", type=float, default=0.0, help="drop spans with less self time than this")
    ap.add_argument("--skip-ms", type=float, default=0.0,
                    help="ignore spans starting in the first N ms (skips the startup burst)")
    ap.add_argument("--until-ms", type=float, default=0.0,
                    help="ignore spans starting after the first N ms (summarises only the load/join phase)")
    ap.add_argument("--thread", default="",
                    help="only spans on threads whose name contains this (see the thread table)")
    ap.add_argument("--skip-cat", action="append", default=[],
                    help="drop events whose category contains this (repeatable)")
    args = ap.parse_args()
    skip_cats = [f'"cat":"{c}' for c in args.skip_cat]

    thread_names: dict[int, str] = {}
    # Per thread: stack of [name, begin_ts, child_us]; spans nest strictly within a thread.
    stacks: dict[int, list[list]] = collections.defaultdict(list)
    total_us: collections.Counter = collections.Counter()
    self_us: collections.Counter = collections.Counter()
    calls: collections.Counter = collections.Counter()
    max_us: dict[str, float] = collections.defaultdict(float)
    thread_busy_us: collections.Counter = collections.Counter()

    origin = None
    cutoff = until = None
    last_ts = 0.0
    events = 0
    started = time.monotonic()
    stopped_early = False

    with open(args.trace, errors="replace") as f:
        for line in f:
            # Cheap pre-filters before paying for json.loads: only B/E (and the rare
            # thread_name M) events matter, and skipped categories never get parsed.
            if '"ph":"B"' not in line and '"ph":"E"' not in line and '"thread_name"' not in line:
                continue
            if skip_cats and any(c in line for c in skip_cats):
                continue
            line = line.strip().rstrip(",")
            try:
                e = json.loads(line)
            except ValueError:
                continue  # a truncated last line after a kill
            events += 1
            phase = e.get("ph")
            tid = e.get("tid", 0)
            if phase == "M":
                if e.get("name") == "thread_name":
                    thread_names[tid] = e.get("args", {}).get("name", str(tid))
                continue
            ts = e.get("ts")
            if ts is None:
                continue
            if origin is None:
                origin = ts
                cutoff = origin + args.skip_ms * 1000.0
                until = origin + args.until_ms * 1000.0 if args.until_ms else float("inf")
            last_ts = ts
            if ts > until + UNTIL_SLACK_US:
                stopped_early = True
                break
            if events % 2_000_000 == 0:
                print(f"  … {events / 1e6:.0f}M events, t={((ts - origin) / 1e6):.0f}s, "
                      f"{time.monotonic() - started:.0f}s elapsed", file=sys.stderr)

            stack = stacks[tid]
            if phase == "B":
                stack.append([e.get("name"), ts, 0.0])
                continue
            # phase == "E": close the matching span (normally the top of the stack).
            name = e.get("name")
            index = len(stack) - 1
            while index >= 0 and stack[index][0] != name:
                index -= 1
            if index < 0:
                continue
            _, begin, child = stack[index]
            del stack[index:]
            duration = ts - begin
            if stack:
                stack[-1][2] += duration
            if begin < cutoff or begin > until:
                continue
            if args.thread and args.thread not in thread_names.get(tid, str(tid)):
                continue
            total_us[name] += duration
            self_us[name] += max(0.0, duration - child)
            calls[name] += 1
            if duration > max_us[name]:
                max_us[name] = duration
            if not stack:
                thread_busy_us[tid] += duration

    if not total_us:
        print(f"no complete spans in {args.trace} — was the capture truncated by a kill?", file=sys.stderr)
        return 1

    # Bevy names the whole-frame span "frame"; fall back to the most-run span, which in a
    # render loop is per-frame by definition.
    frames = calls.get("frame") or max(calls.values())
    window_s = (min(last_ts, until) - max(origin, cutoff)) / 1e6

    print(f"{args.trace}: {events} span events read, ~{frames} frames, window {window_s:.1f}s"
          f"{f', first {args.skip_ms:.0f}ms skipped' if args.skip_ms else ''}"
          f"{f', only the first {args.until_ms:.0f}ms kept' if args.until_ms else ''}"
          f"{' (stopped reading there)' if stopped_early else ''}"
          f"{f', thread~{args.thread!r}' if args.thread else ''}"
          f"{f', categories skipped: {args.skip_cat}' if args.skip_cat else ''}\n")

    print(f"{'busy ms':>10} {'% window':>9}  thread (top-level span time; 100% = never idle)")
    print("-" * 60)
    for tid, us in thread_busy_us.most_common(12):
        print(f"{us / 1000.0:10.0f} {100.0 * us / 1e6 / window_s if window_s > 0 else 0:8.1f}%  "
              f"{thread_names.get(tid, f'tid {tid}')}")

    print(f"\n{'self ms':>10} {'self/frm':>9} {'total ms':>10} {'calls':>8} {'us/call':>9} {'max ms':>9}  span")
    print("-" * 100)
    rows = args.top
    for name, us in self_us.most_common():
        ms = us / 1000.0
        if ms < args.min_ms:
            break
        print(f"{ms:10.1f} {ms / frames:9.3f} {total_us[name] / 1000.0:10.1f} {calls[name]:8d} "
              f"{total_us[name] / calls[name]:9.1f} {max_us[name] / 1000.0:9.2f}  {name}")
        rows -= 1
        if rows == 0:
            break
    return 0


if __name__ == "__main__":
    sys.exit(main())
