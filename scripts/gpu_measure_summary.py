#!/usr/bin/env python3
"""Reduce a Mesa INTEL_MEASURE capture (anv Vulkan, `renderpass` granularity) to
the GPU time per frame and per render pass, averaged over the last N frames.

    scripts/gpu_measure_summary.py <measure.csv> [frames=150]

Render passes are numbered in submission order within a frame (`rp#0` …), each
with its three costliest events (type + shader hashes; hashes change with the
build, so compare passes by position and draw type, not by hash). In this game
the four sun shadow cascades are the `draw indexed indirect count` passes before
the main opaque pass. Written for `just gpu-measure`.
"""
import csv, sys, collections
path = sys.argv[1]; last = int(sys.argv[2]) if len(sys.argv) > 2 else 100
# A capture cut off at exit ends in a short row: drop rows with missing fields.
rows = [r for r in csv.DictReader(open(path)) if None not in r.values() and None not in r]
frames = sorted({int(r['frame']) for r in rows})
keep = set(frames[-last-5:-5])
per_frame = collections.Counter()
per_pass = collections.defaultdict(float)   # (rp-ordinal-in-frame, sig)
pass_events = collections.defaultdict(collections.Counter)
ev_types = collections.Counter()
# renderpass ordinal per frame
for r in rows:
    f = int(r['frame'])
    if f not in keep: continue
    t = float(r['time_us'])
    per_frame[f] += t
    key = (int(r['renderpass']),)
    per_pass[r['renderpass']] += t
    pass_events[r['renderpass']][(r['type'], r['vs'], r['fs'], r['cs'])] += t
n = len(keep)
print(f"frames {n}  mean GPU us/frame {sum(per_frame.values())/n:.0f}  min {min(per_frame.values()):.0f} max {max(per_frame.values()):.0f}")
# renderpass ids increase monotonically; normalise to ordinal within frame
rp_frame = {}
for r in rows:
    f = int(r['frame'])
    if f in keep: rp_frame.setdefault(r['renderpass'], f)
first = collections.defaultdict(list)
for rp, f in rp_frame.items(): first[f].append(int(rp))
ordinal = {}
for f, rps in first.items():
    for i, rp in enumerate(sorted(rps)): ordinal[str(rp)] = i
agg = collections.defaultdict(float); aggev = collections.defaultdict(collections.Counter)
for rp, t in per_pass.items():
    agg[ordinal[rp]] += t
    for k, v in pass_events[rp].items(): aggev[ordinal[rp]][k] += v
for o in sorted(agg):
    evs = aggev[o].most_common(3)
    print(f"rp#{o:2d} {agg[o]/n:8.0f} us  " + " | ".join(f"{k[0]} vs={k[1]} fs={k[2]} cs={k[3]} {v/n:.0f}" for k, v in evs))
