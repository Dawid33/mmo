#!/usr/bin/env python3
"""Reduce a `just diag` log to the CPU-bound-or-GPU-bound verdict.

`DIAG=1` makes Bevy's LogDiagnosticsPlugin dump every diagnostic every few seconds, in
hash-map order — dozens of unsorted lines per dump. This takes the LAST dump in the log
(steady state, after the startup burst) and prints the frame budget against the sum of the
per-pass GPU times, sorted by cost.

    scripts/diag_summary.py target/diag/client.log

The verdict line is the point: the GPU's share of frame_time. High => the GPU is the
bottleneck, and the pass table says which pass (with fragment_shader_invocations as the
overdraw tell). Low => the frame is going somewhere on the CPU, and `just trace-chrome`
is the next step.

Caveat: render passes are submitted as one GPU queue, so summing per-pass GPU time is a
good estimate of GPU busy time per frame, not a rigorous one. `just gpu-log` is the
independent check.
"""

import argparse
import re
import sys

# "render/main_opaque_pass_3d/elapsed_gpu  :  16.252818ms (avg 16.214740ms)" — the avg is
# the smoothed value and the one worth reading.
LINE = re.compile(r"(?P<name>[\w/ ]+?)\s*:\s*(?P<val>[\d.]+)(?P<unit>ms|\s)?\s*\(avg (?P<avg>[\d.]+)")
BARE = re.compile(r"(?P<name>[\w/ ]+?)\s*:\s*(?P<val>[\d.]+)\s*$")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("log", help="log from `just diag` (DIAG=1 client run)")
    ap.add_argument("--timeline", action="store_true",
                    help="also print one row per dump (frame time, GPU total, costliest pass): "
                         "how the GPU load moved through the join, which the last-dump table hides")
    args = ap.parse_args()

    # Keep only bevy_diagnostic lines, and split into dumps: a new dump starts whenever a
    # name we've already seen shows up again.
    dumps: list[dict[str, float]] = [{}]
    for raw in open(args.log, errors="replace"):
        if "bevy_diagnostic" not in raw:
            continue
        line = re.sub(r"\x1b\[[0-9;]*m", "", raw).split("bevy_diagnostic")[-1].lstrip(": ").rstrip()
        m = LINE.search(line) or BARE.search(line)
        if not m:
            continue
        name = m.group("name").strip()
        try:
            value = float(m.groupdict().get("avg") or m.group("val"))
        except (TypeError, ValueError):
            continue
        if name in dumps[-1]:
            dumps.append({})
        dumps[-1][name] = value

    last = dumps[-1] if dumps[-1] else (dumps[-2] if len(dumps) > 1 else {})
    if not last:
        print(f"no diagnostics in {args.log} — was the client run with DIAG=1?", file=sys.stderr)
        return 1

    adapter = ""
    for raw in open(args.log, errors="replace"):
        if "AdapterInfo" in raw:
            adapter = raw.split("AdapterInfo")[-1].strip()
            break

    fps = last.get("fps", 0.0)
    frame_time = last.get("frame_time", 0.0)
    gpu = {k[len("render/"):-len("/elapsed_gpu")]: v
           for k, v in last.items() if k.startswith("render/") and k.endswith("/elapsed_gpu")}
    gpu_total = sum(gpu.values())

    print(f"adapter    : {adapter or '(not logged)'}")
    print(f"dumps      : {len(dumps)} (showing the last = steady state)")
    print(f"entities   : {last.get('entity_count', 0):.0f}")
    print()
    print(f"frame_time : {frame_time:8.2f} ms   ({fps:.1f} fps)")
    print(f"gpu total  : {gpu_total:8.2f} ms   ({100 * gpu_total / frame_time if frame_time else 0:.0f}% of the frame)")
    print(f"unaccounted: {frame_time - gpu_total:8.2f} ms   (CPU: sim bridge, meshing, extract, queue, present wait)")
    print()

    share = gpu_total / frame_time if frame_time else 0.0
    # Bevy presents with PresentMode::Fifo (vsync) by default, so a frame_time pinned near
    # a monitor refresh period means the app is *waiting on present*, not working. Calling
    # that "CPU-bound" would be a lie — the unaccounted ms is idle, and the real headroom
    # is unknown until vsync is off.
    vsynced = any(abs(frame_time - 1000.0 / hz) < 0.6 for hz in (60, 75, 90, 120, 144, 165, 240))
    if vsynced and share < 0.7:
        verdict = (f"VSYNC-CAPPED at ~{1000 / frame_time:.0f} Hz — the frame is not full, so most of the\n"
                   "             unaccounted time is idle waiting on present. Neither side is saturated.\n"
                   "             To measure real headroom, uncap: PresentMode::AutoNoVsync in the WindowPlugin.")
    elif share > 0.7:
        verdict = "GPU-BOUND — the GPU is busy for most of the frame; see the pass table below"
    elif share < 0.4:
        verdict = "CPU-BOUND — the GPU idles most of the frame; run `just trace-chrome` for the per-system table"
    else:
        verdict = "MIXED — neither side clearly dominates; check `just gpu-log` for GPU busy %"
    print(f"verdict    : {verdict}")
    print()

    print(f"{'gpu ms':>9}  {'%frame':>6}  {'frag invocations':>17}  pass")
    print("-" * 72)
    for name, ms in sorted(gpu.items(), key=lambda kv: -kv[1]):
        frag = last.get(f"render/{name}/fragment_shader_invocations", 0.0)
        pct = 100 * ms / frame_time if frame_time else 0
        print(f"{ms:9.3f}  {pct:5.1f}%  {frag:17,.0f}  {name}")

    if args.timeline:
        print()
        print("per dump (one every DIAG_INTERVAL, default 2s; values are Bevy's smoothed averages)")
        print(f"{'dump':>5}  {'frame ms':>9}  {'fps':>6}  {'gpu ms':>8}  {'gpu %':>6}  {'entities':>9}  costliest pass")
        print("-" * 84)
        for i, dump in enumerate(d for d in dumps if d):
            passes = {k[len("render/"):-len("/elapsed_gpu")]: v
                      for k, v in dump.items() if k.startswith("render/") and k.endswith("/elapsed_gpu")}
            total = sum(passes.values())
            ft = dump.get("frame_time", 0.0)
            top = max(passes.items(), key=lambda kv: kv[1], default=("-", 0.0))
            print(f"{i:5d}  {ft:9.2f}  {dump.get('fps', 0.0):6.1f}  {total:8.2f}  "
                  f"{100 * total / ft if ft else 0:5.0f}%  {dump.get('entity_count', 0):9.0f}  "
                  f"{top[0]} ({top[1]:.2f} ms)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
