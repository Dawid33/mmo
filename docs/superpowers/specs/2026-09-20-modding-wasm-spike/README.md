# Throwaway spike sources for 2026-09-20-modding-wasm-research.md

Not part of the workspace (each crate has its own `[workspace]` table); kept only so the numbers
in the report can be re-run. `fw/` = load-time instrumentation tool (NaN canon, finite-wasm gas +
stack, inline-gas post-pass). `wasmi-spike/guest` = the mod guest, `host` = wasmi native +
wasmi-in-wasm32 (spike 1, `bench.mjs`), `host2` = same bytes on wasmi 2.0 / 1.1 / wasmtime 48,
`webhost` + `bench2.mjs` = browser topology on V8 (spike 2). Build commands are in the report's
appendices A and C; prefix builds with `nice -n 10 taskset -c 0-15`.
