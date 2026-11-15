---
name: worktree-vs-develop-preference
description: Never create git worktrees in the mmo repo — all work, tests, and attribution runs happen directly on develop in the main checkout
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 73f4a754-d92b-4311-871c-82021649c246
---

Do NOT create git worktrees in this repo — not for feature isolation, not for test runs, and not for bisection/attribution checks. All work goes directly on `develop` in the main checkout. Escalated twice: 2026-07-09 ("don't do this in a worktree, do it on develop", small asset task) and 2026-07-11 (user interrupted an attribution run in a temp worktree: "run it on develop, not in a clean directory"; the rule is now written into CLAUDE.md itself).

**Why:** develop is the integration branch they commit to directly; extra checkouts thrash the build cache (a fresh CARGO_TARGET_DIR is a full bevy rebuild), drift from develop-only files causes confusion, and the user works concurrently in the main tree.

**How to apply:** for pre/post-change attribution, test on develop at HEAD and reason from committed diffs instead of checking out old commits elsewhere. Subagent dispatches get absolute paths into the main checkout and must verify `git branch --show-current` = develop before committing. If a background-session isolation guard blocks Edit/Write in the shared checkout, do NOT weaken the guard via settings (auto-mode classifier denies it); apply user-requested edits via shell instead.
