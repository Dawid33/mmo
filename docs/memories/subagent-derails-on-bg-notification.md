---
name: subagent-derails-on-bg-notification
description: implementer subagents that launch a background command tend to derail at the final commit/report step; verify their git state and finish it yourself
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 79a844a4-e827-425c-81e8-b8b2c80ee746
---

During subagent-driven-development in this environment, implementer subagents repeatedly derailed at the very end (2 of 3 tasks): after doing the work they launched a background command (e.g. `cargo test --workspace`) and returned a stray message like "waiting for the background command to complete; will resume when notified" — WITHOUT committing or writing their report. The work was correct and on disk, just uncommitted, and the returned "status" was garbage.

Separately, injected `<new-diagnostics>` blocks were unreliable/contradictory (they mixed a subagent's mid-work RED-phase state with post-edit state across files) — don't trust them over the actual repo.

**Why:** background-task notifications firing mid-run appear to knock the subagent off its finish sequence; and rust-analyzer diagnostics lag/mix worktree vs main-checkout state.

**How to apply:** Never trust a subagent's self-reported DONE/status or the injected diagnostics. After each subagent, independently check the real state: `git -C <worktree> log/status`, grep the actual files, and run the build/tests yourself. If the work is complete but uncommitted (derailed), just commit it yourself and reconstruct the report rather than re-dispatching. Tell subagents explicitly to prefer foreground verification and to commit+report before ending. Related: [[subagent-worktree-pinning]].
