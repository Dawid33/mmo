#!/usr/bin/env python3
"""1 Hz resource sampler for `just perf-capture`: CPU/RSS for both processes and the
client's GPU use, all without root.

    scripts/perf_sample.py <capture-dir> client:<pid> server:<pid>

Runs until the first listed process (the client) exits. Writes two CSVs:

rss.csv   t_s,proc,vm_rss_kb,vm_hwm_kb,threads,cpu_ticks
          from /proc/<pid>/status and /proc/<pid>/stat (utime+stime, in clock ticks).

gpu.csv   t_s,source,device,key,value          (long format; keys vary by driver)
  source=fdinfo   the CLIENT PROCESS's own GPU use, from the kernel's DRM fdinfo
                  (/proc/<pid>/fdinfo/*, Documentation/gpu/drm-usage-stats.rst):
                    engine-<name>   cumulative ns that engine ran this process's work
                                    (i915: render/copy/video; amdgpu: gfx/compute/dec/enc)
                    memory-<region> / total-<region> / resident-<region>   KiB
                  busy % of an engine = delta(ns) / delta(wall ns) between two samples;
                  the report does that. Works on i915 and amdgpu (kernel >= 5.19/6.x);
                  nouveau/NVK exposes no engine time, so the T600 yields memory at best.
  source=sysfs    whole-GPU counters from /sys/class/drm/card*, where the driver has any:
                    busy-percent, vram-used-kib, vram-total-kib   (amdgpu)
                    act-freq-mhz, max-freq-mhz                    (i915: clocks only —
                                                                   a ramp tell, not busy %)
                  Whole-GPU means the desktop compositor is in there too.
`device` is the PCI address, so rows can be matched to the adapter in the manifest.
"""

import glob
import os
import re
import sys
import time

INTERVAL_S = 1.0
KIB = {"": 1 / 1024, "kib": 1, "mib": 1024, "gib": 1024 * 1024}


def read(path: str) -> str:
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        return ""


def proc_row(pid: int) -> tuple[int, int, int, int] | None:
    status = read(f"/proc/{pid}/status")
    stat = read(f"/proc/{pid}/stat")
    # A process that is exiting still has a /proc entry but no VmRSS line.
    if not status or not stat or "VmRSS:" not in status:
        return None

    def field(name: str) -> int:
        m = re.search(rf"^{name}:\s+(\d+)", status, re.M)
        return int(m.group(1)) if m else 0

    # utime + stime are fields 14 and 15; count from after the ")" that ends the comm,
    # which may itself contain spaces.
    rest = stat.rsplit(")", 1)[-1].split()
    cpu = int(rest[11]) + int(rest[12]) if len(rest) > 12 else 0
    return field("VmRSS"), field("VmHWM"), field("Threads"), cpu


def fdinfo_rows(pid: int) -> dict[tuple[str, str], float]:
    """Sum DRM usage over the process's unique DRM clients, keyed by (device, key)."""
    totals: dict[tuple[str, str], float] = {}
    seen: set[tuple[str, str]] = set()
    for path in glob.glob(f"/proc/{pid}/fdinfo/*"):
        text = read(path)
        if "drm-driver:" not in text:
            continue
        info = dict(line.split(":", 1) for line in text.splitlines() if ":" in line)
        info = {k.strip(): v.strip() for k, v in info.items()}
        device = info.get("drm-pdev") or info.get("drm-driver", "?")
        # Several fds can share one DRM client (dup'd fds); count each client once.
        client = (device, info.get("drm-client-id", path))
        if client in seen:
            continue
        seen.add(client)
        for key, value in info.items():
            if key.startswith("drm-engine-") and not key.startswith("drm-engine-capacity-"):
                number = value.split()[0]
                if number.isdigit():
                    k = (device, key[len("drm-"):])
                    totals[k] = totals.get(k, 0) + int(number)
            elif key.startswith(("drm-memory-", "drm-total-", "drm-resident-")):
                parts = value.split()
                if parts and parts[0].isdigit():
                    unit = parts[1].lower() if len(parts) > 1 else ""
                    k = (device, key[len("drm-"):] + "-kib")
                    totals[k] = totals.get(k, 0) + int(parts[0]) * KIB.get(unit, 1 / 1024)
    return totals


def sysfs_rows() -> dict[tuple[str, str], float]:
    rows: dict[tuple[str, str], float] = {}
    for card in glob.glob("/sys/class/drm/card[0-9]*"):
        if "-" in os.path.basename(card):  # connectors: card1-eDP-1
            continue
        device = os.path.basename(os.path.realpath(os.path.join(card, "device")))
        for rel, key, scale in (
            ("device/gpu_busy_percent", "busy-percent", 1),
            ("device/mem_info_vram_used", "vram-used-kib", 1 / 1024),
            ("device/mem_info_vram_total", "vram-total-kib", 1 / 1024),
            ("gt_act_freq_mhz", "act-freq-mhz", 1),
            ("gt_max_freq_mhz", "max-freq-mhz", 1),
        ):
            value = read(os.path.join(card, rel)).strip()
            if value.isdigit():
                rows[(device, key)] = int(value) * scale
    return rows


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    out_dir = sys.argv[1]
    procs = [(name, int(pid)) for name, pid in (arg.split(":", 1) for arg in sys.argv[2:])]
    watched_name, watched_pid = procs[0]

    start = time.monotonic()
    with open(os.path.join(out_dir, "rss.csv"), "w", buffering=1) as rss, \
            open(os.path.join(out_dir, "gpu.csv"), "w", buffering=1) as gpu:
        rss.write("t_s,proc,vm_rss_kb,vm_hwm_kb,threads,cpu_ticks\n")
        gpu.write("t_s,source,device,key,value\n")
        while True:
            t = time.monotonic() - start
            alive = False
            for name, pid in procs:
                row = proc_row(pid)
                if row is None:
                    continue
                alive = alive or pid == watched_pid
                rss.write(f"{t:.1f},{name},{row[0]},{row[1]},{row[2]},{row[3]}\n")
            if not alive:
                return 0
            for (device, key), value in sorted(fdinfo_rows(watched_pid).items()):
                gpu.write(f"{t:.1f},fdinfo,{device},{key},{value:.0f}\n")
            for (device, key), value in sorted(sysfs_rows().items()):
                gpu.write(f"{t:.1f},sysfs,{device},{key},{value:.0f}\n")
            # Sleep to the next whole interval so drift does not accumulate.
            time.sleep(max(0.05, INTERVAL_S - ((time.monotonic() - start) % INTERVAL_S)))


if __name__ == "__main__":
    sys.exit(main())
