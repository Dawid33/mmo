# Runtime Thread Caps Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Limit the native Bevy client and dedicated server Tokio runtime to four worker threads by default while preserving validated environment overrides.

**Architecture:** Keep configuration local to each binary runtime: the client parses `VOXEL_CLIENT_WORKER_THREADS` and feeds the result into Bevy's existing `TaskPoolOptions`, while the server parses `TOKIO_WORKER_THREADS` and passes it explicitly to Tokio's runtime builder. Pure value parsers make defaults and failures testable without mutating process-global environment variables, and wasm continues to bypass the native client cap.

**Tech Stack:** Rust 2021, Bevy 0.18 `TaskPoolPlugin`, Tokio 1.52 multi-thread runtime, Cargo/Just.

## Global Constraints

- The native client and dedicated server default to four worker threads.
- `VOXEL_CLIENT_WORKER_THREADS` overrides the native Bevy total worker budget and must be an integer greater than or equal to four.
- `TOKIO_WORKER_THREADS` overrides the dedicated server Tokio worker count and must be a positive integer.
- The wasm client retains Bevy's platform-default behavior and does not read native process environment configuration.
- Region, LOD, world-management, client-networking, game-loop, and tick-timer thread topology stays unchanged.
- Preserve all existing uncommitted user changes, especially the render-benchmark and exit-status work adjacent to the client cap code.
- Do not replace any patched vendored dependencies.

## File Structure

- Modify `crates/client/src/main.rs`: validate the client cap, apply the default only on native targets, and retain the existing Bevy task-pool assignment policy.
- Modify `crates/server/src/lib.rs`: validate the Tokio worker count, construct the bounded runtime, and add local unit tests.
- No dependency, asset, `justfile`, or vendored-crate changes are required; the existing render-benchmark recipe already supplies both override variables.

---

### Task 1: Native Bevy Worker Default and Validation

**Files:**
- Modify: `crates/client/src/main.rs:22-62`
- Modify: `crates/client/src/main.rs:218-228`
- Test: `crates/client/src/main.rs:264-282`

**Interfaces:**
- Consumes: `VOXEL_CLIENT_WORKER_THREADS: Option<&str>` and the existing `task_pool_options(worker_cap: Option<usize>) -> bevy::app::TaskPoolOptions`.
- Produces: `client_worker_cap(value: Option<&str>) -> Result<usize, String>` and a native-only four-worker value passed to `task_pool_options`.

- [ ] **Step 1: Write focused tests for the default, override, and validation**

Replace the two existing worker-cap tests with these tests, retaining `bevy_app_errors_propagate_to_the_process_status` unchanged:

```rust
#[test]
fn client_worker_cap_defaults_to_four() {
    assert_eq!(client_worker_cap(None), Ok(4));
}

#[test]
fn client_worker_cap_accepts_valid_override() {
    assert_eq!(client_worker_cap(Some("6")), Ok(6));
}

#[test]
fn client_worker_cap_rejects_invalid_values() {
    let malformed = client_worker_cap(Some("many")).unwrap_err();
    assert!(malformed.contains("VOXEL_CLIENT_WORKER_THREADS"));
    assert!(malformed.contains("integer >= 4"));

    let undersized = client_worker_cap(Some("3")).unwrap_err();
    assert!(undersized.contains("VOXEL_CLIENT_WORKER_THREADS"));
    assert!(undersized.contains("integer >= 4"));
}

#[test]
fn client_worker_cap_limits_combined_bevy_pools() {
    let options = task_pool_options(Some(client_worker_cap(None).unwrap()));
    assert_eq!(options.max_total_threads, 4);
    assert_eq!(options.async_compute.max_threads, 4);
    assert_eq!(options.async_compute.percent, ASYNC_COMPUTE_PERCENT);
}
```

- [ ] **Step 2: Run the client binary tests and verify RED**

Run:

```bash
cargo test -p client --bin client client_worker_cap -- --nocapture
```

Expected: compilation fails because `client_worker_cap` does not exist; the failure is caused by the missing value parser, not by unrelated code.

- [ ] **Step 3: Implement the pure parser and native environment wrapper**

Replace `client_worker_cap_from_env` and `validated_client_worker_cap` with:

```rust
const DEFAULT_CLIENT_WORKER_THREADS: usize = 4;

fn client_worker_cap(value: Option<&str>) -> Result<usize, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_CLIENT_WORKER_THREADS);
    };
    let cap = value.parse::<usize>().map_err(|_| {
        format!(
            "invalid VOXEL_CLIENT_WORKER_THREADS value `{value}` (expected integer >= 4)"
        )
    })?;
    if cap < 4 {
        return Err(format!(
            "invalid VOXEL_CLIENT_WORKER_THREADS value `{value}` (expected integer >= 4)"
        ));
    }
    Ok(cap)
}

#[cfg(not(target_arch = "wasm32"))]
fn client_worker_cap_from_env() -> usize {
    let value = std::env::var("VOXEL_CLIENT_WORKER_THREADS").ok();
    client_worker_cap(value.as_deref()).unwrap_or_else(|message| panic!("{message}"))
}
```

Keep `task_pool_options` unchanged: setting both `max_total_threads` and `async_compute.max_threads` is necessary because Bevy allocates its three pools from one total budget and clamps each pool separately.

- [ ] **Step 4: Apply the cap only to the native client**

Immediately before `app.add_plugins`, define the platform-specific options:

```rust
#[cfg(not(target_arch = "wasm32"))]
let task_pool_options = task_pool_options(Some(client_worker_cap_from_env()));
#[cfg(target_arch = "wasm32")]
let task_pool_options = task_pool_options(None);
```

Then change only the existing task-pool plugin value:

```rust
.set(bevy::app::TaskPoolPlugin { task_pool_options }),
```

This keeps wasm uncapped and avoids reading `std::env` there.

- [ ] **Step 5: Run the client tests and verify GREEN**

Run:

```bash
cargo test -p client --bin client client_worker_cap -- --nocapture
```

Expected: all four `client_worker_cap...` tests pass.

- [ ] **Step 6: Verify the wasm client still compiles**

Run:

```bash
cargo check -p client --target wasm32-unknown-unknown
```

Expected: exit 0; no native environment function is referenced by the wasm build.

- [ ] **Step 7: Review the client diff without staging unrelated hunks**

Run:

```bash
git diff -- crates/client/src/main.rs
```

Expected: the cap-related hunks show a four-worker native default and pure validation. Existing render-benchmark window and exit-status changes remain intact. Do not stage or commit the file wholesale while it contains pre-existing uncommitted user work; defer integration to the final handoff unless the user explicitly authorizes staging those overlapping hunks.

---

### Task 2: Dedicated Server Tokio Worker Cap

**Files:**
- Modify: `crates/server/src/lib.rs:1-18`
- Modify: `crates/server/src/lib.rs:228-237`
- Test: append to `crates/server/src/lib.rs`

**Interfaces:**
- Consumes: `TOKIO_WORKER_THREADS: Option<&str>`.
- Produces: `server_worker_threads(value: Option<&str>) -> Result<usize, String>` and `build_server_runtime(worker_threads: usize) -> tokio::runtime::Runtime`.

- [ ] **Step 1: Write focused server configuration tests**

Append this unit-test module to `crates/server/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_worker_threads_default_runtime_has_four_workers() {
        let worker_threads = server_worker_threads(None).unwrap();
        let runtime = build_server_runtime(worker_threads);
        assert_eq!(runtime.metrics().num_workers(), 4);
    }

    #[test]
    fn server_worker_threads_accept_valid_override() {
        assert_eq!(server_worker_threads(Some("2")), Ok(2));
    }

    #[test]
    fn server_worker_threads_reject_invalid_values() {
        let zero = server_worker_threads(Some("0")).unwrap_err();
        assert!(zero.contains("TOKIO_WORKER_THREADS"));
        assert!(zero.contains("positive integer"));

        let malformed = server_worker_threads(Some("many")).unwrap_err();
        assert!(malformed.contains("TOKIO_WORKER_THREADS"));
        assert!(malformed.contains("positive integer"));
    }
}
```

- [ ] **Step 2: Run the server tests and verify RED**

Run:

```bash
cargo test -p server --lib server_worker_threads -- --nocapture
```

Expected: compilation fails because `server_worker_threads` and `build_server_runtime` do not exist.

- [ ] **Step 3: Implement parsing and bounded runtime construction**

Near the imports in `crates/server/src/lib.rs`, add:

```rust
const DEFAULT_SERVER_WORKER_THREADS: usize = 4;

fn server_worker_threads(value: Option<&str>) -> Result<usize, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_SERVER_WORKER_THREADS);
    };
    let worker_threads = value.parse::<usize>().map_err(|_| {
        format!(
            "invalid TOKIO_WORKER_THREADS value `{value}` (expected positive integer)"
        )
    })?;
    if worker_threads == 0 {
        return Err(format!(
            "invalid TOKIO_WORKER_THREADS value `{value}` (expected positive integer)"
        ));
    }
    Ok(worker_threads)
}

fn build_server_runtime(worker_threads: usize) -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .unwrap()
}
```

The explicit `.worker_threads(...)` is required because Tokio otherwise defaults to every available core. Parsing the conventional environment variable ourselves preserves its override after setting the builder value explicitly.

- [ ] **Step 4: Use the bounded builder in `run`**

Replace the current inline runtime builder with:

```rust
let configured_worker_threads = std::env::var("TOKIO_WORKER_THREADS").ok();
let worker_threads = server_worker_threads(configured_worker_threads.as_deref())
    .unwrap_or_else(|message| panic!("{message}"));
let rt = build_server_runtime(worker_threads);
```

Keep the existing runtime-owning OS thread and `rt.block_on(...)` call unchanged.

- [ ] **Step 5: Run the focused server tests and verify GREEN**

Run:

```bash
cargo test -p server --lib server_worker_threads -- --nocapture
```

Expected: all three `server_worker_threads...` tests pass, including the runtime metrics assertion reporting four workers.

- [ ] **Step 6: Commit the isolated server change**

First confirm the server file had no unrelated pre-existing diff, then run:

```bash
git diff --check -- crates/server/src/lib.rs
git add crates/server/src/lib.rs
git commit -m "perf(server): cap Tokio worker threads"
```

Expected: a narrow server commit containing only parsing, runtime construction, wiring, and focused tests. If a new unrelated server diff appears before execution, do not stage it wholesale; report the overlap instead.

---

### Task 3: Workspace Verification and Handoff

**Files:**
- Verify: `crates/client/src/main.rs`
- Verify: `crates/server/src/lib.rs`
- Verify: `justfile`

**Interfaces:**
- Consumes: the completed client and server configuration changes.
- Produces: formatting, lint, and full-suite evidence; no new runtime interface.

- [ ] **Step 1: Format the workspace**

Run:

```bash
just fmt
```

Expected: exit 0. Formatting may mechanically touch already-modified files; inspect the resulting diff and preserve all user changes.

- [ ] **Step 2: Run formatting and lint checks**

Run:

```bash
just fmt-check
just clippy
```

Expected: both commands exit 0 with no formatting differences or Clippy warnings.

- [ ] **Step 3: Run focused first-party tests**

Run:

```bash
just test
```

Expected: all quick first-party test suites pass.

- [ ] **Step 4: Run required final verification**

Run:

```bash
CARGO_BUILD_JOBS=4 RUST_TEST_THREADS=4 just test-full
```

Expected: exit 0; all quick and ignored slow correctness tests selected by `just test-full` pass.

- [ ] **Step 5: Inspect final scope and report the result**

Run:

```bash
git status --short
git diff --check
git diff -- crates/client/src/main.rs crates/server/src/lib.rs justfile
```

Expected: no whitespace errors; `justfile` is unchanged by this feature; client and server defaults are four with the documented overrides. Report the focused and full verification results, the server commit if created, and explicitly identify the client changes left uncommitted because they overlap the user's pre-existing work.
