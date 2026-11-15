---
name: subagent-edit-diagnostics-transient
description: "new-diagnostics after a subagent edit are usually stale mid-edit LSP snapshots; verify with the real compiler, trust neither them nor the report"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 28e57965-868f-4ae5-b52f-ebaed7339f28
---

During subagent-driven implementation, the `<new-diagnostics>` block that arrives after an implementer/fixer finishes almost always shows alarming errors ("cannot find type X", "no method named Y", type mismatches across several files) that are **transient mid-edit LSP snapshots** — captured while the subagent was partway through a multi-file edit, not the committed state. In the vertical-chunk-stacking run every single task triggered these; the committed code was clean every time.

**Why:** rust-analyzer indexes intermediate saves; long subagent runs (900s / 94 tool-uses) leave many stale snapshots. rust-analyzer also reports false "private field" errors on in-crate access that the real compiler accepts.

**How to apply:** after any subagent edit, do NOT react to `new-diagnostics` and do NOT trust the subagent's "DONE, green" report either (see [[subagent-derails-on-bg-notification]]). Run the authoritative check yourself in the worktree: `cargo build --workspace --bins` and the focused `cargo test -p <crate> --lib <module>`. If those are clean, the diagnostics were stale. Match the diagnostic's line numbers against the final file — if they don't correspond, it's a stale snapshot. Also verify branch + commit with `git log` per [[subagent-worktree-pinning]].
