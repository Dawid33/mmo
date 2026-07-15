# Configurable Cargo Justfile Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every Cargo command executed by a root `justfile` recipe honor the configurable `cargo` variable.

**Architecture:** Perform a command-token-only replacement in the existing root `justfile`; comments and examples remain unchanged. Verify the invariant statically and verify Just's rendered commands dynamically using a sentinel executable override.

**Tech Stack:** Just, Bash, ripgrep

## Global Constraints

- Replace each executable `cargo ...` occurrence in the root `justfile` with `{{cargo}} ...`.
- Preserve prose, comments, installation examples, and the variable's stable-Cargo default.
- Do not change Rust source, Cargo manifests, dependencies, profiles, target directories, or recipe behavior beyond Cargo executable selection.

---

### Task 1: Route All Recipe Cargo Commands Through the Variable

**Files:**
- Modify: `justfile`
- Test: static and dry-run shell assertions against `justfile`

**Interfaces:**
- Consumes: the existing Just variable `cargo := env_var_or_default("CARGO_CLIF", "cargo")`
- Produces: every executable Cargo command rendered from a recipe starts with the value of `{{cargo}}`

- [ ] **Step 1: Run the invariant check and verify it fails**

Run:

```bash
if rg -n '^\s+cargo\s' justfile; then
    echo 'FAIL: bare Cargo recipe commands remain'
    exit 1
fi
```

Expected: FAIL after listing the existing bare Cargo commands, proving the check detects the current inconsistency.

- [ ] **Step 2: Replace each bare executable Cargo token**

For every recipe command matched by `^(\s+)cargo(\s)`, apply this exact token
substitution:

```text
search:  ^(\s+)cargo(\s)
replace: $1{{cargo}}$2
```

This includes `build-stable`, web, web-release, quick and full tests, benchmarks, formatting, Clippy, and cleanup. Do not alter comment/example occurrences of `cargo`.

- [ ] **Step 3: Run the invariant check and verify it passes**

Run:

```bash
if rg -n '^\s+cargo\s' justfile; then
    echo 'FAIL: bare Cargo recipe commands remain'
    exit 1
fi
```

Expected: exit 0 with no output.

- [ ] **Step 4: Verify representative rendered recipes use the override**

Run:

```bash
for recipe in build-stable web web-release test test-full keepup-budget soak fmt fmt-check clippy clean; do
    output=$(just --dry-run cargo=sentinel-cargo "$recipe" 2>&1)
    if printf '%s\n' "$output" | rg '^\s*cargo\s'; then
        echo "FAIL: $recipe rendered bare cargo"
        exit 1
    fi
    if ! printf '%s\n' "$output" | rg -q 'sentinel-cargo'; then
        echo "FAIL: $recipe did not render sentinel-cargo"
        exit 1
    fi
done
```

Expected: exit 0 with no failure messages.

- [ ] **Step 5: Validate Just syntax and whitespace**

Run:

```bash
just --list >/dev/null
git diff --check -- justfile
```

Expected: both commands exit 0 with no errors.

- [ ] **Step 6: Commit the workflow change**

```bash
git add justfile docs/superpowers/plans/2026-07-15-configurable-cargo-justfile.md
git commit -m "fix(justfile): honor configurable Cargo everywhere"
```
