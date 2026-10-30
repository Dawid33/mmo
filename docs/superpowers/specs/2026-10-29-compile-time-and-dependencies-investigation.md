# Compile time and dependency reduction — investigation (2026-10-29)

Static investigation only: no timing builds were run (the box was busy with other
cargo jobs at the time, and timing runs need an idle machine). The numbers below come from
`cargo tree`, `cargo metadata`, source greps and artifacts already in `target/debug`.
Ranked by expected payoff for the **edit → build → run** loop, not for clean builds.

## Measured state

| What | Value |
|---|---|
| Packages in the lock graph | 850 |
| Normal deps (unique, linux) | client 701 · slint-ui 259 · server 254 · worldgen 123 · game 121 |
| First-party source | client 40.9k lines (one crate) · game 13.4k · worldgen 5.4k · server 1.3k |
| `target/debug/client` | **2.51 GB, of which 2.28 GB (91%) is DWARF**; `.text` is 89 MB |
| `target/debug/server` | 276 MB, 259 MB (94%) DWARF |
| `slint-ui` generated `out/index.rs` | **9.8 MB / 101.6k lines** (363 components, 2,980 `include_bytes!`) from ~3.8k lines of `.slint` |
| `slint-ui` build-script output dirs | 12 (one per profile/feature/env combination) |
| `target/debug/incremental` | 9.4 GB (client 1.3 GB, slint_ui 0.9 GB, four `game` dirs of 0.4–0.6 GB) |

Observed live while investigating: a `cargo build --bin client --bin server` (play dial,
`REGION_BLOCKS=256`) and a `cargo test -p game` (test dial, 64) were running together in
the same `target/`, and `rustc` spent ~2 min on `slint_ui` alone.

## 1. Debug info dominates linking (config only, biggest loop win)

`[profile.dev] debug = "full"` applies to **every** package, including Bevy, wgpu, naga,
Slint and the rapier/parry forks. As a result each client relink writes ~2.3 GB of DWARF, and
`rustc` emits full debug info for all ~700 dependencies on a clean build.

Options, in increasing order of change:

- **Dependencies at line tables only, workspace crates at full.** Stack traces and `perf`
  symbolization (including inlined frames) still work; you lose variable inspection
  inside dependencies, which nobody does in Bevy internals.
  ```toml
  [profile.dev.package."*"]
  opt-level = 3
  debug = "line-tables-only"
  ```
  CLAUDE.md already says to profile in release / `profiling`, which keeps `debug = "full"`.
- **`split-debuginfo = "unpacked"`** in `[profile.dev]`: the DWARF stays in `.dwo` files next
  to the objects, so the linker doesn't touch it. This is the larger link-time win, but check
  `perf`/hotspot against it first (they follow `.dwo` via the skeleton CU when the paths
  still exist).
- `slint-ui` specifically: see §3.

Expected effect: the client link shrinks from ~2.5 GB of output to a few hundred MB.
This is unmeasured; the first timing run should be `touch crates/client/src/main.rs` →
`just build`, before and after.

## 2. The region-size dial thrashes the shared `target/`

The test/check recipes build with `VOXEL_REGION_BLOCKS=64` and the play recipes with
`256`. Both write `target/debug`. Env vars are part of cargo's *fingerprint*, not its
*metadata hash*, so the two builds overwrite each other's `game` artifact in place.
**Every alternation between `just smoke`/`test-one` and `just run` therefore recompiles
`game` → `worldgen` → `server` → `client`** (the 40.9k-line Bevy crate plus the 2.5 GB
link). Two concurrent cargo commands with different dials also serialize on the
build-directory lock and then invalidate each other, which is what was happening live.

Fix (justfile only): give the play recipes their own target dir, e.g.
`CARGO_TARGET_DIR=target/play`, keyed on the dial when it is overridden
(`target/play-rb{{play_blocks}}`). It costs one extra cold build of the deps and the disk for
it (keep §1 so that copy isn't 25 GB). rust-analyzer already uses its own
`target/rust-analyzer`. The long-term alternative is making `REGION_BLOCKS` a runtime
value, but it feeds const arrays, LOD tables and asserts, so that is a large refactor and is
not worth doing just for build times.

## 3. `slint-ui` compiles ~100k lines of generated Rust

`slint_build` inlines the `widgets` library into every use site. The result is 363
component structs and 9.8 MB of Rust from ~3.8k lines of `.slint`, all built at dev
`opt-level = 1` with full debug info (workspace members don't get the `"*"` override). It is
a leaf crate, so this only bites when it rebuilds. It rebuilds often, though: 12 build-script
variants exist, `widgets/build.rs` reruns on changes to `../ui/.../codegen`, and every
`../ui` edit counts.

- `[profile.dev.package.slint-ui] debug = 0` (and try `opt-level = 0`). The generated
  code is property bindings, and the rasterizer lives in `i-slint-renderer-software`, which is a
  dependency at O3. Cheap to do; check the UI still feels the same.
- Cut what is generated. `ui/index.slint` should export only the screens the game uses, and the
  `widgets` library should avoid re-instantiating large components per use (check whether 363
  structs reflects real screens or library copies). This is upstream work in `~/Projects/ui`.
- Find out why there are 12 build-script variants. Run
  `CARGO_LOG=cargo::core::compiler::fingerprint=info` on the next unexpected `slint_ui`
  rebuild. Likely contributors are dev vs test vs `profiling` vs clippy, plus the asset-viewer
  example.

## 4. Unused and mis-scoped dependencies (cheap hygiene)

**Status (2026-10-30): done** — plan `docs/superpowers/plans/2026-10-30-slint-only-ui-and-unused-deps.md`, Tasks 1–2.

Found by grepping each crate's `src`/`tests`/`examples` for `name::`/`name!`.
Macro-emitted paths were checked separately: `macros`' `quote!` output names `::borrow`,
`::derive_more`, `::crossbeam`, `::log`, `::assert_json_diff`, `::serde_json` and
`::slotmapd`, so `game` must keep those.

| Crate | Remove | Note |
|---|---|---|
| `macros` (proc-macro) | `bincode`, `crc32fast`, `crossbeam`, `log`, `assert-json-diff`, `borrow`, `derive_more` | Imports only `syn`/`quote`/`proc_macro2`; the rest only appear inside `quote!`. Under resolver 2, proc-macro deps are built separately for the host, so these are **extra compiles** of `derive_more`/`borrow` (each with its own proc macro), `crossbeam` and more. |
| `game` | `bytemuck`, `arrayvec`, `either`, `spade`, `foldhash` | No references. `spade` is the heaviest (pulls `robust`, `smallvec`, …). |
| `game` | optional `vec_map`, `web-time`, `slab`, `ordermap`, `hashbrown`, `obj`, `ena` | Never enabled (commented-out features). Not compiled, just noise. |
| `game` | the `[features] default = ["taffy_tree", "flexbox", "grid", …]` block | Copy-pasted from taffy; all empty features. |
| `server` | `papaya`, `slotmapd`, `bincode`, `crc32fast` | No references. |
| `server` | `rcgen 0.13` | One call (`lib.rs:134`). `wtransport` already pulls `rcgen 0.14`; bump to 0.14, or reuse `wtransport::Identity::self_signed` for quinn's cert too. |
| `client` | `bincode` | No references (framing goes through `game::encode_packet`). |
| `bbmodel` | `log` | No references. |
| `crates/block-mesh` fork | `ilattice` → `glam 0.19` | `game` uses only `ndshape` + the `Voxel`/`MergeVoxel` traits; the fork drags a third glam version into the sim crate. Make `ilattice` optional in the fork. |

Removing these speeds up clean builds a little and makes `game` (upstream of everything)
leaner. It won't change the edit loop much on its own. Verify with `just smoke` and
`just determinism-wasm` (the latter because `game`'s deps change).

## 5. Duplicate stacks worth knowing about

**Status (2026-10-30):** the Bevy text/UI stack and egui are gone (Slint is the only UI; Tasks 3–9 of the same plan); client normal deps 701 → 653.

- **Two text/font stacks in the client.** Bevy's (`bevy_text` → parley 0.9, swash,
  harfrust 0.6, skrifa 0.42, fontique 0.9) and Slint's (parley 0.11, skrifa 0.44,
  fontique 0.11). `bevy_ui`/`bevy_text` are still used by `hud.rs`, `minimap.rs`,
  `ui_bridge.rs`, `grid_debug.rs` and picking. The stated direction is all player UI in Slint.
  Once those move, dropping the `bevy_ui`, `bevy_ui_render`, `bevy_text`, `default_font` and
  `ui_picking` features removes a whole stack. egui (the F3 panel) has its own text and
  doesn't need `bevy_text`.
- Three glam versions (0.19 via block-mesh, 0.32 Bevy, 0.33 glamx/rapier); 0.32 vs 0.33 is
  inherent to Bevy vs the physics fork.
- `rand` 0.9 (quinn) + 0.10 (Bevy), `hashbrown` ×4, `getrandom` ×3: inherent, small.
- The Bevy features `ktx2` + `zstd_rust` look unused (no `.ktx2` assets), but
  `tonemapping_luts` needs them, so keep them.

## 6. Structural: the client is one 40.9k-line Bevy crate

Every client edit re-runs incremental codegen for one large crate and then relinks the
2.5 GB binary. Upstream crates make it worse: any `game` or `worldgen` edit invalidates all
of it. (`client` uses `worldgen` only for `local_server.rs`, the minimap, the commander and
render-bench.)

- **Bevy `dynamic_linking` for native dev builds.** This is Bevy's own recommendation
  for iteration: Bevy becomes one `.so`, and the client link handles only first-party code.
  Expose it as a client feature (`dev_fast = ["bevy/dynamic_linking"]`), used by
  `just client`/`run`. Never use it for release, wasm or perf captures. Caveats: binaries need
  the `.so` on `LD_LIBRARY_PATH` (`cargo run` handles it), and the wasm build ignores it.
- **Split `client`** along seams that already exist. `renderer/meshing/` (mesher, records,
  visibility) is nearly engine-free and could be a Bevy-free crate with fast unit tests;
  `voxel_pull/` + `far_voxel/` + `lod_rings.rs` could be a render crate; `netcode*` +
  `local_server.rs` a connection crate; the app crate keeps systems and wiring. Each split
  makes an edit recompile a smaller unit, and moves `worldgen` off the client's critical path.
  This is the most work, so do it after §1–§2 have been measured.

## 7. Smaller knobs

- Linker: rustc ≥ 1.90 already links x86_64-linux with `rust-lld`. `mold` (not installed)
  is usually a bit faster again, but after §1 the remaining gain is small. Try it only if
  linking still shows up in `--timings`.
- `sccache` is installed but not wired (`RUSTC_WRAPPER` unset). It can't cache incremental
  (workspace/path) crates, so it doesn't help the edit loop. It does make §2's second target
  dir and clean builds cheap for registry deps.
- Housekeeping: `crates/game/src/target/rust-analyzer/` is a stray RA build tree (git-ignored,
  488 K). Something ran rust-analyzer with `crates/game/src` as its root. Delete it.

## Suggested order

1. §1 deps at line tables + §3 `slint-ui` `debug = 0` → measure a touched-client rebuild.
2. §2 play target dir → stops the dial thrash.
3. §4 dependency removals (one commit, `just smoke` + `just determinism-wasm`).
4. §6 Bevy `dynamic_linking` dev feature → measure.
5. §5 / §6 split client, and Slint-only UI, as planned work.

Measure each step with `cargo build --timings` (HTML in `target/cargo-timings/`) on an
idle machine, plus a wall-clock time of `touch crates/client/src/main.rs && just build`
and of `touch crates/game/src/lib.rs && just build`.
