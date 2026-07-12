#!/usr/bin/env python3
"""Reduce a Bevy Chrome-Trace capture to a pasteable per-span table.

`just trace-chrome` runs the client with bevy's `trace_chrome` feature, which writes a
Chrome Trace Event JSON file — one B/E span pair per ECS system run, per extract step and
per render-graph node. That file is tens to hundreds of MB, so it is useless to read
directly; this collapses it into "where did main-thread frame time actually go".

    scripts/trace_summary.py target/trace/client.json [--top N] [--min-ms N]

Columns are per-span totals over the whole capture: total wall time, how many times the
span ran, and mean per call. Sorted by total, because a cheap system running every frame
outranks an expensive one that ran twice at startup — and the first is what costs FPS.

The `frames` count (spans named "frame" / "main app") lets you divide totals into
per-frame costs; the script prints ms/frame directly when it can find a frame span.

Notes on reading the output:
  * Bevy runs systems in parallel across the task pool, so summed span time can exceed
    wall time. Compare spans against each other, not against the frame budget.
  * Startup-only work (asset loading, pipeline compilation) inflates totals; use --skip-ms
    to drop the first N ms of the capture if the startup burst dominates.
"""

import argparse
import collections
import json
import sys


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("trace", help="Chrome Trace Event JSON written by bevy's trace_chrome")
    ap.add_argument("--top", type=int, default=40, help="rows to print (default 40)")
    ap.add_argument("--min-ms", type=float, default=0.0, help="drop spans totalling less than this")
    ap.add_argument("--skip-ms", type=float, default=0.0,
                    help="ignore spans starting in the first N ms (skips the startup burst)")
    args = ap.parse_args()

    with open(args.trace) as f:
        events = json.load(f)

    # tracing-chrome emits duration events as separate begin ('B') / end ('E') records,
    # matched per (name, thread). Timestamps are microseconds.
    starts = min((e["ts"] for e in events if "ts" in e), default=0)
    cutoff = starts + args.skip_ms * 1000.0

    open_spans: dict[tuple, list[float]] = collections.defaultdict(list)
    total_us: collections.Counter = collections.Counter()
    calls: collections.Counter = collections.Counter()

    for e in events:
        phase = e.get("ph")
        if phase not in ("B", "E"):
            continue
        key = (e.get("name"), e.get("tid"))
        if phase == "B":
            open_spans[key].append(e["ts"])
        elif open_spans[key]:
            begin = open_spans[key].pop()
            if begin < cutoff:
                continue
            name = e.get("name")
            total_us[name] += e["ts"] - begin
            calls[name] += 1

    if not total_us:
        print(f"no complete spans in {args.trace} — was the capture truncated by a kill?", file=sys.stderr)
        return 1

    # Bevy names the whole-frame span "frame"; fall back to the most-run span, which in a
    # render loop is per-frame by definition.
    frames = calls.get("frame") or max(calls.values())

    print(f"{args.trace}: {len(events)} events, ~{frames} frames"
          f"{f', first {args.skip_ms:.0f}ms skipped' if args.skip_ms else ''}\n")
    print(f"{'total ms':>10} {'ms/frame':>9} {'calls':>8} {'us/call':>9}  span")
    print("-" * 78)
    for name, us in total_us.most_common():
        ms = us / 1000.0
        if ms < args.min_ms:
            continue
        print(f"{ms:10.1f} {ms / frames:9.3f} {calls[name]:8d} {us / calls[name]:9.1f}  {name}")
        args.top -= 1
        if args.top == 0:
            break
    return 0


if __name__ == "__main__":
    sys.exit(main())
