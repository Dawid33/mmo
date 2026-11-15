---
name: subagent-worktree-pinning
description: Subagents spawned while the session is in a git worktree may still run with cwd pinned to the main checkout — dispatch prompts must mandate absolute paths + a pre-commit branch check
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 327717c8-a557-401d-afc6-3c5890bd3cd9
---

During subagent-driven execution in a `.claude/worktrees/*` worktree, an implementer subagent ran its commands (including `git commit` and a bare `git stash`) in the MAIN checkout and committed onto the user's `develop` branch, despite the dispatch saying "Work from: <worktree path>".

**Why:** Subagent working directories can be pinned at launch to the original repo root; a prose "work from" line is not enough.

**How to apply:** Every implementer/fixer dispatch must include a hard gate: use absolute paths everywhere; before the first `git commit`, verify `git rev-parse --show-toplevel` prints the worktree path and `git branch --show-current` prints the expected branch, else STOP and report BLOCKED. Forbid bare `git stash` (stash stack is shared across worktrees — see CLAUDE.md session guidance). After each subagent reports, the controller verifies `git log` in the worktree actually advanced. Recovery if it happens again: `git -C <main> reset --keep <expected-tip>` then cherry-pick the stray commit into the worktree branch.
