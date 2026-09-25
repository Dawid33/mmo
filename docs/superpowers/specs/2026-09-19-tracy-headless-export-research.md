# Tracy as a text data source: headless capture, export, and what it can surface

Handoff report, 2026-09-19. Audience: the agent implementing performance profiling for this
repo. Read `CLAUDE.md` § Profiling first — this report assumes its vocabulary (`just diag`,
`trace-chrome`, `perf-capture`, the `[perf]` join timeline).

Every claim below is tagged **[verified]** (run on this box during the research),
**[source]** (read from Tracy v0.13.1 / Bevy 0.18.1 source, not run), or **[unverified]**.

## 1. Summary

- `just tracy-client` today needs a human at the Tracy GUI. It does not have to: Tracy ships
  two CLI tools — `tracy-capture` (records a `.tracy` file, no GUI) and `tracy-csvexport`
  (turns it into CSV). Both are **built and working** at `~/Software/tracy` (tag `v0.13.1`).
  **[verified]**
- The pipeline was proven end to end with a probe program on the exact crate versions in
  `Cargo.lock`. It has **not** been run against the real Bevy client yet — that needs a
  tracy-featured `profiling` build (full Bevy codegen, many minutes; ask the user first).
- Biggest gap is not tooling but **instrumentation: the first-party crates contain zero tracing
  spans.** Tracy (like `trace-chrome`) currently sees Bevy's own spans only. The sim thread,
  rollback/re-sim, netcode, meshing task bodies and the whole server are invisible. § 5.
- What Tracy adds over the existing text tools: CPU spans + GPU zones + numeric plots + event
  marks **on one nanosecond timeline, from process start**, with low overhead and a compact
  file that a human can also open in the GUI.

## 2. Current state in the repo

`just tracy-client [detailed]` (justfile ~l.612): builds the server (dev) and the client with
`--profile profiling --features tracy`, runs both. Client features
(`crates/client/Cargo.toml`): `tracy = ["bevy/trace_tracy"]`,
`tracy-detailed = ["tracy", "bevy/detailed_trace"]`. Native only — never enable on wasm.

What `bevy/trace_tracy` does **[source]**:

- `bevy_log` installs `tracing_tracy::TracyLayer` on the global subscriber → **every tracing
  span becomes a Tracy zone**; every tracing *event* becomes a Tracy *message*.
- `bevy_log` also runs `LogTracer::init()`, bridging the `log` crate into tracing. The `[perf]`
  timeline (`crates/game/src/perf.rs`) logs via `log::info!`, so on the client those lines
  should land in the trace as timestamped messages with no code change. (Event → message
  **[verified]** with the probe; the log→tracing→Tracy chain in the real client **[unverified]**.)
- `bevy_render` emits one frame mark per frame (`tracy.frame_mark`).
- With `RenderDiagnosticsPlugin` added (this repo: `DIAG=1`,
  `crates/client/src/renderer/diag.rs`), bevy_render creates a **Tracy GPU context** and records
  GPU zones per render pass from the same wgpu timestamp queries `just diag` prints.
- Optional `bevy/trace_tracy_memory` wraps the global allocator so every alloc/free is traced
  (not currently exposed as a client feature; see § 4 for why it is low priority).

Pinned versions: `tracy-client 0.18.4`, `tracy-client-sys 0.28.0` (bundles Tracy **v0.13.1**),
`tracing-tracy 0.11.4`. The protocol is version-locked: tools must be built from the same Tracy
tag. If a Bevy bump moves `tracy-client-sys`, rebuild the tools at the new tag.

Threads, for reading per-thread data: server region threads are named (`region x,z`), LOD
workers are named (`lod-worker-N`); the client's sim/instance threads
(`crates/client/src/instance.rs` ~l.1027/1042, `main.rs` l.92) are **unnamed**
`std::thread::spawn`s — name them, or they show up as bare ids in any profiler.

## 3. The headless pipeline

### Build the tools (done once; already done on this box)

No distro package on Mint 22.3; the GitHub release ships Windows binaries only. Neither CLI tool
needs the GUI dependencies; CMake fetches capstone/zstd etc. via CPM at configure time (needs
network). ~5 minutes total. **[verified]**

```sh
git clone --depth 1 --branch v0.13.1 https://github.com/wolfpld/tracy.git ~/Software/tracy
cd ~/Software/tracy
for t in capture csvexport; do
  nice -n 10 taskset -c 0-15 cmake -B $t/build -S $t -DCMAKE_BUILD_TYPE=Release
  nice -n 10 taskset -c 0-15 cmake --build $t/build --parallel 16
done
# -> ~/Software/tracy/capture/build/tracy-capture
#    ~/Software/tracy/csvexport/build/tracy-csvexport
```

### Capture

`tracy-capture -o out.tracy [-a address] [-p port] [-f] [-s seconds] [-m memlimit%]`

- The profiled program is the network *listener*; `tracy-capture` connects to it and **waits
  until it appears** — so start capture *first*. **[verified]**
- The Rust client is not built `ondemand`, so it buffers every event from process start until
  something connects (Bevy warns: "memory consumption will grow until a client is connected").
  Capture-first therefore records the join/load phase from t=0 — which a hand-attached GUI
  always misses, and which is exactly the phase `trace-join` and the `[perf]` timeline care about.
- Stops on: profiled process exit (saves, exit code 0 **[verified]**), `-s N` seconds elapsed,
  or SIGINT (disconnects and saves **[source]**). `-f` overwrites an existing output file.
- Env on the profiled process: `TRACY_NO_EXIT=1` blocks its exit until buffered data is
  delivered (keeps the tail); `TRACY_PORT=<n>` moves off default 8086 — pair with `-p`.
  **[verified]** Use explicit distinct ports if client and server are ever both instrumented;
  one `tracy-capture` process per profiled process.
- Prints a summary on exit (frames, time span, zone count, trace size) — worth keeping in the
  capture directory. Probe: 122 frames / 240 zones → 1.97 MB.

### Export

`tracy-csvexport [options] out.tracy` → CSV on stdout. **[verified]** unless noted.

| flags | rows | columns |
|---|---|---|
| *(none)* | one per zone name | `name,src_file,src_line,total_ns,total_perc,counts,mean_ns,min_ns,max_ns,std_ns` |
| `-e` | same, **self time** (children excluded) | same |
| `-t <pct>` | adds a percentile + truncated mean **[source]** | `…,percentile_ns,truncated_mean_ns` |
| `-u` | one per zone **event** | `name,src_file,src_line,ns_since_start,exec_time_ns,thread,value` |
| `-u -p` | appends every **plot** sample | `name,,,ns_since_start,,,value` |
| `-g` | one per **GPU zone** event **[source]** | `name,src_file,Time from start of program,GPU execution time` |
| `-m` | **messages** only | `MessageName,total_ns` (ns since start) |
| `-f <substr>` (`-c` case-sensitive) | filters zone names | — |
| `-s <sep>` | changes the separator (zone names can contain commas — use `-s $'\t'`) | — |

Sample (probe, aggregate mode):

```
name,src_file,src_line,total_ns,total_perc,counts,mean_ns,min_ns,max_ns,std_ns
frame,src/main.rs,8,628882870,73.654627,120,5240690,5084322,5531619,96704.181071
work{n=7},src/main.rs,2,3098743,0.362924,17,182279,82371,344753,85419.866535
```

Messages, including a tracing event formatted like the repo's join timeline:

```
MessageName,total_ns
message = [perf] event=probe_mark t_ms=40,436363600
```

## 4. Data catalogue — what this can surface

"Today" = with the existing `tracy` feature and no code change. "CSV" = reachable through
`tracy-csvexport`; "file only" = recorded in the `.tracy` but only the GUI (or a custom
exporter, § 6) can read it.

| Data | Where it comes from | Today? | Reachable | What it answers here |
|---|---|---|---|---|
| **ECS system zones** — one per system run, main + render world, named by system path (first-party systems included, e.g. bridge / meshing-queue / lod_rings systems) | Bevy spans | yes | CSV (agg, `-e`, `-u`) | Which system eats the frame; per-system mean vs **max** (stall detection); same question as `trace-chrome`, lower overhead |
| **Schedule / extract / prepare / queue / render-graph node zones** | Bevy spans | yes | CSV | CPU cost of the render side: extract of the voxel pull path, phase sorting, per-node encode time |
| **Finer ECS/render internals** | `tracy-detailed` (`bevy/detailed_trace`) | yes (opt-in, noisy, slower) | CSV | Only when a normal capture points inside Bevy itself |
| **GPU zones per render pass** | bevy_render Tracy GPU context, needs `DIAG=1` | yes [source] | CSV (`-g`) | Per-pass GPU ms *per frame* (diag gives 2 s averages): GPU hitch detection, shadow vs prepass vs opaque cost, and a second CPU-vs-GPU-bound verdict to cross-check `diag_summary.py` |
| **Frame marks** | bevy_render | yes | **file only** — no frame table in csvexport | Frame-time distribution. Workaround: reconstruct from a per-frame root zone via `-u -f` (find the right zone name from a real capture — [unverified] which one is cleanest), or take frame times from `diag` |
| **Messages (event marks)** | any tracing event; `log::` lines via Bevy's LogTracer | yes on the client [partly verified] | CSV (`-m`) | The `[perf]` join timeline (`connected`, `region_loaded`, `first_near_mesh`, `world_ready`, `main_stall` …) as ns-precise marks *inside* the trace → cut join vs steady state by event, not by a guessed `--skip-ms 5000`; attribute zones to join phases |
| **First-party zones**: sim tick, rollback/re-sim, reconciliation, netcode recv/decode, snapshot apply, meshing task bodies, LOD decode, region tick, LOD builder, worldgen | needs instrumentation (§ 5) | **no — zero spans exist** | CSV once added | The known hot paths per project memory: physics ≈ 99.8 % of sim time, prediction-runaway re-sim cost, first-tick-after-load spike, join meshing burst. None of this is visible to Tracy *or* trace-chrome today |
| **Plots** (numeric time series) | needs `tracy_client::Client::plot` calls | no | CSV (`-u -p`) **[verified]** | Anything now printed in periodic counter lines, but per-sample and on the span timeline: rollback depth / replayed ticks, mispredicts, mesh-queue length, in-flight tasks, pending LOD keys, channel backlogs, entity count, bytes received. Correlate "frame hitched" with "queue was 400 deep" |
| **Sampled call stacks** (whole process, incl. unspanned code and kernel) | Tracy sampling; needs perf access — this box has `perf_event_paranoid = -1` | likely [unverified] | file only | Function-level hot spots. `just perf-client` + `perf_summary.py` already deliver this as text — do not rebuild it on Tracy |
| **Context switches / CPU core usage / thread wake-ups** | Tracy system tracing; usually needs root (tracefs) | [unverified] | file only | "Was the sim thread descheduled during the stall?", task-pool oversubscription. Unique to Tracy but needs a custom exporter |
| **Memory alloc/free events** | `bevy/trace_tracy_memory` | no (feature not wired) | file only | Allocation churn per zone. High overhead, not exported — low priority; `rss.csv` in `perf-capture` covers the coarse question |
| **Lock contention** | Tracy lockable wrappers (C++ only) | no | — | Not available from Rust |
| **Capture summary** | `tracy-capture` stdout | yes | text | Frames, time span, zone count, trace size — sanity line for a manifest |

### Derived analyses a reducer script can compute from the CSVs

1. **Top-N zones by total / self / per-frame ms** (aggregate + `-e`) — the `trace_summary.py`
   table, from a cheaper capture.
2. **Stall table**: zones ranked by `max_ns` and by `max/mean` ratio — one 300 ms call hides in
   a mean. Aggregate mode gives this for free; `-t 99` adds a percentile.
3. **Phase cuts by event**: read `-m`, find `world_ready` / `connection_ready`, then split `-u`
   rows into join vs steady state at that timestamp.
4. **Hitch forensics**: reconstruct frames, pick the worst N, list the zones (and plot values)
   overlapping each — "what was different in the bad frames".
5. **CPU-vs-GPU per frame**: join GPU zone sums (`-g`) against frame durations; flags frames
   that are GPU-bound while the 2 s diag average says CPU-bound, and vice versa.
6. **Per-thread busy time / parallelism**: sum `exec_time_ns` of top-level zones per `thread`
   id over a window (the column is a numeric id, not a name).
7. **A/B regression diff**: two captures → join aggregate CSVs on `name` → Δ mean / Δ per-frame,
   sorted. Compare only within one world scale, adapter and profile (CLAUDE.md rule).

## 5. Instrumentation gap and how to close it

Without first-party spans, the most interesting work is one opaque block (or, on non-Bevy
threads, nothing at all). Suggested first sites, all coarse (per tick / per batch / per task —
**never** per voxel, per chunk-in-a-loop or per entity; same volume contract as `[perf]`):

- client sim thread (`instance.rs`): tick, reconcile/rollback, re-sim loop (with replayed-tick
  count as a plot, not a span field), snapshot apply, netcode drain
- meshing: the body of each `AsyncComputeTaskPool` task (Bevy spans the *system* that spawns and
  polls, not the task body), LOD decode/mesh
- game: `RegionRunner` tick phases (input, physics step, boundary scan, hashing)
- server: region tick, LOD build jobs, worldgen — only if the server gets a Tracy backend

Constraints that shape the mechanism:

- `game` and `server` must stay Bevy-free. `tracing` is engine-neutral but is **not** currently
  a dependency of either (both use `log` only). `crates/game/Cargo.toml` has a commented-out
  `profiling = "1.0"` — the `profiling` crate is a thin macro facade that compiles to nothing
  without a backend feature and can target tracing or Tracy; that, or plain `tracing` spans, are
  the two reasonable choices. Either way spans cost nothing meaningful when no layer is
  installed, and they do not touch simulation state, so determinism is unaffected.
- Spans opened on plain `std::thread`s are picked up by Bevy's global subscriber automatically.
- **Do not put varying values in span fields.** tracing-tracy bakes field values into the zone
  name: `work{n=1}` and `work{n=2}` aggregate as separate rows **[verified]**. That is good for
  Bevy's `system{name=…}`; for anything high-cardinality (tick number, chunk coords) it shreds
  the aggregate table. Put numbers in plots or messages instead.
- The server binary has no tracing subscriber. Instrumenting it means adding one (tracing-tracy
  behind an opt-in feature, like `pyroscope`), pinned to the same `tracy-client` version. Defer
  until the client side proves useful.
- Plots need a direct `tracy-client` dependency behind the client's `tracy` feature (must
  resolve to the same 0.18.x as Bevy's).

## 6. Limits and hazards

- **`-u` is unbounded.** A Bevy client emits roughly 10⁵–10⁶ zones per minute [estimate]; an
  unfiltered dump is hundreds of MB. Same rule as the folded perf stacks: never read it whole —
  aggregate first, `-u -f <zone>` for the few zones worth a timeline, reduce with a script.
- **`-f` is a substring filter on the zone name**, one pattern per run. For several zones, run
  it several times or stream one `-u` pass through the reducer.
- **Zone names contain commas and quotes** (`system{name="a::b"}`) and csvexport does not quote
  fields — use `-s $'\t'` (or another rare separator) and parse accordingly. [source]
- No frame table, no thread names, no sampling/ctx-switch/memory export (see catalogue).
  `csvexport.cpp` is a 531-line program over `tracy::Worker`; a custom exporter for frames or
  context switches is the same shape (link `TracyServer`, iterate `GetFramesBase()` etc.).
  Only worth it if the reconstruct-from-zones route proves inadequate.
- **Observer effect.** Tracy zones are cheap (tens of ns) but tracing-tracy formats fields per
  span, and `tracy-detailed` is explicitly slow. Treat absolute numbers from a tracy build as
  comparable only with other tracy builds; FPS claims still come from `just diag`.
- **Memory growth** if nothing connects: a tracy-featured client with no capture attached
  buffers forever. The recipe must start capture first and fail loudly if the tool is missing.
- **Measure in `profiling`/release, pin the adapter** (`just adapter=6700 …`) and record both in
  the capture directory — an unpinned run on this box can land on the NVK T600 or llvmpipe.
- The `.tracy` file stays a normal trace: keep it next to the CSVs so the user can open the same
  capture in the GUI.

## 7. Suggested implementation order

1. **`just tracy-capture [seconds]` recipe** (keep the justfile in sync — CLAUDE.md rule):
   `tracy_bin` variable defaulting to `~/Software/tracy`, overridable like `cargo`/`adapter`;
   check the binaries exist; build server (release) + client (`--profile profiling --features
   tracy`) under `{{cap}}`; start server; start
   `tracy-capture -f -o target/tracy/<utc>/client.tracy -s N`; run the client with
   `TRACY_NO_EXIT=1 DIAG=1 PERF_LOG=1`; when capture exits, run csvexport (aggregate, `-e`,
   `-g`, `-m`) into the same directory. Reuse `perf_capture.sh`'s conventions
   (`PERF_CAPTURE_SECONDS`, manifest with git rev / adapter asked+selected / world scale).
2. **First real capture** — this is the verification step that has not happened. Confirm: GPU
   zones present with `DIAG=1`; `[perf]` lines arrive as messages; which zone is the clean
   per-frame root; actual `-u` size per minute; whether sampling data is recorded.
3. **`scripts/tracy_summary.py`**, sibling of `trace_summary.py` (same flags where they apply:
   `--top`, `--min-ms`, `--skip-ms`, `--until-ms`, plus `--until-event world_ready`):
   analyses 1–3 of § 4 first, then 4–5.
4. **First-party spans + plots** (§ 5), client sim thread first — that is where Tracy stops
   duplicating `trace-chrome` and starts showing things no current tool shows.
5. Optional: fold into `just perf-capture` as an opt-in (`PERF_CAPTURE_TRACY=1`), A/B diff
   mode, server backend, custom frame exporter.

Verification budget per CLAUDE.md: `just smoke` for code changes; a tracy-featured client build
is a slow opt-in — tell the user before starting one. Live runs: server + one client.

## 8. How Tracy relates to the existing text tools

| Need | Use |
|---|---|
| CPU-vs-GPU verdict, fps, per-pass GPU averages | `just diag` (keep as the source of truth for FPS) |
| Function-level CPU hot spots, any thread, no instrumentation | `just perf-client` / `perf-server` + `perf_summary.py` |
| Join timeline, RSS, GPU busy, manifest | `just perf-capture` |
| Per-system table, zero extra tooling | `just trace-chrome` / `trace-join` |
| **Per-frame** GPU+CPU on one timeline, event-cut phases, hitch forensics, plots correlated with spans, join from t=0 at low overhead, human-openable capture | **Tracy headless (this report)** |

## Sources

- Tracy v0.13.1: `capture/src/capture.cpp`, `csvexport/src/csvexport.cpp`, `cmake/vendor.cmake`
  — https://github.com/wolfpld/tracy/tree/v0.13.1
- `tracy-csvexport` manpage — https://manpages.ubuntu.com/manpages/questing/man1/tracy-csvexport.1.html
- Bevy 0.18.1 (local registry): `bevy_log/src/lib.rs` (TracyLayer, LogTracer, frame-mark
  filter), `bevy_render/src/renderer/mod.rs` (frame mark), `bevy_render/src/diagnostic/`
  (`tracy_gpu.rs`, GPU context)
- Probe program: tracy-client 0.18.4 + tracing-tracy 0.11.4, captured on ports 18086/18087
  with the locally built v0.13.1 tools
