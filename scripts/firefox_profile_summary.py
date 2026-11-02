#!/usr/bin/env python3
"""Reduce a Firefox Profiler capture of the web build to pasteable text tables.

Record in Firefox (about:profiling or the devtools Performance tab; the "Graphics" preset
adds the compositor/renderer threads), then "Upload → Download" the profile: a
`*.json.gz` in the profiler's *processed* format (any recent version; the shared-table
layout of format >= 50 and the `prefixOffset` stack encoding of >= 76 are both handled).
The wasm build is symbolicated by Firefox itself (`client.wasm.<rust path>` frames), so
nothing needs to be re-symbolicated here.

    scripts/firefox_profile_summary.py profile.json.gz [--top N]
    scripts/firefox_profile_summary.py profile.json.gz --from-ms 5000      # skip the join
    scripts/firefox_profile_summary.py profile.json.gz --thread CanvasRenderer
    scripts/firefox_profile_summary.py profile.json.gz --logs              # every log line

What it prints (times are ms since the profile started; "wall" = sampled time, which
includes time the thread was blocked, e.g. in a sync WebGL IPC round trip; "cpu" = the
thread's real CPU time from `threadCPUDelta`):

  1. overview      — browser, CPU, window, every thread: wall/cpu/busy%.
  2. game thread   — the thread with the most `client.wasm.` samples (Bevy runs on the
                     page's main thread on wasm; there are no worker threads):
       * frames          rAF-callback markers (falls back to `App::update` sample runs):
                         count, fps, p50/p95/max, the slowest frames.
       * where it went   wasm by crate / WebGL calls / JS glue / GC / browser / idle.
       * bevy systems    the frame under each `FunctionSystem::run_unsafe` (the
                         system fn; Firefox truncates the FunctionSystem name itself).
       * crates (incl)   every crate on the stack — rendering (wgpu*, bevy_render,
                         bevy_pbr) vs sim (game, rapier3d) vs UI (slint) at a glance.
       * self / incl     the top functions.
       * graphics        WebGL entry points (inclusive), the wgpu_hal::gles side, and the
                         sync IPC markers (`PWebGL::Msg_*`, each one a stall waiting for
                         the GPU process/thread).
       * long tasks      `MainThreadLongTask` markers, each with its top systems.
  3. other threads — the WebGL executor (`CanvasRenderer`, in the parent process),
                     the compositor/renderer: cpu, top self functions, top libraries
                     (libgallium = the Mesa driver).
  4. markers       — per-name count / total / max of interval markers, per thread.
  5. memory        — the malloc counter per process: start, peak, end.
  6. game log      — the `[perf]` lines the client logged (tracing-wasm turns every log
                     line into a UserTiming marker), with their profile time.
  7. verdict       — a few lines naming the dominant costs.

Only the standard library; streams nothing because Firefox profiles are small (tens of MB
uncompressed). The input is untrusted data: it is only parsed, never executed.
"""

import argparse
import bisect
import collections
import gzip
import json
import re
import sys

HASH_RE = re.compile(r"\[[0-9a-f]{8,}\]")
WASM_PREFIX = "client.wasm."
FIRST_PARTY = {
    "client", "game", "worldgen", "server", "slint_ui", "slint_ui_game",
    "slint_ui_frontend", "slint_ui_tools", "bbmodel", "state_hash", "macros",
}
RENDER_CRATES = (
    "wgpu", "wgpu_core", "wgpu_hal", "wgpu_types", "naga", "glow", "bevy_render",
    "bevy_pbr", "bevy_core_pipeline", "bevy_mesh", "bevy_image", "bevy_camera",
    "bevy_light", "bevy_shader", "bevy_material",
)
WEBGL_API_RE = re.compile(r"^(WebGL2?RenderingContext|WebGLRenderingContext)\.")
WEBGL_NATIVE_RE = re.compile(r"(ClientWebGLContext|WebGLContext|HostWebGLContext|webgl::)")
GLUE_RE = re.compile(r"^(__wbg_|__wbindgen_|__wbg_get_imports)")
# Frames that carry no information of their own: JIT code addresses, unsymbolicated libc
# helpers (`fun_98d70` — on a blocked thread usually the futex/poll wait), trampolines.
ANON_RE = re.compile(r"^(0x[0-9a-f]+|fun_[0-9a-f]+|fast (exit|entry) trampoline.*|<\.plt ELF section.*|"
                     r"syscall|poll|__GI___poll|futex_.*|__futex.*|pthread_cond_.*wait.*|epoll_wait)$")
IPC_RE = re.compile(r"^P[A-Za-z]+::Msg_[A-Za-z]+$")
# Known costs worth a sentence when they show up (regex on a function name → explanation).
HINTS = [
    (r"GetIndexedFetchMaxVert|MaxForRange",
     "WebGL index-range validation: for every drawElements Firefox finds the largest index in "
     "the drawn range on the CPU (cached per buffer+range; any bufferSubData/bufferData on the "
     "index buffer drops that buffer's cache, so draws from a big shared index buffer that is "
     "being written re-scan it). Fewer/larger index-buffer writes, separate index buffers per "
     "upload batch, or WebGPU avoid it."),
    (r"PWebGL::Msg_GetFrontBuffer",
     "GetFrontBuffer: the page's paint waiting for the WebGL executor to finish every queued "
     "command of the frame — time here is the executor's backlog, not the page's work."),
    (r"PWebGL::Msg_SyncPing",
     "SyncPing: a WebGL call that needed a round trip — a sync query (getError, getParameter, "
     "readPixels, fence/clientWaitSync) or the command queue being full (backpressure from a "
     "busy executor)."),
    (r"texSubImage2D|texImage2D|TexImage",
     "Texture uploads: CPU copy + IPC of the pixel data."),
    (r"bufferSubData|BufferSubData|BufferData",
     "Buffer uploads: the data is copied into the IPC shmem and again by the driver."),
    (r"StateHasher|crc32fast|state_hash",
     "Rollback hash verification (game::StateHasher → crc32): per-entry `HashMode::Full` walks "
     "and the server-interval `state_hash`. Its cost scales with the state walked per entry."),
    (r"PackedLodGrid>::decode|mesh_quads|meshing::",
     "Client meshing / LOD decode running on the main thread (wasm has no worker pool)."),
    (r"Minor GC|Major GC|GCSlice|CC \(",
     "JS GC/CC: garbage from the JS glue (wasm-bindgen arrays/closures) or the page."),
]


# ---------------------------------------------------------------------------------------
# loading


def load(path):
    opener = gzip.open if path.endswith(".gz") else open
    with opener(path, "rt", encoding="utf-8") as f:
        return json.load(f)


class Tables:
    """The processed format's shared tables (format >= 50), or a thread's own (older)."""

    def __init__(self, src, strings):
        self.S = strings
        st = src["stackTable"]
        self.stack_frame = st["frame"]
        if "prefix" in st:
            self.stack_prefix = st["prefix"]
        else:  # format 76: prefix delta-encoded, 0 = no prefix
            self.stack_prefix = [None if o == 0 else i - o for i, o in enumerate(st["prefixOffset"])]
        fr = src["frameTable"]
        self.frame_func = fr["func"]
        self.frame_cat = fr["category"]
        self.frame_subcat = fr.get("subcategory")
        self.frame_symbol = fr.get("nativeSymbol")
        fu = src["funcTable"]
        self.func_name = [strings[n] for n in fu["name"]]
        self.func_resource = fu.get("resource")
        self.res_name = [strings[n] for n in src["resourceTable"]["name"]] if "resourceTable" in src else []
        ns = src.get("nativeSymbols")
        self.symbol_lib = ns["libIndex"] if ns else None
        self._chain_cache = {}
        self._cat_cache = {}

    def chain(self, stack):
        """Frames of a stack, leaf first."""
        got = self._chain_cache.get(stack)
        if got is None:
            out = []
            s = stack
            while s is not None:
                out.append(self.stack_frame[s])
                s = self.stack_prefix[s]
            got = tuple(out)
            self._chain_cache[stack] = got
        return got

    def stack_category(self, stack):
        """Category of a stack: the innermost frame with a non-default (non-0) category.

        Native frames carry category 0 meaning "inherit from the caller", so resolving
        leaf→root to the first non-zero category is what the profiler front-end shows."""
        got = self._cat_cache.get(stack)
        if got is None:
            got = (0, 0)
            for f in self.chain(stack):
                c = self.frame_cat[f]
                if c:
                    got = (c, (self.frame_subcat[f] or 0) if self.frame_subcat else 0)
                    break
            self._cat_cache[stack] = got
        return got

    def name(self, frame):
        return self.func_name[self.frame_func[frame]]

    def eff_leaf(self, chain):
        """Innermost frame that is not anonymous (JIT address, libc helper, trampoline)."""
        for f in chain:
            if not ANON_RE.match(self.name(f)):
                return f
        return chain[0]

    def lib(self, frame, libs):
        if self.frame_symbol is None or self.symbol_lib is None:
            return None
        sym = self.frame_symbol[frame]
        if sym is None:
            return None
        return libs[self.symbol_lib[sym]]["name"]


# ---------------------------------------------------------------------------------------
# names


def clean(name):
    """Strip rustc disambiguator hashes and the `client.wasm.` module prefix."""
    name = HASH_RE.sub("", name)
    if name.startswith(WASM_PREFIX):
        name = name[len(WASM_PREFIX):]
    return name


def short(name, width):
    name = clean(name)
    return name if len(name) <= width else name[: width - 1] + "…"


def crate_of(name):
    """Crate of a (cleaned) Rust path: `<A as B>::f` → A's crate, `a::b::f` → a."""
    n = name
    while n.startswith("<"):
        n = n[1:]
    n = n.lstrip("&*").replace("mut ", "", 1).replace("dyn ", "", 1)
    head = re.split(r"::|<| |\(|\[", n, maxsplit=1)[0]
    return head or "?"


def is_wasm(name):
    return name.startswith(WASM_PREFIX)


def pct(part, whole):
    return 100.0 * part / whole if whole else 0.0


def quantile(sorted_vals, q):
    if not sorted_vals:
        return 0.0
    i = min(len(sorted_vals) - 1, max(0, int(round(q * (len(sorted_vals) - 1)))))
    return sorted_vals[i]


# ---------------------------------------------------------------------------------------
# per-thread sample view


class ThreadView:
    def __init__(self, thread, tables, zero, t_from, t_to, interval):
        self.thread = thread
        self.T = tables
        self.name = thread.get("name", "?")
        self.process = thread.get("processName") or thread.get("processType") or "?"
        self.pid = thread.get("pid")
        self.tid = thread.get("tid")
        s = thread["samples"]
        times = s.get("time")
        if times is None:  # timeDeltas encoding
            acc, times = 0.0, []
            for d in s["timeDeltas"]:
                acc += d
                times.append(acc)
        n = len(times)
        cpu = s.get("threadCPUDelta") or [None] * n
        weights = s.get("weight") or [1] * n
        self.samples = []  # (t_rel_ms, stack, wall_ms, cpu_ms or None)
        for i in range(n):
            t = times[i] - zero
            if t < t_from or t > t_to:
                continue
            nxt = times[i + 1] - times[i] if i + 1 < n else interval
            wall = min(nxt, interval * 4) * (weights[i] or 1)  # cap gaps (paused ranges)
            c = cpu[i]
            self.samples.append((t, s["stack"][i], wall, None if c is None else c / 1e6))
        self.wall = sum(x[2] for x in self.samples)
        cpus = [x[3] for x in self.samples if x[3] is not None]
        self.cpu = sum(cpus) if cpus else None
        self.wasm_samples = 0
        for _, st, _, _ in self.samples:
            if st is not None and any(is_wasm(self.T.name(f)) for f in self.T.chain(st)):
                self.wasm_samples += 1

    def busy_wall(self):
        """Wall time of samples whose resolved category is not Idle."""
        return sum(w for _, st, w, _ in self.samples if st is not None and self.T.stack_category(st)[0] != 0)


# ---------------------------------------------------------------------------------------
# markers


def markers_of(thread, strings, zero, t_end=None):
    """(name, start, end, data) for interval markers and (name, t, None, data) for instants.
    Start/end halves (phase 2/3) are paired per name, LIFO. With `t_end`, a start still open
    when the recording stopped is closed there and an end whose start fell before the
    recording began is opened at 0; both get `data["_partial"] = True`."""
    m = thread["markers"]
    open_ = collections.defaultdict(list)
    out = []
    partial = lambda d: dict(d or {}, _partial=True)
    for i in range(m["length"]):
        name = strings[m["name"][i]]
        phase = m["phase"][i]
        st, en, data = m["startTime"][i], m["endTime"][i], m["data"][i]
        if phase == 0:
            out.append((name, st - zero, None, data))
        elif phase == 1:
            out.append((name, st - zero, en - zero, data))
        elif phase == 2:
            open_[name].append((st, data))
        elif phase == 3:
            if open_[name]:
                s0, d0 = open_[name].pop()
                out.append((name, s0 - zero, en - zero, d0 or data))
            elif t_end is not None:
                out.append((name, 0.0, en - zero, partial(data)))
    if t_end is not None:
        for name, opens in open_.items():
            for s0, d0 in opens:
                out.append((name, s0 - zero, t_end, partial(d0)))
    return out


# ---------------------------------------------------------------------------------------
# printing helpers


def table(rows, header, total, top, width=100, rest=True):
    """rows: list of (label, wall_ms[, extra]) sorted descending. `rest=False` for
    inclusive tables, whose leftover sum means nothing."""
    print(f"  {'ms':>9} {'%':>6}  {header}")
    for row in rows[:top]:
        label, ms = row[0], row[1]
        extra = f"  {row[2]}" if len(row) > 2 and row[2] else ""
        print(f"  {ms:9.1f} {pct(ms, total):5.1f}%  {short(label, width)}{extra}")
    if len(rows) > top:
        if rest:
            left = sum(r[1] for r in rows[top:])
            print(f"  {left:9.1f} {pct(left, total):5.1f}%  … {len(rows) - top} more")
        else:
            print(f"  {'':9} {'':6}  … {len(rows) - top} more")


def section(title):
    print()
    print(f"== {title} " + "=" * max(0, 86 - len(title)))


# ---------------------------------------------------------------------------------------
# game-thread analysis


WRAPPER_CRATES = {"winit", "bevy_winit", "wasm_bindgen", "bevy_app"}


def is_plumbing(name):
    """Event-loop / schedule wrapper frames that sit above every system."""
    c = clean(name)
    if crate_of(c) in WRAPPER_CRATES:
        return True
    return crate_of(c) == "bevy_ecs" and re.search(
        r"schedule|executor|resource_scope|run_system|ExclusiveFunctionSystem|FunctionSystem<", c) is not None


SKIP_SYSTEM_CRATES = {"bevy_ecs", "core", "alloc", "std", "bevy_utils", "bevy_tasks", "?"}


def system_of(T, chain):
    """The Bevy system a sample is in.

    Release wasm inlines most systems into the executor (and Firefox truncates the long
    `FunctionSystem<…>` frame names before their method), so the anchor is the innermost
    `…Executor as SystemExecutor>::run`: the first frame below it that is not bevy_ecs /
    core / alloc plumbing is the system — or the first thing it calls when the system body
    itself was inlined. `<&mut f as FnMut<…>>::call_mut` is unwrapped to `f`. The result is
    prefixed with the schedule family, [render] for the render sub-app, else [main]."""
    names = [T.name(f) for f in chain]  # leaf first
    anchor = None
    for i, n in enumerate(names):
        if "SystemExecutor>::run" in n:
            anchor = i
            break
    if anchor is None:
        return None
    fam = "[render] " if any("run_render_schedule" in n or "RenderScheduleOrder" in n for n in names) else "[main] "
    for j in range(anchor - 1, -1, -1):
        n = names[j]
        if ANON_RE.match(n) or not is_wasm(n):
            continue
        c = clean(n)
        m = re.match(r"<&mut (.+?) as core::ops::function::FnMut", c)
        if m:
            c = m.group(1)
        if crate_of(c) in SKIP_SYSTEM_CRATES:
            continue
        c = re.sub(r"(::\{closure#\d+\})+$", "", c)
        return fam + c
    return fam + "<bevy_ecs internals: executor / apply_deferred / commands>"


def bucket_of(T, chain, cats):
    """Coarse owner of one sample's self time."""
    leaf = T.name(T.eff_leaf(chain))
    names = [T.name(f) for f in chain]
    if IPC_RE.match(leaf):
        return f"blocked: sync IPC {leaf}"
    if any(WEBGL_API_RE.match(n) for n in names) or any(WEBGL_NATIVE_RE.search(n) and not is_wasm(n) for n in names[:8]):
        return "graphics: WebGL call (browser side)"
    if is_wasm(leaf):
        return "wasm: " + crate_of(clean(leaf))
    if GLUE_RE.match(leaf):
        return "js: wasm-bindgen glue"
    return None


def analyze_game(view, markers, cats, libs, top, from_ms, to_ms):
    T = view.T
    total = view.wall
    section(f"game thread: {view.name} ({view.process}, pid {view.pid}) — wall {total:.0f} ms"
            + (f", cpu {view.cpu:.0f} ms" if view.cpu is not None else ""))

    # ---- frames
    rafs = sorted((s, e, bool((dd or {}).get("_partial"))) for n, s, e, dd in markers
                  if n == "requestAnimationFrame callbacks" and e is not None and from_ms <= s <= to_ms)
    source = "requestAnimationFrame callbacks markers"
    frames = [(s, e - s) for s, e, _ in rafs]
    n_partial = sum(p for _, _, p in rafs)
    if n_partial:
        source += f"; {n_partial} cut off by the recording's start/end, counted at their visible length"
    if len(frames) < 3 and n_partial < len(frames):
        print(f"\n  only {len(frames)} rAF callbacks; also deriving frames from App::update samples")
    if len(frames) < 3:
        # Fallback: runs of samples inside App::update.
        source = "runs of samples inside bevy App::update (no rAF markers)"
        frames, run_start, last = [], None, None
        for t, st, w, _ in view.samples:
            inside = st is not None and any("App>::update" in T.name(f) for f in T.chain(st))
            if inside:
                if run_start is None or t - last > 2.5 * (w or 1):
                    if run_start is not None:
                        frames.append((run_start, last - run_start + w))
                    run_start = t
                last = t
        if run_start is not None:
            frames.append((run_start, last - run_start + 1))
    print(f"\n-- frames ({source})")
    if frames:
        durs = sorted(d for _, d in frames)
        span = (frames[-1][0] + frames[-1][1]) - frames[0][0] if len(frames) > 1 else durs[0]
        starts = [s for s, _ in frames]
        periods = [(a, b - a) for a, b in zip(starts, starts[1:])]
        gaps = sorted(p for _, p in periods)
        # The frame TIME is start→start: it includes paint/present (e.g. a sync
        # GetFrontBuffer wait), which happens after the rAF callback returns.
        print(f"  {len(frames)} frames over {span:.0f} ms → {1000 * len(frames) / span if span else 0:.1f} fps")
        if gaps:
            print(f"  frame time (start→start): p50 {quantile(gaps, .5):.1f}  p95 {quantile(gaps, .95):.1f}"
                  f"  max {gaps[-1]:.1f} ms; over 16.7 ms: {sum(g > 16.7 for g in gaps)}/{len(gaps)}")
        print(f"  inside the callback: p50 {quantile(durs, .5):.1f}  p95 {quantile(durs, .95):.1f}  max {durs[-1]:.1f} ms"
              f"  (sum {sum(durs):.0f} = {pct(sum(durs), total):.0f}% of window)")
        if gaps and quantile(durs, .5) < 0.5 * quantile(gaps, .5):
            print("  → most of each frame is OUTSIDE the rAF callback (paint / present / other tasks)")
        if len(frames) < 3:
            print("  too few frames for statistics: each frame is a long task — see 'long tasks'")
        slow = sorted(periods or frames, key=lambda x: -x[1])[: min(5, top)]
        print("  slowest: " + ", ".join(f"{d:.0f} ms @ {s:.0f}" for s, d in slow))
    else:
        print("  no frames found")

    # ---- buckets / systems / crates / functions
    buckets = collections.Counter()
    systems = collections.Counter()
    crates_incl = collections.Counter()
    self_fn = collections.Counter()
    incl_fn = collections.Counter()
    webgl_api = collections.Counter()
    hal = collections.Counter()
    libs_self = collections.Counter()
    sub = collections.Counter()
    for t, st, w, _ in view.samples:
        if st is None:
            buckets["idle (no stack)"] += w
            continue
        chain = T.chain(st)
        cat, subcat = T.stack_category(st)
        b = bucket_of(T, chain, cats)
        if b is None:
            cname = cats[cat]["name"] if cat < len(cats) else str(cat)
            if cname == "JavaScript":
                b = "js: page/other JS"
            elif cname == "Idle":
                b = "idle"
            else:
                b = f"browser: {cname}"
        buckets[b] += w
        if cat < len(cats):
            subs = cats[cat].get("subcategories") or []
            sub[f"{cats[cat]['name']}: {subs[subcat] if subcat < len(subs) else subcat}"] += w
        sysname = system_of(T, chain)
        if sysname:
            systems[sysname] += w
        elif any("App>::update" in T.name(f) for f in chain):
            systems["<in App::update, outside any executor> (sub-app extract/glue)"] += w
        seen_c, seen_f = set(), set()
        for f in chain:
            n = T.name(f)
            if f not in seen_f:
                seen_f.add(f)
                incl_fn[n] += w
            if is_wasm(n):
                c = crate_of(clean(n))
                if c not in seen_c:
                    seen_c.add(c)
                    crates_incl[c] += w
        self_fn[T.name(T.eff_leaf(chain))] += w
        lib = T.lib(chain[0], libs)
        libs_self[lib or ("wasm" if is_wasm(T.name(chain[0])) else "js/label")] += w
        for f in chain:
            n = T.name(f)
            if WEBGL_API_RE.match(n):
                webgl_api[n] += w
                break
        for f in reversed(chain):  # outermost wgpu_hal frame
            n = T.name(f)
            if is_wasm(n) and "wgpu_hal" in n:
                hal[re.sub(r"\[[0-9a-f]+\]", "", n)] += w
                break

    print("\n-- where the thread's time went (self time, by owner)")
    table(buckets.most_common(), "owner", total, top)
    print("\n-- profiler categories (self, resolved)")
    table(sub.most_common(), "category: subcategory", total, min(top, 12))
    print("\n-- bevy systems (inclusive; first non-bevy_ecs frame under the innermost executor)")
    table(systems.most_common(), "system", total, top, width=120, rest=False)
    print("\n-- wasm crates (inclusive: crate anywhere on the stack)")
    tag = lambda c: "render" if c in RENDER_CRATES else ("first-party" if c in FIRST_PARTY else "")
    table([(c, ms, tag(c)) for c, ms in crates_incl.most_common() if c not in WRAPPER_CRATES],
          "crate (event-loop wrappers winit/wasm_bindgen/bevy_app hidden)", total, top, rest=False)
    print("\n-- top self functions")
    table(self_fn.most_common(), "function", total, top, width=130)
    print("\n-- top inclusive functions (wasm + WebGL; event-loop/schedule wrappers skipped)")
    rows = [(n, ms) for n, ms in incl_fn.most_common()
            if (WEBGL_API_RE.match(n) or (is_wasm(n) and not is_plumbing(n))) and ms < 0.98 * total]
    table(rows, "function", total, top, width=130, rest=False)

    section("graphics on the game thread")
    gl_total = sum(webgl_api.values())
    print(f"\n-- WebGL API calls (inclusive, outermost WebGL frame): {gl_total:.1f} ms = {pct(gl_total, total):.1f}%")
    table(webgl_api.most_common(), "call", total, top)
    hal_total = sum(hal.values())
    print(f"\n-- wgpu_hal::gles entry points (inclusive): {hal_total:.1f} ms = {pct(hal_total, total):.1f}%")
    table(hal.most_common(), "function", total, min(top, 12), width=130)
    render = sum(ms for c, ms in crates_incl.items() if c in RENDER_CRATES)
    print(f"\n  render-crate time is not additive across crates; largest single render crate:"
          f" {max((crates_incl[c] for c in RENDER_CRATES), default=0):.1f} ms")
    ipc = collections.defaultdict(list)
    for n, s, e, _ in markers:
        if n.startswith("PWebGL::") or n.startswith("PCanvas") or n.startswith("PWebGPU"):
            if e is not None:
                ipc[n].append(e - s)
            else:
                ipc[n]
    print("\n-- WebGL IPC markers (sync messages block the thread until the executor answers)")
    if ipc:
        print(f"  {'count':>6} {'total ms':>9} {'max ms':>8}  message")
        for n, ds in sorted(ipc.items(), key=lambda kv: -sum(kv[1])):
            print(f"  {len(ds):6d} {sum(ds):9.1f} {max(ds, default=0):8.2f}  {n}")
    else:
        print("  none")

    # ---- long tasks
    long_tasks = sorted(((s, e) for n, s, e, dd in markers
                         if e is not None and from_ms <= e and s <= to_ms
                         and (n in ("LongTask", "MainThreadLongTask") or (dd or {}).get("type") == "MainThreadLongTask")),
                        key=lambda x: -(x[1] - x[0]))
    section(f"long tasks on the game thread ({len(long_tasks)})")
    times = [x[0] for x in view.samples]
    for s, e in long_tasks[:top]:
        lo, hi = bisect.bisect_left(times, s), bisect.bisect_right(times, e)
        c = collections.Counter()
        for _, st, w, _ in view.samples[lo:hi]:
            if st is None:
                continue
            chain = T.chain(st)
            b = bucket_of(T, chain, cats)
            c[b if b and b.startswith("blocked") else (system_of(T, chain) or b or short(T.name(T.eff_leaf(chain)), 80))] += w
        win = sum(c.values())
        print(f"  {e - s:7.0f} ms @ {s:7.0f}:  " + "; ".join(
            f"{short(k, 70)} {pct(v, win):.0f}%" for k, v in c.most_common(4)))
    return buckets, systems, crates_incl, frames, webgl_api, ipc


def game_self_names(view):
    """Self time per (effective-leaf and IPC-message) name on a thread — what HINTS match."""
    T = view.T
    out = collections.Counter()
    for _, st, w, _ in view.samples:
        if st is None:
            continue
        chain = T.chain(st)
        out[T.name(T.eff_leaf(chain))] += w
        for f in chain:
            n = T.name(f)
            if WEBGL_API_RE.match(n):
                out[n] += w
                break
    return out


def analyze_other(view, libs, top):
    """A non-game thread, weighted by CPU time (threadCPUDelta) when the profile has it, so
    a thread parked in poll()/a futex does not look busy; else by non-Idle wall time."""
    T = view.T
    total = view.wall
    by_cpu = view.cpu is not None
    busy = view.cpu if by_cpu else view.busy_wall()
    print(f"\n-- {view.name} ({view.process}, pid {view.pid}): wall {total:.0f} ms, "
          + (f"on CPU {busy:.0f} ms ({pct(busy, total):.0f}%)" if by_cpu
             else f"non-idle {busy:.0f} ms ({pct(busy, total):.0f}%)"))
    if busy < 0.01 * total:
        return {}
    self_fn = collections.Counter()
    libs_self = collections.Counter()
    gl_incl = collections.Counter()
    for _, st, w, c in view.samples:
        if st is None:
            continue
        if by_cpu:
            w = c or 0.0
            if not w:
                continue
        elif T.stack_category(st)[0] == 0:
            continue
        chain = T.chain(st)
        self_fn[T.name(T.eff_leaf(chain))] += w
        libs_self[T.lib(chain[0], libs) or "js/label"] += w
        seen = set()
        for f in chain:
            n = T.name(f)
            if n not in seen and re.search(r"WebGL|webgl::|HostWebGL|gl::GLContext|Msg_|Recv[A-Z]", n):
                seen.add(n)
                gl_incl[n] += w
    unit = "cpu" if by_cpu else "busy"
    print(f"   libraries (self, % of {unit}):")
    table(libs_self.most_common(), "library", busy, 6)
    print(f"   functions (self, % of {unit}):")
    table(self_fn.most_common(), "function", busy, min(top, 12), width=120)
    if gl_incl:
        print(f"   WebGL / IPC frames (inclusive, % of {unit}):")
        table(gl_incl.most_common(), "function", busy, min(top, 12), width=120, rest=False)
    return self_fn


# ---------------------------------------------------------------------------------------


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("profile", help="Firefox Profiler processed profile (.json or .json.gz)")
    ap.add_argument("--top", type=int, default=25)
    ap.add_argument("--from-ms", type=float, default=0.0, help="analyse from this profile time")
    ap.add_argument("--to-ms", type=float, default=float("inf"), help="… up to this profile time")
    ap.add_argument("--thread", help="game thread name/tid substring (default: most wasm samples)")
    ap.add_argument("--logs", action="store_true", help="print every UserTiming log line, not only [perf]")
    args = ap.parse_args()

    d = load(args.profile)
    meta = d["meta"]
    if "threads" not in d or not d["threads"] or "samples" not in d["threads"][0]:
        sys.exit("not a processed Firefox profile (open it in profiler.firefox.com and "
                 "use Upload → Download to save the processed form)")
    cats = meta.get("categories") or []
    libs = d.get("libs") or []
    interval = meta.get("interval", 1.0)
    shared = d.get("shared") or {}
    strings_shared = shared.get("stringArray")

    tables = {}

    def tables_for(th):
        if "stackTable" in shared:
            key = "shared"
            if key not in tables:
                tables[key] = Tables(shared, strings_shared)
            return tables[key]
        key = id(th)
        if key not in tables:
            tables[key] = Tables(th, th.get("stringArray") or th["stringTable"])
        return tables[key]

    all_times = [t for th in d["threads"] for t in (th["samples"].get("time") or [])[:1]]
    zero = meta.get("profilingStartTime") or (min(all_times) if all_times else 0.0)
    end = meta.get("profilingEndTime")

    views = [ThreadView(th, tables_for(th), zero, args.from_ms, args.to_ms, interval) for th in d["threads"]]
    strings_of = lambda th: strings_shared if strings_shared is not None else (th.get("stringArray") or th["stringTable"])

    section("overview")
    print(f"  {meta.get('product')} {meta.get('misc', '')}  on {meta.get('CPUName', '?')} "
          f"({meta.get('physicalCPUs', '?')}c/{meta.get('logicalCPUs', '?')}t), {meta.get('oscpu', '')}")
    dur = (end - zero) if end else max((v.samples[-1][0] for v in views if v.samples), default=0)
    print(f"  profile {dur:.0f} ms, sampling every {interval} ms, format v{meta.get('preprocessedProfileVersion', '?')},"
          f" features {','.join((meta.get('configuration') or {}).get('features', []))}")
    if args.from_ms or args.to_ms != float("inf"):
        print(f"  window: {args.from_ms:.0f} … {args.to_ms:.0f} ms")
    print(f"  {'thread':<26} {'process':<26} {'wall ms':>8} {'cpu ms':>8} {'cpu%':>5} {'wasm%':>6}")
    for v in views:
        cpu = f"{v.cpu:8.0f} {pct(v.cpu, v.wall):4.0f}%" if v.cpu is not None else f"{'-':>8} {'-':>5}"
        print(f"  {v.name[:26]:<26} {str(v.process)[:26]:<26} {v.wall:8.0f} {cpu} {pct(v.wasm_samples, len(v.samples)):5.0f}%")

    if args.thread:
        game = next((v for v in views if args.thread in v.name or args.thread == str(v.tid)), None)
        if game is None:
            sys.exit(f"no thread matching {args.thread!r}")
    else:
        game = max(views, key=lambda v: v.wasm_samples)
        if not game.wasm_samples:
            print("\n  no client.wasm frames in any thread — is this a profile of the web build?")
    game_markers = markers_of(game.thread, strings_of(game.thread), zero, t_end=min(args.to_ms, dur))

    buckets, systems, crates, frames, webgl_api, ipc = analyze_game(
        game, game_markers, cats, libs, args.top, args.from_ms, args.to_ms)

    section("other threads (WebGL executor = CanvasRenderer; Renderer/Compositor = WebRender)")
    other_self = {}
    for v in views:
        if v is not game:
            other_self[v.name + " (" + str(v.process) + ")"] = analyze_other(v, libs, args.top) or {}

    section("markers (intervals within the window: count / total / max ms)")
    for v in views:
        ms = markers_of(v.thread, strings_of(v.thread), zero)
        agg = collections.defaultdict(list)
        for n, s, e, _ in ms:
            if e is not None and args.from_ms <= s <= args.to_ms:
                agg[n].append(e - s)
        rows = sorted(agg.items(), key=lambda kv: -sum(kv[1]))
        if not rows:
            continue
        print(f"\n-- {v.name} ({v.process})")
        print(f"  {'count':>6} {'total':>9} {'max':>8}  marker")
        for n, ds in rows[: min(args.top, 15)]:
            print(f"  {len(ds):6d} {sum(ds):9.1f} {max(ds):8.1f}  {n[:90]}")

    section("memory (malloc counter per process, MiB)")
    pname = {str(v.pid): v.process for v in views}
    for c in d.get("counters") or []:
        smp = c.get("samples") or {}
        ts, counts = smp.get("time") or [], smp.get("count") or []
        if not counts:
            continue
        acc, series = 0, []
        for t, n in zip(ts, counts):
            acc += n
            series.append((t - zero, acc))
        win = [x for x in series if args.from_ms <= x[0] <= args.to_ms] or series
        mib = lambda b: b / 2**20
        peak = max(win, key=lambda x: x[1])
        print(f"  {c.get('name')} {pname.get(str(c.get('pid')), c.get('pid'))}: start {mib(win[0][1]):.1f}"
              f"  peak {mib(peak[1]):.1f} @ {peak[0]:.0f} ms  end {mib(win[-1][1]):.1f}"
              f"  (net {mib(win[-1][1] - win[0][1]):+.1f})")

    section("game log lines (tracing-wasm UserTiming markers)")
    lines = []
    for n, s, e, data in game_markers:
        if n != "UserTiming" or not data:
            continue
        text = data.get("name") or ""
        if data.get("entryType") == "mark" and len(text) < 12:
            continue  # the paired short id marks
        text = text.split(" log.target =")[0].split("\nlog.")[0].strip()
        if args.logs or "[perf]" in text or "ERROR" in text or "WARN" in text:
            lines.append((s, text))
    for s, text in lines[: (None if args.logs else 60)]:
        print(f"  {s:8.0f}  {text[:200]}")
    if not lines:
        print("  none (--logs prints every line)")

    # ---- verdict
    section("verdict")
    total = game.wall
    idle = buckets.get("idle", 0) + buckets.get("idle (no stack)", 0)
    wasm = sum(ms for b, ms in buckets.items() if b.startswith("wasm:"))
    gl = buckets.get("graphics: WebGL call (browser side)", 0)
    js = sum(ms for b, ms in buckets.items() if b.startswith("js:"))
    gc = sum(ms for b, ms in buckets.items() if "GC" in b)
    print(f"  game thread busy {pct(total - idle, total):.0f}% of {total:.0f} ms: wasm {pct(wasm, total):.0f}%,"
          f" WebGL calls {pct(gl, total):.0f}%, JS {pct(js, total):.0f}%, GC/CC {pct(gc, total):.0f}%")
    if frames:
        st_ = [a for a, _ in frames]
        per = sorted(b - a for a, b in zip(st_, st_[1:])) or sorted(dd for _, dd in frames)
        print(f"  {len(frames)} frames, frame time p50 {quantile(per, .5):.1f} ms, max {per[-1]:.0f} ms"
              + ("  → slower than vsync" if quantile(per, .5) > 16.7 else ""))
    render = max((crates[c] for c in RENDER_CRATES), default=0)
    top_sys = [(s, ms) for s, ms in systems.most_common(4) if not s.startswith("<in App")]
    if top_sys:
        print("  heaviest systems: " + "; ".join(f"{short(s, 60)} {pct(ms, total):.0f}%" for s, ms in top_sys[:3]))
    print(f"  largest render crate on the stack: {pct(render, total):.0f}% of the thread"
          f" (wgpu/bevy_render work incl. the WebGL calls below it)")
    sync = {n: ds for n, ds in ipc.items() if ds}
    if sync:
        n, ds = max(sync.items(), key=lambda kv: sum(kv[1]))
        print(f"  sync WebGL IPC: {sum(len(v) for v in sync.values())} round trips, {sum(sum(v) for v in sync.values()):.0f} ms"
              f" (largest: {n} ×{len(ds)}, {sum(ds):.0f} ms) — each blocks the main thread on the GL executor")
    for v in views:
        if v is not game and v.cpu and pct(v.cpu, v.wall) > 50:
            role = ("the WebGL executor — GPU-side work is the bottleneck" if "CanvasRenderer" in v.name
                    else "a Web Worker (the client's sim worker) — it may fall behind" if "Worker" in v.name
                    else "a saturated browser thread")
            print(f"  {v.name} ({v.process}) on CPU {pct(v.cpu, v.wall):.0f}% of the window: {role}")
    blocked = sum(ms for b, ms in buckets.items() if b.startswith("blocked"))
    if blocked:
        print(f"  game thread blocked in sync IPC {pct(blocked, total):.0f}% of the window"
              f" (waiting on another thread/process, not computing)")

    # ---- hints: known costs that cost >= 2% of some thread
    hits = []
    sources = {f"game thread ({game.name})": dict(game_self_names(game))}
    sources.update(other_self)
    for pattern, text in HINTS:
        rx = re.compile(pattern)
        for where, fns in sources.items():
            whole = game.wall
            ms = sum(v for n, v in fns.items() if rx.search(n))
            if ms >= 0.02 * whole:
                hits.append((ms, where, pattern, text))
    if hits:
        print("\n  known costs present:")
        for ms, where, pattern, text in sorted(hits, key=lambda h: -h[0]):
            print(f"  * {ms:.0f} ms on {where} [{pattern}]:")
            print("      " + text)


if __name__ == "__main__":
    main()
