#!/usr/bin/env python3
"""Turn a `just perf-capture` directory into manifest.json + summary.txt.

    scripts/perf_capture_report.py target/perf-capture/<capture-dir>

Run by scripts/perf_capture.sh at the end of a capture; safe to re-run on an old capture
directory (it only reads client.log / server.log / rss.csv / gpu.csv / diag.txt and rewrites the two
outputs — provenance already in manifest.json is kept when the flags are not given again).

summary.txt is the file to read first. It holds, in order: what was run, whether it
loaded, the join timeline (both processes on one clock), CPU/RSS and GPU use, the game-thread
budget, the server counters, the CPU-vs-GPU verdict, and any errors. Everything in it is
derived from the raw files next to it, which stay available for a closer look — grep them,
never read them whole.

The join timeline comes from structured log lines emitted by both binaries:

    [perf] event=<name> t_ms=<ms since that process started> key=value ...

`process_start` carries `unix_ms`, which is what lets client and server events share one
clock here (t=0 is the client's process start). Events that fire once print as they are;
events that fire many times (one per region, per LOD key) collapse to count / first / last
and the sum + max of every numeric field.
"""

import argparse
import collections
import csv
import json
import os
import platform
import re
import subprocess
import sys

ANSI = re.compile(r"\x1b\[[0-9;]*m")
PERF = re.compile(r"\[perf\] (event=\S+.*)$")
ENV_KNOBS = (
    "VOXEL_WORLD", "VOXEL_NEAR_RADIUS", "VOXEL_LOD_RINGS", "VOXEL_NEAR_RENDER", "VOXEL_FAR_RENDER",
    "VOXEL_CLIENT_WORKER_THREADS", "TOKIO_WORKER_THREADS", "LOD_BUILD_THREADS", "HASH_MODE",
    "DIAG", "DIAG_INTERVAL", "PERF_LOG", "RUST_LOG", "VOXEL_PULL_DEBUG", "WGPU_ADAPTER_NAME", "WGPU_BACKEND",
)
# Repeated events that are gauges, not work: show the last reading instead of sums.
LAST_VALUE_EVENTS = {"load_progress"}
# How many trailing periodic lines of each kind to quote in the summary.
TAIL = 4


def sh(*cmd: str) -> str:
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=10).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return ""


def read_lines(path: str) -> list[str]:
    if not os.path.exists(path):
        return []
    with open(path, errors="replace") as f:
        return [ANSI.sub("", line.rstrip("\n")) for line in f]


def number(text: str):
    try:
        return float(text)
    except ValueError:
        return None


def perf_events(lines: list[str], side: str) -> list[dict]:
    events = []
    for line in lines:
        m = PERF.search(line)
        if not m:
            continue
        fields = dict(kv.split("=", 1) for kv in m.group(1).split() if "=" in kv)
        t = number(fields.pop("t_ms", ""))
        if t is None:
            continue
        events.append({"side": side, "event": fields.pop("event"), "t_ms": t, "fields": fields})
    return events


def host_info() -> dict:
    cpu = ""
    for line in read_lines("/proc/cpuinfo"):
        if line.startswith("model name"):
            cpu = line.split(":", 1)[1].strip()
            break
    mem_kb = 0
    for line in read_lines("/proc/meminfo"):
        if line.startswith("MemTotal:"):
            mem_kb = int(line.split()[1])
            break
    return {"cpu": cpu, "logical_cores": os.cpu_count(), "mem_total_mb": mem_kb // 1024,
            "kernel": platform.release(), "clk_tck": os.sysconf("SC_CLK_TCK")}


def binary_info(profile: str) -> dict:
    out = {}
    for name in ("client", "server"):
        path = os.path.join("target", profile, name)
        if os.path.exists(path):
            st = os.stat(path)
            out[name] = {"path": path, "bytes": st.st_size, "mtime_unix": int(st.st_mtime)}
    return out


def build_manifest(args, client: list[str], previous: dict) -> dict:
    adapter_selected = next((l.split("AdapterInfo", 1)[1].strip() for l in client if "AdapterInfo" in l), "")
    world_resolved = next((l.split("world scale:", 1)[1].strip() for l in client if "world scale:" in l), "")
    started, ended = number(args.started or ""), number(args.ended or "")
    fresh = {
        "git": {
            "revision": sh("git", "rev-parse", "HEAD"),
            "branch": sh("git", "rev-parse", "--abbrev-ref", "HEAD"),
            "dirty": bool(sh("git", "status", "--porcelain", "--untracked-files=no")),
            "dirty_files": sh("git", "status", "--porcelain", "--untracked-files=no").splitlines(),
        },
        "build_profile": args.profile,
        "binaries": binary_info(args.profile),
        "adapter_requested": args.adapter_requested,
        "world_requested": args.world_requested or "full",
        "env": {k: os.environ[k] for k in ENV_KNOBS if k in os.environ},
        "host": host_info(),
        "started_unix": started,
        "ended_unix": ended,
        "duration_s": round(ended - started, 1) if started and ended else None,
        "client_exit_code": args.client_rc,
        "server_exit_code": args.server_rc,
    }
    # Re-running the report on an old capture must not overwrite what was true at capture
    # time (git state, env, exit codes) with what is true now.
    manifest = dict(previous) if previous and args.started is None else fresh
    # These two are read back from the log, so they are right either way — and they are the
    # ones that matter most: what the client actually got, not what was asked for.
    manifest["adapter_selected"] = adapter_selected or "(no AdapterInfo line in client.log)"
    manifest["world_resolved"] = world_resolved or "(no `world scale:` line in client.log)"
    return manifest


def timeline(events: list[dict]) -> list[str]:
    if not events:
        return ["(no [perf] lines — binaries predate the join timeline, or never started)"]
    # One clock: shift each side by its process_start unix_ms, relative to the client's.
    starts = {e["side"]: number(e["fields"].get("unix_ms", "")) for e in events if e["event"] == "process_start"}
    origin = starts.get("client") or min((v for v in starts.values() if v), default=None)
    for e in events:
        base = starts.get(e["side"])
        e["t"] = e["t_ms"] + (base - origin) if base and origin else e["t_ms"]
    out = []
    if not (starts.get("client") and starts.get("server")):
        out.append("(a process_start line is missing — client and server clocks are NOT aligned below)")

    groups: dict[tuple, list[dict]] = collections.OrderedDict()
    for e in sorted(events, key=lambda e: e["t"]):
        # `main_stall` is one event name for several systems; keep them apart.
        name = e["event"] + (f"[{e['fields']['system']}]" if "system" in e["fields"] else "")
        groups.setdefault((e["side"], name), []).append(e)

    out.append(f"{'t_ms':>9} {'last_ms':>9} {'n':>5}  {'side':<6} {'event':<34} detail")
    out.append("-" * 110)
    for (side, name), group in sorted(groups.items(), key=lambda kv: kv[1][0]["t"]):
        first, last = group[0], group[-1]
        if len(group) == 1:
            detail = " ".join(f"{k}={v}" for k, v in first["fields"].items() if k != "unix_ms")
            out.append(f"{first['t']:9.0f} {'':>9} {1:5d}  {side:<6} {name:<34} {detail}")
            continue
        sums: dict[str, float] = collections.OrderedDict()
        maxes: dict[str, float] = {}
        for e in group:
            for k, v in e["fields"].items():
                n = number(v)
                if n is None:
                    continue
                sums[k] = sums.get(k, 0.0) + n
                maxes[k] = max(maxes.get(k, n), n)
        if name in LAST_VALUE_EVENTS:
            # A heartbeat: its sums mean nothing, its latest reading is the point.
            detail = "last: " + " ".join(f"{k}={v}" for k, v in last["fields"].items())
        else:
            detail = " ".join(f"{k}:sum={sums[k]:.0f},max={maxes[k]:.0f}" for k in sums)
        out.append(f"{first['t']:9.0f} {last['t']:9.0f} {len(group):5d}  {side:<6} {name:<34} {detail}")
    return out


def resources(path: str, clk_tck: int) -> list[str]:
    if not os.path.exists(path):
        return ["(no rss.csv)"]
    rows = collections.defaultdict(list)
    with open(path) as f:
        for row in csv.DictReader(f):
            try:
                rows[row["proc"]].append({k: float(v) for k, v in row.items() if k != "proc"})
            except ValueError:
                continue
    out = [f"{'proc':<7} {'samples':>7} {'peak RSS MB':>12} {'final RSS MB':>13} {'threads':>8} "
           f"{'avg CPU %':>10} {'peak CPU %':>11}   (100% = one core)"]
    out.append("-" * 96)
    for proc, samples in rows.items():
        cpu = []
        for a, b in zip(samples, samples[1:]):
            dt = b["t_s"] - a["t_s"]
            if dt > 0:
                cpu.append(100.0 * (b["cpu_ticks"] - a["cpu_ticks"]) / clk_tck / dt)
        out.append(f"{proc:<7} {len(samples):7d} {max(s['vm_hwm_kb'] for s in samples) / 1024:12.0f} "
                   f"{samples[-1]['vm_rss_kb'] / 1024:13.0f} {max(s['threads'] for s in samples):8.0f} "
                   f"{sum(cpu) / len(cpu) if cpu else 0:10.0f} {max(cpu, default=0):11.0f}")
    # RSS over time, coarse: enough to see a join-time spike or a leak without the csv.
    for proc, samples in rows.items():
        step = max(1, len(samples) // 12)
        series = " ".join(f"{s['t_s']:.0f}s:{s['vm_rss_kb'] / 1024:.0f}" for s in samples[::step])
        out.append(f"{proc} RSS MB over time: {series}")
    return out


def gpu(path: str, join_s: float) -> list[str]:
    """Per-engine busy % of the client process (fdinfo) + whole-GPU sysfs counters."""
    if not os.path.exists(path):
        return ["(no gpu.csv)"]
    series: dict[tuple, list[tuple[float, float]]] = collections.defaultdict(list)
    with open(path) as f:
        for row in csv.DictReader(f):
            try:
                series[(row["source"], row["device"], row["key"])].append((float(row["t_s"]), float(row["value"])))
            except (KeyError, ValueError):
                continue
    if not series:
        return ["(gpu.csv is empty: the driver exposes neither DRM fdinfo usage nor sysfs counters —",
                " expected on nouveau/NVK. diag.txt's in-process pass timings are the only GPU evidence.)"]

    def busy(points: list[tuple[float, float]], lo: float, hi: float) -> tuple[float, float]:
        """(avg %, peak %) of a cumulative-ns counter over samples with lo <= t < hi."""
        rates = [100.0 * (v1 - v0) / 1e9 / (t1 - t0)
                 for (t0, v0), (t1, v1) in zip(points, points[1:]) if t1 > t0 and lo <= t0 < hi]
        return (sum(rates) / len(rates), max(rates)) if rates else (0.0, 0.0)

    out = [f"client process, from DRM fdinfo (busy % = engine time / wall time; join = first {join_s:.0f}s)",
           f"{'device':<14} {'engine':<22} {'join avg %':>10} {'join peak %':>11} {'rest avg %':>10} {'rest peak %':>11}",
           "-" * 84]
    engines = [(k, v) for k, v in sorted(series.items()) if k[0] == "fdinfo" and k[2].startswith("engine-")]
    for (_, device, key), points in engines:
        if points[-1][1] == points[0][1]:
            continue  # engine never ran
        ja, jp = busy(points, 0.0, join_s)
        ra, rp = busy(points, join_s, float("inf"))
        out.append(f"{device:<14} {key[len('engine-'):]:<22} {ja:10.0f} {jp:11.0f} {ra:10.0f} {rp:11.0f}")
    if not engines:
        out.append("(no engine time in fdinfo for this driver)")
    for (source, device, key), points in sorted(series.items()):
        values = [v for _, v in points]
        if source == "fdinfo" and key.endswith("-kib") and max(values) > 0:
            out.append(f"{device:<14} {key:<34} peak {max(values) / 1024:8.0f} MB   final {values[-1] / 1024:8.0f} MB")
    sysfs = [(k, v) for k, v in sorted(series.items()) if k[0] == "sysfs"]
    if sysfs:
        out += ["", "whole GPU, from sysfs (includes the desktop compositor)"]
    for (_, device, key), points in sysfs:
        values = [v for _, v in points]
        if key.endswith("-kib"):
            out.append(f"{device:<14} {key:<34} peak {max(values) / 1024:8.0f} MB   final {values[-1] / 1024:8.0f} MB")
        else:
            out.append(f"{device:<14} {key:<34} avg {sum(values) / len(values):9.0f}      peak {max(values):9.0f}")
    return out


def busiest(lines: list[str], needle: str, score, keep: int = 4, last: int = 2) -> list[str]:
    """Periodic lines worth reading: the `keep` highest by `score` plus the last `last`,
    in log order. The interesting windows are the join's, which a plain tail scrolls past."""
    hits = [l[l.index(needle):] for l in lines if needle in l]
    if not hits:
        return [f"(no `{needle}` lines)"]
    ranked = sorted(range(len(hits)), key=lambda i: score(hits[i]), reverse=True)
    chosen = sorted({i for i in ranked[:keep] if score(hits[i]) > 0} | set(range(max(0, len(hits) - last), len(hits))))
    out = [f"({len(hits)} lines; the {keep} busiest + the last {last}, `#n` = window index)"]
    return out + [f"#{i:<3d} {hits[i]}" for i in chosen]


def field_sum(line: str, *patterns: str) -> float:
    total = 0.0
    for pattern in patterns:
        m = re.search(pattern, line)
        if m:
            total += float(m.group(1))
    return total


def tail(lines: list[str], needle: str, n: int = TAIL) -> list[str]:
    hits = [l for l in lines if needle in l]
    if not hits:
        return [f"(no `{needle}` lines)"]
    head = [f"({len(hits)} lines; last {min(n, len(hits))} shown)"]
    return head + [l[l.index(needle):] for l in hits[-n:]]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("capture_dir")
    ap.add_argument("--adapter-requested", default="")
    ap.add_argument("--world-requested", default="")
    ap.add_argument("--profile", default="release")
    ap.add_argument("--started", default=None, help="unix seconds; given by perf_capture.sh")
    ap.add_argument("--ended", default=None)
    ap.add_argument("--client-rc", type=int, default=None)
    ap.add_argument("--server-rc", type=int, default=None)
    args = ap.parse_args()

    d = args.capture_dir
    client = read_lines(os.path.join(d, "client.log"))
    server = read_lines(os.path.join(d, "server.log"))
    manifest_path = os.path.join(d, "manifest.json")
    previous = {}
    if os.path.exists(manifest_path):
        with open(manifest_path) as f:
            previous = json.load(f)
    manifest = build_manifest(args, client, previous)
    with open(manifest_path, "w") as f:
        json.dump(manifest, f, indent=2)
        f.write("\n")

    events = perf_events(client, "client") + perf_events(server, "server")
    reached = {e["event"] for e in events}
    git = manifest.get("git", {})

    s: list[str] = []
    s.append(f"PERF CAPTURE  {os.path.basename(os.path.normpath(d))}")
    s.append("=" * 110)
    s.append(f"git        : {git.get('revision', '?')[:12]} on {git.get('branch', '?')}"
             f"{'  DIRTY: ' + ', '.join(x.split(maxsplit=1)[-1] for x in git.get('dirty_files', [])[:6]) if git.get('dirty') else '  (clean)'}")
    s.append(f"profile    : {manifest.get('build_profile')}"
             f"{'' if manifest.get('build_profile') in ('release', 'profiling') else '   <-- NOT a release build: numbers measure the build, not the game'}")
    s.append(f"adapter    : asked for '{manifest.get('adapter_requested') or '(unpinned)'}' -> got {manifest['adapter_selected']}")
    s.append(f"world      : asked for '{manifest.get('world_requested')}' -> {manifest['world_resolved']}")
    s.append(f"env        : {' '.join(f'{k}={v}' for k, v in manifest.get('env', {}).items()) or '(none of the knobs set)'}")
    host = manifest.get("host", {})
    s.append(f"host       : {host.get('cpu')} x{host.get('logical_cores')}, {host.get('mem_total_mb')} MB, Linux {host.get('kernel')}")
    s.append(f"run        : {manifest.get('duration_s')} s, client exit {manifest.get('client_exit_code')}, "
             f"server exit {manifest.get('server_exit_code')}  (143 = closed by PERF_CAPTURE_SECONDS)")
    s.append(f"loaded     : {'YES — client reached world_ready' if 'world_ready' in reached else 'NO world_ready event — the join did not complete (or the binaries predate the timeline)'}")

    s += ["", "JOIN TIMELINE  (ms since client process start; repeated events collapsed)", ""]
    s += timeline(events)

    s += ["", "RESOURCES  (rss.csv, 1 Hz)", ""]
    s += resources(os.path.join(d, "rss.csv"), host.get("clk_tck") or 100)

    ready = next((e for e in events if e["event"] == "world_ready"), None)
    join_s = max(10.0, ready["t"] / 1000.0) if ready and "t" in ready else 30.0
    s += ["", "GPU  (gpu.csv, 1 Hz, no root; per-pass timings are under CPU vs GPU below)", ""]
    s += gpu(os.path.join(d, "gpu.csv"), join_s)

    s += ["", "CLIENT GAME THREAD  ([client-budget]: where the game loop's wall time goes)", ""]
    s += busiest(client, "[client-budget]", lambda l: field_sum(l, r"util=([\d.]+)%"))
    s += [""] + tail(client, "[client] mispredicts/s")
    warnings = sum("KEEP-UP WARNING" in l for l in client)
    s.append(f"KEEP-UP WARNING lines: {warnings}{'   <-- prediction log is running away' if warnings else ''}")
    s += ["  " + l[l.index("KEEP-UP WARNING"):][:150] for l in client if "KEEP-UP WARNING" in l][:5]

    s += ["", "SERVER", ""]
    # Busy = manager-thread worldgen + LOD traffic + backlog: the windows where the hub stalls.
    s += busiest(server, "[server] window=",
                 lambda l: field_sum(l, r"gen n=\d+ sum=([\d.]+)ms", r" sub=(\d+)", r" built=(\d+)",
                                     r"backlog in=(\d+)", r" region=(\d+) tx="))
    s += [""] + tail(server, "[region ", n=9)
    drops = sum("send to client timed out" in l for l in server)
    s.append(f"`send to client timed out; skipping` lines: {drops}"
             f"{'   <-- packets were dropped: the join may be permanently incomplete' if drops else ''}")

    s += ["", "CPU vs GPU  (diag.txt — steady state, last DIAG dump)", ""]
    s += read_lines(os.path.join(d, "diag.txt")) or ["(no diag.txt)"]

    lod_state = [l[l.index("[perf-lod]"):] for l in client if "[perf-lod]" in l]
    if lod_state:
        s += ["", "LOD RENDER STATE AT world_ready  ([perf-lod]: what the client meshed, per LOD root;",
              "map rows = z, columns = x, `.` no cell, `0` no quads, 1-9 quad density)", ""]
        s += lod_state
    pull = [l[l.index("[pull-cull]"):] for l in client if "[pull-cull]" in l]
    if pull:
        s += ["", "PULL-PATH CULLING  (VOXEL_PULL_DEBUG=1, once a second; last 6 lines)", ""] + pull[-6:]

    s += ["", "ERRORS", ""]
    bad = re.compile(r"panicked|\bERROR\b|\bWARN\b")
    for name, lines in (("client.log", client), ("server.log", server)):
        hits = [l for l in lines if bad.search(l) and "KEEP-UP WARNING" not in l]
        s.append(f"{name}: {len(hits)} WARN/ERROR/panic lines")
        counts = collections.Counter(re.sub(r"\d+(\.\d+)?", "N", l[-160:]) for l in hits)
        s += [f"  x{n:<4d} {text.strip()}" for text, n in counts.most_common(6)]
        # A panic's message is on the line AFTER "panicked at file:line".
        for i, l in enumerate(lines):
            if "panicked at" in l and i + 1 < len(lines):
                s.append(f"  PANIC: {lines[i + 1].strip()[:200]}")

    with open(os.path.join(d, "summary.txt"), "w") as f:
        f.write("\n".join(s) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
