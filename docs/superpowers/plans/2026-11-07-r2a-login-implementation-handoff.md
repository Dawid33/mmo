# Handoff: implement R2a — login service, accounts, client login (subagent-driven)

**Date:** 2026-11-07 · **For:** a fresh session that executes the plan
**Status:** plan written, decisions settled with Dawid, self-reviewed. Nothing implemented yet.
R2b (`2026-11-07-r2b-role-split.md`) depends on this plan and starts only after it is done.

## Your task

Execute `docs/superpowers/plans/2026-11-07-r2a-login-accounts.md`, Tasks 1–11, in order, with
**`superpowers:subagent-driven-development`** (Dawid chose subagent-driven when the plan was
written): a fresh implementer subagent per task, a fresh reviewer per task, a fix loop until the
reviewer passes it, then a final whole-branch review on the most capable model.

- Keep the SDD ledger at `.superpowers/sdd/2026-11-07-r2a-login-accounts/progress.md`
  (git-ignored scratch, like the R1 ledgers next to it): per task the brief, the commit(s), the
  review verdict and anything deferred.
- Task order matters: 1 → 2 → 3 → 4 → 5 → 6 → 7; 8 (other repo) before 10; 9 before 10; 11 last.
  Tasks 3/4 and 8 could run in parallel, but don't: one shared checkout, shared build cache.

## Read first, in this order

1. **The plan:** `docs/superpowers/plans/2026-11-07-r2a-login-accounts.md`. Its "Decisions
   settled in planning", "Global Constraints" and "Review Focus" apply to every task. Paste the
   Global Constraints and the task's Interfaces block into every implementer brief.
2. **The design:** `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md`
   §0.7(a) (auth + JWT), §0.9(b) (accounts global, players per world), §0.10 (sessions per
   `(account, world)`). Skip §0.11 (rate limiting — deferred).
3. **`CLAUDE.md`**: build/test rules (`just test-one`, `just smoke`, the 80 % CPU cap, slow-test
   tag); the "Adventure survival" player-identity paragraph (`PlayerToken`, `Hello`,
   `PlayerStore`, takeover); `crates/server` world store; `crates/client` session / netcode /
   WASM / Slint frontend; `crates/slint-ui`.
4. The world-store crate as the pattern for the login database (dual engine, migrations,
   `schemas_match`): `crates/world-store/src/open.rs`, `crates/world-store/tests/schema.rs`.

## Decisions to carry (don't re-open)

- Email: **Amazon SES v2 API** behind a `Mailer` trait; `LogMailer` for dev/tests. **No test
  and no dev run sends real mail** — `SesMailer` only with `VOXEL_MAILER=ses`, and ask Dawid
  before any run that sets it.
- Unverified accounts may log in (assumption flagged in the plan; `VOXEL_LOGIN_REQUIRE_VERIFIED`
  switch). If Dawid says otherwise during execution, flip the default — nothing else changes.
- GDPR delete/export: R5. Panel: its own plan. Rate limiting: deferred.
- Single-player and `just run`: **no login**, token `Hello`, unchanged.
- Revocation kick and `SpawnPlayer` push are **R2b**, not here.

## Facts checked on 2026-11-07 (develop `d4c27ac`), so subagents don't re-derive them

- `ClientPacket::Hello { token: PlayerToken }` — `crates/game/src/protocol.rs:743` (first
  variant, l.737). `ServerPacket::Refused(String)` l.876 is the only identity refusal.
- `PlayerToken(pub String)` 32 lowercase hex, `PlayerId(pub u64)` — `crates/game/src/player.rs:21,33`.
- Server edge: `crates/server/src/player_store.rs` — `PlayerStore::resolve` (l.28-32),
  `resolve_hello` (l.49-70, `HELLO_TIMEOUT` 10 s, `HELLO_MAX_BYTES` 4096), `MemoryPlayerStore`,
  `StorePlayers`. QUIC hello `crates/server/src/lib.rs:419-434` (`quic_hello`), refusal
  l.437-447, construction l.496-500; WebTransport `crates/server/src/webtransport.rs:130,
  175-184, 225-258`. The manager never sees `Hello` (`world_manager.rs:1336`).
- Newest-wins takeover already exists: `WorldManager::client_connected`
  (`crates/game/src/world_manager.rs:2585-2624`, `LOGGED_IN_ELSEWHERE` l.485).
- Client: `crates/client/src/player_token.rs` (file `$XDG_DATA_HOME/mmo/player_token` 0600,
  wasm `localStorage["mmo.player_token"]`); Hello sent in `netcode.rs:229-234` (native) and
  `netcode_web.rs:228-231` (wasm); `ServerConnection.token` l.73. **The client has no HTTP
  client today** (only wasm `fetch_with_str` in `web_worker/mod.rs:177`); native `tokio` has
  only the `time` feature; `web-sys` lacks `Request`/`RequestInit`/`Headers`.
- Frontend: `crates/slint-ui/ui/frontend.slint` screens 0–4 (`screen` property l.20,
  `back-button-x/y` per-screen ternary l.55-56); `main-menu.slint` join-only flow l.77-98;
  `crates/client/src/frontend/mod.rs` (`FrontendAction` l.36, `JOIN_ONLY` l.53, `screen_index`
  l.65-72, wiring l.134-222); `AppState` `crates/client/src/session.rs:41`. Frontend tests:
  `crates/slint-ui/tests/frontend.rs`.
- The widgets' `TextField` (`../ui/crates/widgets/ui/textbox.slint:74`) has only `text`,
  `enabled`, `accepted` — no password mode (Task 8 adds it, in **that** repo).
- Workspace deps: no `axum`, `reqwest`, `jsonwebtoken`, `argon2`, AWS crates yet; `sqlx 0.8`
  (runtime-tokio, sqlite, postgres, migrate, macros-for-migrate-only) and `quinn`/`rcgen` exist.

## Traps (from earlier SDD runs in this repo — put the relevant ones in each brief)

- **No worktrees.** Subagents work in the main checkout on `develop`. Give them absolute paths
  and have them check `git branch --show-current` == `develop` before committing.
- **Dawid's uncommitted work is in the tree** (CLAUDE.md, justfile, client files, slint, deploy).
  Implementers `git add` only their task's files; CLAUDE.md / justfile hunks by index-only
  staging (`git show HEAD:<f>` + the hunk → `git hash-object -w` → `git update-index
  --cacheinfo`). Check `git diff --cached --stat` before every commit.
- **Subagents derail when a background command notifies them** — tell implementers not to run
  anything in the background; after each task, verify the real `git log` / `git show --stat`
  yourself, don't trust the report.
- **Post-edit diagnostics from a subagent are often stale LSP snapshots** — trust only `just`
  test/smoke output.
- **Client test binaries are an explicit opt-in** (Bevy-sized link). Tasks 9/10 name
  `just test-one client "--lib …"` runs — ask Dawid once before the first one; `just smoke`
  type-checks the client otherwise. wasm: `cargo check -p client --target
  wasm32-unknown-unknown` prefixed with `nice -n 10 taskset -c 0-15`, and `just slint-wasm`.
- **Postgres halves** need `just pg-up` (compose on 127.0.0.1:55432); add `test-login-pg`
  mirroring `test-pg`.
- **Task 8 commits in `~/Projects/ui`**, not here; its test lands here with Task 10. Record the
  `../ui` commit hash in the ledger.
- Argon2 at default cost is slow: tests use `Hasher::for_tests()`; tag anything >100 ms slow.
- Never print or log refresh tokens, passwords or whole JWTs; never commit keys
  (`target/dev-login/` is git-ignored via `target/`).

## Model / review guidance

- Implementers: the standard implementer model is fine for Tasks 1, 4, 6, 8, 11; use the most
  capable model for 2 (crypto), 5 (API semantics: rotation/reuse, no enumeration), 7 (edge
  auth), 10 (Slint + Bevy state flow).
- Reviewers: check each task against its Interfaces block (names/types exactly — later tasks
  depend on them), its Review Focus tests, and the Global Constraints (no sqlx/AWS in client;
  `login-api` builds for wasm32; secrets).
- Final whole-branch review: the five Review Focus lines end to end, plus "single-player
  unchanged" (`just run` code path never touches login).

## Done means

- All 11 tasks committed on `develop`, ledger complete, final review findings fixed or recorded.
- `just smoke`, `just test-one login "--test it"`, `just test-one auth "--test it"`,
  `just test-one server "--test it auth_edge::"`, the slint-ui `login_screen::` tests pass.
- Task 11's live check run **only with Dawid's OK** (one client, `adapter=Intel`), results in
  `TODO.md`.
- Report to Dawid: commits, what was deferred, anything that changes R2b's starting
  assumptions (then update R2b's plan before it starts).
