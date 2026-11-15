# R2a — Login service, accounts and client login Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A central login service (email + password, Argon2id, EdDSA JWT, JWKS, Amazon SES mail, world registry, per-`(account, world)` sessions) and a client that logs in over HTTPS (native and browser) and joins a world's server with a login-issued JWT instead of a bare `PlayerToken` — while single-player keeps working with no login service at all.

**Architecture:** Three new crates: `login-api` (wire DTOs, wasm-safe, serde only), `auth` (JWT signing / verification + JWKS, native only) and `login` (the axum service + `login` binary with admin subcommands, its own SQLite/Postgres database). The game server gains an auth mode: `VOXEL_AUTH=token` (today, single-player and dev) or `VOXEL_AUTH=jwt` (verifies the JWT at the QUIC / WebTransport edge against the login service's JWKS, then resolves the JWT's `player_token` through the existing `PlayerStore`, so the world store and the manager's newest-wins takeover are unchanged). The client gains a login screen and a world picker (Slint, native + wasm) and an HTTP client (`reqwest`, which uses `fetch` on wasm).

**Tech Stack:** Rust 2024 for the new crates; axum 0.8 + tower-http (CORS); sqlx 0.8 (runtime queries, SQLite + Postgres, existing workspace dep); argon2 0.5; jsonwebtoken 9 (EdDSA) + ed25519-dalek 2 (key generation, PKCS#8 PEM); aws-config + aws-sdk-sesv2 (login crate only); reqwest 0.12 (rustls, json; wasm fetch backend) in client and server; Slint (`crates/slint-ui`, widgets in `../ui`).

**Spec:** `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md` §0.4(d), §0.6, §0.7(a), §0.8 q8–q12/q19, §0.9(a)(b), §0.10 — and the handoff `docs/superpowers/plans/2026-11-07-r2-role-split-planning-handoff.md`. Companion plan: `2026-11-07-r2b-role-split.md` (coordinator / host split, owner epochs, `SpawnPlayer` push, revocation kick).

## Why two plans

R2 as specified is ~20 tasks across two subsystems that share only one seam (the `Hello` credential): login/accounts/client UI, and the coordinator/host split with owner epochs. R2a ships on its own — a single `server` process verifying login JWTs — and R2b builds the process split and the login→host push on top of it. Order: R2a first (user's choice, 2026-11-07).

## Decisions settled in planning (user, 2026-11-07, plus the defaults the handoff fixed)

1. **Email: the Amazon SES v2 API** (`aws-sdk-sesv2` `SendEmail`), behind a `Mailer` trait with a `LogMailer` (dev / tests: logs the link, keeps sent mail in memory). Verification and password reset are built in R2a. **Assumption (recommended default, not confirmed):** an unverified account may log in; `VOXEL_LOGIN_REQUIRE_VERIFIED=1` refuses login until verified. AWS credentials come from the default chain (`AWS_PROFILE` / env), region `AWS_REGION` (eu-central-1 like the Orion buckets); never committed.
2. **GDPR deletion / data export: deferred to R5.** R2a stores emails (accounts are needed) but builds no delete / export; the account schema must not make deletion harder (personal data only in `accounts` / `email_tokens`; world-side rows hold no personal data).
3. **The management panel is its own plan.** R2a's admin surface is the `login` binary's subcommands (`world add|list|remove`, `account set-password|verify`), plus the existing `worldctl forge`.
4. **Single-player needs no login:** the client-spawned server runs `VOXEL_AUTH=token` (the default) with today's token-only `Hello`; it works with no login service running and offline. A `jwt` server refuses a token credential, and a `token` server refuses a JWT credential (`Refused` with a message naming the mode).
5. **JWT lifetime / checking (§0.8 q9 default):** the JWT is checked only at connect (and, in R2b/R3, reconnect and cross-host handover). The join JWT lives `JOIN_JWT_TTL_SECS = 300`; a reconnect asks login for a fresh one with the refresh token. Revocation is a login→host kick — that needs the internal link and lands in **R2b**; in R2a logout / reset revoke refresh tokens only.
6. **Signing:** EdDSA (Ed25519). Login holds private keys (PKCS#8 PEM files in `VOXEL_LOGIN_KEYS_DIR`, newest = signing key, `kid` = first 16 hex of SHA-256 of the public key); hosts fetch the public keys from `GET /.well-known/jwks.json`, cache them, refetch on an unknown `kid` at most every `JWKS_REFETCH_MIN_SECS = 30`. `login keygen` writes a new key; rotation = add a key, keep the old one in the dir until its tokens expired.
7. **Claims** (`login_api::JoinClaims`): `sub` (account id as decimal string), `world` (registry world id), `player_token` (hex, **not a secret any more** — §0.7(a)), `session_epoch` (u64), `aud = "mmo-host"`, `iat`, `exp`, `kid` in the header. A host checks signature, `exp` (leeway `JWT_LEEWAY_SECS = 30`), `aud`, `world == VOXEL_WORLD_ID`, and that `session_epoch` is not older than the newest epoch it has seen for that `player_token` (in memory; R2b replaces it with the pending `SpawnPlayer`).
8. **Accounts are global; players per world** (§0.9(b)). `world_players(account_id, world_id) → player_token` is created at the first join to a world (16 random bytes, hex — `game::PlayerToken::generate`); the world's own store keeps turning that token into a `PlayerId` (`resolve_player`, unchanged). **One account may play several worlds at once, once per world** (§0.10.4): `sessions(account_id, world_id, session_epoch)`; every join bumps the epoch; the newest session wins in that world — the host's existing takeover (`client_connected` → `Refused(LOGGED_IN_ELSEWHERE)` + park) does it, and the epoch check refuses a late connect with an older ticket.
9. **World registry** lives in the login database (`worlds`): id (slug), display name, status (`open`/`closed`), QUIC address (`host:port`), WebTransport URL, optional WebTransport cert hash (dev self-signed). Server worlds' listing comes from it; `world.json` / `preview.png` stay single-player menu files only.
10. **Refresh tokens:** 32 random bytes, base64url, stored as SHA-256 hash, `REFRESH_TTL_DAYS = 30`, rotated on every `/v1/refresh` (old one revoked; reuse of a revoked token revokes the whole family = all of the account's refresh tokens). Client stores it: native file `$XDG_DATA_HOME/mmo/login_refresh` (0600, like `player_token.rs`), browser `localStorage["mmo.login_refresh"]`.
11. **Passwords:** Argon2id, `argon2` crate defaults (m=19 MiB, t=2, p=1), PHC string stored; length `PASSWORD_MIN_CHARS = 10` ..= `PASSWORD_MAX_CHARS = 128`. Emails trimmed + lowercased, `EMAIL_MAX_CHARS = 254`, must contain exactly one `@` with non-empty sides. Login failure answers one message for unknown email and wrong password (no account enumeration); register of an existing email answers success and mails "you already have an account" instead (no enumeration).
12. **Rate limiting is out of scope** (user deferred §0.11 out of R0–R5). Don't make it harder: every handler goes through one axum `Router` so a tower layer can be added later.
13. **Login database:** its own database (Postgres in servers — a logical DB beside the worlds', e.g. `postgres://…/mmo_login`; SQLite file for local dev and tests, default `target/dev-login/login.sqlite`), `VOXEL_LOGIN_DB`. Same dual-engine rules as `world-store`: two migration sets kept column-for-column in step, runtime `sqlx::query()`, shared type subset, a `schemas_match` test.
14. **Transport:** the login service speaks plain HTTP on `VOXEL_LOGIN_BIND` (default `127.0.0.1:8790`); TLS is the Ingress's job (R5). The browser build's page and the login service differ in origin in dev, so CORS allows `VOXEL_LOGIN_CORS_ORIGINS` (comma list; dev default `http://127.0.0.1:1334`).
15. **Where the client finds login:** native `VOXEL_LOGIN_URL` (runtime env), else the build-time `option_env!("VOXEL_LOGIN_URL")`; none = no "Play online" button (today's menu). Browser: `?login=<url>` overrides, else the build-time value, else the page origin + `/login` when the page is `https:` on a real host, else none — then the wasm menu stays today's join-only dev menu.

## Global Constraints

- Work on `develop` in the main checkout. **Never create a git worktree** (CLAUDE.md), including subagents. The tree holds the user's uncommitted work: `git add` only a task's own files; for CLAUDE.md / justfile hunks use index-only staging (HEAD copy + your hunk → `git update-index --cacheinfo`). The `../ui` widgets change (Task 8) is committed in **that** repo.
- `game` stays Bevy-free, IO-free, sync, wasm-safe. `login-api` is serde-only and wasm-safe. The client never depends on `world-store`, `sqlx`, `auth`, `login` or AWS crates.
- Wire framing on game edges stays `game::encode_packet` / `decode_packet`; the HTTP API is JSON (serde) — it is not a game edge.
- No compile-time database: runtime `sqlx::query()` only; both engines' migrations + `schemas_match`.
- Secrets are never committed: signing keys, refresh tokens, AWS credentials, SMTP/SES config. Dev keys go under git-ignored `target/dev-login/`. Never log a refresh token, a password or a whole JWT (log `kid` + `sub` only).
- Never weaken the server state-hash check; nothing here touches sim state.
- Verification: `just test-one <crate> "<args>"`, `just smoke`, `just test-pg` style recipes for Postgres halves; never whole suites; client test binaries are an explicit opt-in — the client-side tasks use `--lib` filters and say so. Tests >100 ms get `#[ignore = "slow: >100ms — run via just test-full"]` (Argon2 hashing at default cost is ~50–100 ms: tests use `Hasher::for_tests()` with m=8 KiB, t=1).
- Ask before live runs (one client, `adapter=Intel`) and before anything that sends real email.
- Keep the justfile in sync: `login`, `login-keygen`, `login-world-add`, `login-world-list`, `dev-login` (login + a `jwt` dev server), `test-login-pg`.
- Every new tunable is a named constant (user preference).

## Review Focus

1. **A connect with an expired / wrong-world / wrong-signature / unknown-`kid` / older-epoch JWT** is refused at the edge with `Refused(<reason>)` and never reaches the manager — Task 7 tests each.
2. **Two sessions of one account in the same world:** the second join bumps the epoch; the first client is taken over (`LOGGED_IN_ELSEWHERE`, parked once), and the first ticket, if it connects late, is refused as superseded — Task 7 (edge) + Task 5 (epoch bump).
3. **One account in two worlds at once:** joins to world A and world B issue independent tickets and both stay valid; neither bumps the other's epoch — Task 5.
4. **Single-player and `just run` with no login service:** token mode unchanged; a JWT credential to a token server is refused with a message, not a panic — Task 7 + the smoke type-check.
5. **The browser logs in and joins over WebTransport:** CORS preflight passes for the configured origin and fails for others; the wasm client sends `Credential::Jwt` on WebTransport — Task 5 (CORS test), Task 10 (wasm build check), live check in Task 11.

---

## File structure

```
crates/login-api/            NEW  wasm-safe DTOs + constants
  Cargo.toml                       (serde, serde_json only)
  src/lib.rs                       requests/responses, JoinClaims, JoinTicket, ApiError, paths, TTL consts
crates/auth/                 NEW  JWT + JWKS (native)
  Cargo.toml                       (jsonwebtoken, ed25519-dalek, sha2, base64, serde, login-api, game)
  src/lib.rs                       Signer, Verifier, Jwks, KeyRing, VerifyError
crates/login/                NEW  the service + binary
  Cargo.toml
  migrations/{sqlite,pg}/0001_login_v1.sql
  src/lib.rs                       pub fn router(state) / serve(config)
  src/config.rs                    LoginConfig::from_env
  src/store.rs                     LoginStore (sqlx), row types
  src/password.rs                  Hasher (Argon2id), policy checks
  src/mail.rs                      Mailer trait, LogMailer, SesMailer, templates
  src/api.rs                       axum handlers
  src/pages.rs                     GET /verify, GET /reset HTML pages
  src/bin/login.rs                 `login serve|keygen|world …|account …`
  tests/it.rs + tests/{schema,store,api,ctl}.rs
crates/game/src/protocol.rs  MOD  Hello { credential: Credential }
crates/game/src/player.rs    MOD  Credential enum
crates/server/src/player_store.rs  MOD  resolve_hello takes an Authenticator
crates/server/src/auth_edge.rs     NEW  AuthMode, JwksCache, Authenticator
crates/server/src/lib.rs, webtransport.rs  MOD  pass the Authenticator
crates/client/src/login/     NEW  api.rs (reqwest), store.rs (refresh token), flow.rs (Bevy systems)
crates/client/src/netcode.rs, netcode_web.rs, session.rs, frontend/mod.rs  MOD
crates/slint-ui/ui/login.slint, worlds.slint  NEW; frontend.slint, main-menu.slint MOD
../ui/crates/widgets/ui/textbox.slint  MOD (other repo): `password` property
```

---

### Task 1: `Credential` in the `Hello` packet

**Files:**
- Modify: `crates/game/src/player.rs` (add `Credential`), `crates/game/src/protocol.rs:737-745` (`ClientPacket::Hello`)
- Modify: `crates/server/src/player_store.rs:49-70` (`resolve_hello`), `crates/server/src/lib.rs:419-434` (`quic_hello`), `crates/server/src/webtransport.rs:225-244` (`web_hello`)
- Modify: `crates/client/src/netcode.rs:229-234`, `crates/client/src/netcode_web.rs:228-231`, `crates/client/src/local_server.rs` (if it builds a `Hello`)
- Test: `crates/game/tests/it.rs` new module `credential.rs`; existing `crates/server/tests/webtransport_handshake.rs`, `player_store.rs`

**Interfaces:**
- Produces: `pub enum Credential { Token(PlayerToken), Jwt(String) }` (serde, `Debug` redacts: `Token(abc123…)`, `Jwt(kid=…)` without the payload); `ClientPacket::Hello { credential: Credential }`; `pub const MAX_JWT_BYTES: usize = 2048` in `player.rs`.

- [ ] **Step 1: Write the failing test** `credential::hello_round_trips_both_kinds`: encode/decode `Hello { credential: Credential::Token(t) }` and `Credential::Jwt("a.b.c".into())` through `encode_packet`/`decode_packet`, assert equal; `credential::debug_never_prints_secrets`: `format!("{:?}", Credential::Jwt(x))` does not contain the payload segment, and the token form shows at most `PLAYER_TOKEN_DEBUG_CHARS`.
- [ ] **Step 2: Run** `just test-one game "--test it credential::"` — expect a compile failure (no `Credential`).
- [ ] **Step 3: Implement** `Credential` and change `Hello`. Server: `resolve_hello` matches `Hello { credential: Credential::Token(t) }` exactly as today; `Credential::Jwt(_)` → `Err("this server does not use login (single-player / dev)")` for now (Task 7 replaces it). Client: native and wasm send `Credential::Token(load_or_create_token())` — behaviour unchanged.
- [ ] **Step 4: Run** `just test-one game "--test it credential::"`, `just test-one server "--test player_store"`, `just test-one server "--test webtransport_handshake -- --include-ignored"`, `just smoke` — all pass.
- [ ] **Step 5: Commit** `feat(protocol): Hello carries a Credential (token or login JWT)`.

### Task 2: `login-api` and `auth` crates (claims, tickets, signing, JWKS)

**Files:**
- Create: `crates/login-api/{Cargo.toml,src/lib.rs}`, `crates/auth/{Cargo.toml,src/lib.rs}`, `crates/auth/tests/it.rs` + `tests/jwt.rs`
- Modify: root `Cargo.toml` (members; workspace deps `jsonwebtoken = "9"`, `ed25519-dalek = { version = "2", features = ["pkcs8", "pem", "rand_core"] }`, `sha2`, `base64`)

**Interfaces:**
- Produces (`login_api`):
  - `pub struct JoinClaims { pub sub: String, pub world: String, pub player_token: String, pub session_epoch: u64, pub aud: String, pub iat: u64, pub exp: u64 }`
  - `pub struct JoinTicket { pub world: String, pub jwt: String, pub quic: Option<String>, pub webtransport: Option<String>, pub cert_hash: Option<String>, pub expires_in_secs: u64 }`
  - `pub struct RegisterRequest { email, password }`, `LoginRequest { email, password }`, `LoginResponse { refresh_token: String, account_email: String, email_verified: bool }`, `RefreshRequest { refresh_token }`, `JoinRequest { refresh_token, world }` → `JoinResponse { ticket: JoinTicket, refresh_token: String }` (join rotates the refresh token too), `ResetRequest { email }`, `ResetConfirm { token, password }`, `VerifyRequest { token }`, `WorldEntry { id, name, status: WorldStatus }`, `enum WorldStatus { Open, Closed }`, `ApiError { code: ApiErrorCode, message: String }` with `enum ApiErrorCode { BadRequest, BadCredentials, NotVerified, Revoked, UnknownWorld, WorldClosed, Internal }`.
  - consts: `JOIN_JWT_TTL_SECS: u64 = 300`, `HOST_AUDIENCE: &str = "mmo-host"`, `REFRESH_TTL_DAYS: u64 = 30`, `PASSWORD_MIN_CHARS = 10`, `PASSWORD_MAX_CHARS = 128`, `EMAIL_MAX_CHARS = 254`, path consts `PATH_REGISTER = "/v1/register"`, `PATH_LOGIN`, `PATH_REFRESH`, `PATH_LOGOUT`, `PATH_WORLDS`, `PATH_JOIN`, `PATH_VERIFY = "/v1/verify"`, `PATH_RESET_REQUEST`, `PATH_RESET_CONFIRM`, `PATH_JWKS = "/.well-known/jwks.json"`.
- Produces (`auth`):
  - `pub struct SigningKey` (`from_pkcs8_pem(&str)`, `generate() -> (SigningKey, String /*pem*/)`, `kid() -> &str`); `pub struct Signer { keys: Vec<SigningKey> }` — `Signer::load_dir(&Path) -> io::Result<Signer>` (newest file by name sorts last = signing key), `sign(&JoinClaims) -> String`, `jwks() -> Jwks`.
  - `pub struct Jwks { pub keys: Vec<Jwk> }`, `Jwk { kty: "OKP", crv: "Ed25519", x: base64url, kid, alg: "EdDSA", use_: "sig" }` (serde rename `use`).
  - `pub struct Verifier { by_kid: BTreeMap<String, DecodingKey> }` — `from_jwks(&Jwks)`, `verify(&str, now_secs: u64, expect_world: &str) -> Result<JoinClaims, VerifyError>`; `enum VerifyError { Malformed, UnknownKid(String), BadSignature, Expired, WrongAudience, WrongWorld { got: String } }` with `Display` used as the refusal text.
  - `pub const JWT_LEEWAY_SECS: u64 = 30`. Verification passes `now` explicitly (tests drive time; `jsonwebtoken`'s own `exp` check is disabled and done by hand with the leeway).

- [ ] **Step 1: Write failing tests** in `crates/auth/tests/jwt.rs`: `sign_then_verify_round_trips` (claims equal); `expired_is_refused` (now = exp + leeway + 1 → `Expired`; exp + leeway → ok); `wrong_world_is_refused`; `wrong_audience_is_refused`; `other_key_is_bad_signature` (sign with key A, verifier knows only key B under A's kid → `BadSignature`); `unknown_kid` → `UnknownKid`; `jwks_round_trips_json` (serialize, parse, verifier built from it verifies); `load_dir_signs_with_newest` (two PEMs `0001.pem`, `0002.pem` → header kid = key 2's); `kid_is_sha256_prefix` (16 lowercase hex).
- [ ] **Step 2: Run** `just test-one auth "--test it"` — fails to compile.
- [ ] **Step 3: Implement** both crates per the Interfaces. `login-api` depends on `serde` only (+ `serde_json` in tests); make sure it builds for `wasm32-unknown-unknown`.
- [ ] **Step 4: Run** `just test-one auth "--test it"`, and `nice -n 10 taskset -c 0-15 cargo check -p login-api --target wasm32-unknown-unknown` — pass.
- [ ] **Step 5: Commit** `feat(auth): login-api DTOs and EdDSA join JWTs with JWKS`.

### Task 3: Login database (schema, store, password hashing)

**Files:**
- Create: `crates/login/Cargo.toml`, `crates/login/migrations/sqlite/0001_login_v1.sql`, `crates/login/migrations/pg/0001_login_v1.sql`, `crates/login/src/{lib.rs,store.rs,password.rs}`, `crates/login/tests/{it.rs,common/mod.rs,schema.rs,store.rs}`
- Modify: root `Cargo.toml` (member; `argon2 = "0.5"`, `axum = "0.8"`, `tower-http = { version = "0.6", features = ["cors"] }`, `aws-config`, `aws-sdk-sesv2`), `.gitignore` if needed (`target/` already covers `target/dev-login/`)

**Interfaces:**
- Schema v1 (both engines column-for-column; ints, text, blobs only; times as Unix seconds INTEGER):
  - `accounts(account_id INTEGER PK autoinc/identity, email TEXT UNIQUE NOT NULL, password_hash TEXT NOT NULL, email_verified_at INTEGER NULL, created_at INTEGER NOT NULL)`
  - `refresh_tokens(token_hash BLOB PK, account_id INTEGER NOT NULL, family BLOB NOT NULL, expires_at INTEGER NOT NULL, revoked_at INTEGER NULL)` + index on `account_id`
  - `email_tokens(token_hash BLOB PK, account_id INTEGER NOT NULL, kind INTEGER NOT NULL /*0 verify,1 reset*/, expires_at INTEGER NOT NULL, used_at INTEGER NULL)`
  - `worlds(world_id TEXT PK, name TEXT NOT NULL, status INTEGER NOT NULL /*0 open,1 closed*/, quic TEXT NULL, webtransport TEXT NULL, cert_hash TEXT NULL, created_at INTEGER NOT NULL)`
  - `world_players(account_id INTEGER, world_id TEXT, player_token TEXT UNIQUE NOT NULL, created_at INTEGER NOT NULL, PK(account_id, world_id))`
  - `sessions(account_id INTEGER, world_id TEXT, session_epoch INTEGER NOT NULL, started_at INTEGER NOT NULL, PK(account_id, world_id))`
- `pub struct LoginStore { pool: AnyPool-like enum }` — `LoginStore::open(url: &str) -> Result<LoginStore, LoginError>` (SQLite path / `sqlite::memory:` / `postgres://`, migrate); methods, all `async`:
  `create_account(email, password_hash, now) -> Result<Option<AccountId>, _>` (None = email exists), `account_by_email(email) -> Option<AccountRow>`, `account(id) -> Option<AccountRow>`, `set_password(id, hash)`, `mark_verified(id, now)`,
  `insert_refresh(hash: [u8;32], account, family: [u8;16], expires_at)`, `take_refresh(hash, now) -> RefreshOutcome { Valid { account, family }, Reused { account, family }, Unknown }` (Valid revokes it; Reused = it was already revoked → caller revokes the family), `revoke_family(family, now)`, `revoke_account_refresh(account, now)`,
  `insert_email_token(hash, account, kind, expires_at)`, `use_email_token(hash, kind, now) -> Option<AccountId>`,
  `put_world(WorldRow)`, `remove_world(id)`, `worlds() -> Vec<WorldRow>`, `world(id) -> Option<WorldRow>`,
  `player_token_for(account, world, now, new_token: impl FnOnce() -> String) -> String` (insert-if-absent),
  `bump_session(account, world, now) -> u64` (upsert; returns the new epoch, starting at 1).
- `pub type AccountId = i64`; `pub struct Hasher { params }` — `Hasher::default()`, `Hasher::for_tests()`, `hash(&str) -> String`, `verify(&str, &str) -> bool`; `pub fn check_password(&str) -> Result<(), &'static str>`, `pub fn normalize_email(&str) -> Result<String, &'static str>`.

- [ ] **Step 1: Write failing tests:** `schema::schemas_match` (copy `crates/world-store/tests/schema.rs`'s introspection: SQLite vs a checked-in `EXPECTED` list, then Postgres vs SQLite when `MMO_TEST_DATABASE_URL` is set); `store::create_account_refuses_duplicate_email`; `store::refresh_rotation` (insert → take = Valid → take again = Reused); `store::revoke_family_revokes_all_of_it`; `store::email_token_single_use_and_expiry` (use before expiry → Some, second use → None, expired → None); `store::bump_session_is_per_account_and_world` (A/w1 → 1, 2; A/w2 → 1; B/w1 → 1); `store::player_token_stable_per_world` (same account+world → same token, other world → different); `password::policy` (9 chars refused, 10 ok, 129 refused); `password::email_normalized` (`" A@B.c "` → `a@b.c`, `"a@@b"`, `"@b"`, `"a@"` refused); `password::hash_verify` with `for_tests()`.
- [ ] **Step 2: Run** `just test-one login "--test it"` — fails to compile.
- [ ] **Step 3: Implement** migrations, `LoginStore` (one sqlx pool per engine behind an enum, runtime queries; sqlite WAL like `world-store/src/open.rs`), `password.rs`.
- [ ] **Step 4: Run** `just test-one login "--test it"`; with `just pg-up`, `just test-login-pg "--test it"` (new recipe mirroring `test-pg`) — pass.
- [ ] **Step 5: Commit** `feat(login): login database (accounts, refresh tokens, worlds, sessions) on SQLite and Postgres`.

### Task 4: Mail (SES and log mailers, verification / reset messages)

**Files:**
- Create: `crates/login/src/mail.rs`; Test: `crates/login/tests/mail.rs`

**Interfaces:**
- `pub struct Mail { pub to: String, pub subject: String, pub text: String }`
- `#[async_trait]`-free trait: `pub trait Mailer: Send + Sync { fn send(&self, mail: Mail) -> BoxFuture<'static, Result<(), MailError>>; }`
- `pub struct LogMailer { sent: Arc<Mutex<Vec<Mail>>> }` — logs `to` + subject + the link at info, keeps mail for tests (`sent()`).
- `pub struct SesMailer { client: aws_sdk_sesv2::Client, from: String }` — `SesMailer::from_env(from: String) -> SesMailer` (`aws_config::load_defaults(BehaviorVersion::latest())`), sends a `Simple` text body.
- `pub fn verify_mail(to, public_url, token) -> Mail`, `pub fn reset_mail(to, public_url, token) -> Mail`, `pub fn already_registered_mail(to, public_url) -> Mail` — links `{public_url}/verify?token=…` and `{public_url}/reset?token=…`.
- consts: `VERIFY_TOKEN_TTL_SECS = 86_400`, `RESET_TOKEN_TTL_SECS = 3_600`.

- [ ] **Step 1: Failing tests:** `mail::verify_mail_links_public_url` (text contains `https://x/login/verify?token=t`), `mail::reset_mail_links`, `mail::log_mailer_records`.
- [ ] **Step 2: Run** `just test-one login "--test it mail::"` — fails.
- [ ] **Step 3: Implement.** No test talks to SES; `SesMailer` is only constructed by `serve` when `VOXEL_MAILER=ses`.
- [ ] **Step 4: Run** the tests — pass.
- [ ] **Step 5: Commit** `feat(login): SES and log mailers with verification and reset mails`.

### Task 5: The HTTP API

**Files:**
- Create: `crates/login/src/{api.rs,pages.rs,config.rs}`, modify `crates/login/src/lib.rs`; Test: `crates/login/tests/api.rs`

**Interfaces:**
- `pub struct LoginConfig { bind: SocketAddr, db_url: String, keys_dir: PathBuf, public_url: String, cors_origins: Vec<String>, require_verified: bool, mailer: MailerKind }` — `from_env()`: `VOXEL_LOGIN_BIND` (default `127.0.0.1:8790`), `VOXEL_LOGIN_DB` (default `target/dev-login/login.sqlite`), `VOXEL_LOGIN_KEYS_DIR` (default `target/dev-login/keys`), `VOXEL_LOGIN_PUBLIC_URL` (default `http://127.0.0.1:8790`), `VOXEL_LOGIN_CORS_ORIGINS` (default `http://127.0.0.1:1334`), `VOXEL_LOGIN_REQUIRE_VERIFIED` (`1` = on), `VOXEL_MAILER=log|ses` (default `log`) + `VOXEL_MAIL_FROM`.
- `pub struct AppState { store: LoginStore, signer: Arc<Signer>, hasher: Hasher, mailer: Arc<dyn Mailer>, config: Arc<LoginConfig>, clock: Arc<dyn Fn() -> u64 + Send + Sync> }`; `pub fn router(state: AppState) -> axum::Router`; `pub async fn serve(config: LoginConfig) -> io::Result<()>`.
- Endpoints (JSON bodies from `login-api`; errors are `ApiError` with status 400/401/403/404/409/500):
  - `POST /v1/register` → 202 always (mails verify, or "already registered").
  - `POST /v1/login` → `LoginResponse` (new refresh family) | 401 `BadCredentials` | 403 `NotVerified` (only with `require_verified`).
  - `POST /v1/refresh` → `LoginResponse` (rotated) | 401 `Revoked` (reuse revokes the family).
  - `POST /v1/logout` `{refresh_token}` → 204, revokes the family.
  - `GET /v1/worlds` → `Vec<WorldEntry>` (no auth; registry is public).
  - `POST /v1/join` `JoinRequest` → `JoinResponse`: rotate refresh; 404 `UnknownWorld`, 403 `WorldClosed`; `player_token_for`, `bump_session`, sign `JoinClaims { exp = now + JOIN_JWT_TTL_SECS, aud = HOST_AUDIENCE, … }`, ticket addresses from the `worlds` row.
  - `POST /v1/verify` `{token}` → 204 | 400; `POST /v1/reset/request` → 202 always; `POST /v1/reset/confirm` → 204 (sets password, revokes all refresh tokens) | 400.
  - `GET /verify?token=` and `GET /reset?token=` (`pages.rs`): plain HTML (no JS): verify confirms and shows the result; reset shows a form that POSTs (`application/x-www-form-urlencoded`) to `/reset` which confirms and shows the result.
  - `GET /.well-known/jwks.json` → `Jwks`; `GET /healthz` → 200.
  - CORS layer: allowed origins from config, methods GET/POST, header `content-type`.

- [ ] **Step 1: Failing tests** (`tower::ServiceExt::oneshot` against `router()` on `sqlite::memory:`, `LogMailer`, a fixed clock, `Hasher::for_tests()`): `api::register_login_join_happy_path` (register → verify via the mailed token → login → worlds lists the seeded world → join → ticket's JWT verifies with `Verifier::from_jwks(GET jwks)` for that world, claims carry the account's `player_token` and `session_epoch == 1`); `api::second_join_bumps_epoch_same_world` (epochs 1, 2); `api::two_worlds_independent` (join w1 → epoch 1, join w2 → epoch 1, both JWTs verify for their own world only); `api::login_unknown_email_and_bad_password_look_the_same` (same status + body); `api::register_existing_email_sends_already_registered_and_202`; `api::refresh_reuse_revokes_family` (refresh twice with the same token → second is 401, and the token returned by the first refresh is now also 401); `api::require_verified_refuses_unverified`; `api::reset_sets_password_and_revokes_refresh`; `api::join_closed_world_403`, `api::join_unknown_world_404`; `api::cors_allows_configured_origin_only` (OPTIONS preflight from `http://127.0.0.1:1334` has `access-control-allow-origin`, from `http://evil` has none); `api::reset_page_form_round_trip` (GET page contains a form, POST form → 200 "password changed").
- [ ] **Step 2: Run** `just test-one login "--test it api::"` — fails.
- [ ] **Step 3: Implement** handlers (`api.rs`), pages, config. Tokens: 32 random bytes via `getrandom`, base64url (no padding), DB holds SHA-256. Time from `state.clock`.
- [ ] **Step 4: Run** `just test-one login "--test it"` — pass; mark any test >100 ms slow.
- [ ] **Step 5: Commit** `feat(login): HTTP API — register, verify, login, refresh, logout, reset, worlds, join, JWKS`.

### Task 6: The `login` binary (serve + admin) and recipes

**Files:**
- Create: `crates/login/src/bin/login.rs`, `crates/login/src/ctl.rs`; Test: `crates/login/tests/ctl.rs`
- Modify: `justfile` (index-only staging), `CLAUDE.md` (index-only staging: a "Login service" paragraph under `crates/server` or a new crate bullet)

**Interfaces:**
- `pub fn run_ctl(args: &[String], out: &mut dyn Write) -> Result<(), String>` (testable); subcommands: `serve`; `keygen [--dir D]` (writes `NNNN.pem`, next number, mode 0600, prints the kid); `world add <id> --name <n> [--quic host:port] [--wt url] [--cert-hash h] [--closed]`; `world list`; `world remove <id>`; `world open|close <id>`; `account set-password <email>` (reads the password from stdin); `account verify <email>`.
- just recipes: `login *args` (`cargo run -p login --bin login -- {{args}}`, default `serve`), `login-keygen`, `login-world-add *args`, `login-world-list`, `test-login-pg *args`, and `dev-login`: ensures a key exists, registers world `dev` (`--quic 127.0.0.1:6466 --wt https://127.0.0.1:6467/`) if absent, then runs `login serve` and a dev server with `VOXEL_AUTH=jwt VOXEL_WORLD_ID=dev VOXEL_LOGIN_URL=http://127.0.0.1:8790` together (SIGTERM both on exit, like `just web`).

- [ ] **Step 1: Failing tests:** `ctl::keygen_numbers_files_and_signs_with_newest`, `ctl::world_add_list_remove`, `ctl::set_password_then_login` (via `LoginStore` + `Hasher::for_tests` injection).
- [ ] **Step 2: Run** `just test-one login "--test it ctl::"` — fails.
- [ ] **Step 3: Implement** `ctl.rs` + the thin `bin/login.rs`; add the recipes and the CLAUDE.md paragraph (login service: crates, env vars, recipes, schema, JWT claims, single-player unaffected).
- [ ] **Step 4: Run** the tests; `just --list | grep login` shows the recipes; `just smoke` passes.
- [ ] **Step 5: Commit** (own files with `git add`; justfile + CLAUDE.md hunks via `git update-index`) `feat(login): login binary with admin subcommands; just login / dev-login`.

### Task 7: The host verifies login JWTs at the edge

**Files:**
- Create: `crates/server/src/auth_edge.rs`; Test: `crates/server/tests/auth_edge.rs` (+ `[[test]]` if it installs a logger; otherwise in `tests/it.rs`)
- Modify: `crates/server/src/player_store.rs:49-70`, `crates/server/src/lib.rs:496-500` (construct), `:419-434`, `crates/server/src/webtransport.rs:130,175-184`, `crates/server/Cargo.toml` (`auth`, `login-api`, `reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json"] }`)

**Interfaces:**
- Consumes: `auth::Verifier`, `auth::Jwks`, `login_api::{JoinClaims, PATH_JWKS}`, `Credential` (Task 1).
- Produces:
  - `pub enum AuthMode { Token, Jwt { world: String, login_url: String } }` — `AuthMode::from_env()`: `VOXEL_AUTH` = `token` (default) | `jwt` (then `VOXEL_WORLD_ID` and `VOXEL_LOGIN_URL` required — the server refuses to start without them, naming the variable).
  - `pub struct JwksCache` — `async fn verifier_for(&self, kid: &str) -> Result<Verifier, VerifyError>` (fetch on start, refetch on unknown kid no more often than `JWKS_REFETCH_MIN_SECS = 30`; fetch failures logged, cached keys kept); test constructor `JwksCache::fixed(Jwks)`.
  - `pub struct Authenticator { mode, jwks: Option<JwksCache>, epochs: Mutex<BTreeMap<String, u64>> /* player_token -> newest epoch */, now: Arc<dyn Fn() -> u64 + Send + Sync> }` — `async fn authenticate(&self, credential: Credential) -> Result<PlayerToken, String>`; jwt mode: verify, check `session_epoch >= epochs[token]` else `Err("superseded by a newer login")`, record the max; token mode: today's `is_valid` check.
  - `resolve_hello(store, auth: &Authenticator, first: ClientPacket)` → authenticate, then `store.resolve(&token)` as today.

- [ ] **Step 1: Failing tests** (driving `resolve_hello` directly with a `MemoryPlayerStore`, a `JwksCache::fixed`, a fixed clock, and JWTs signed by `auth::Signer` from a temp key dir): `auth_edge::jwt_host_accepts_valid_ticket` (returns the same `PlayerId` for two valid tickets of one `player_token`); `…refuses_expired`; `…refuses_wrong_world`; `…refuses_bad_signature`; `…refuses_unknown_kid` (after a refetch that still lacks it); `…refuses_older_epoch_after_newer` (epoch 2 accepted, then epoch 1 → "superseded"); `…refuses_token_credential` ("this server requires login"); `token_host_refuses_jwt` (message names single-player / dev); `token_host_unchanged` (today's token path). One end-to-end QUIC test in `tests/auth_edge.rs` (slow-tagged): a jwt-mode server on an ephemeral port refuses a token Hello with `Refused` and closes.
- [ ] **Step 2: Run** `just test-one server "--test it auth_edge::"` — fails.
- [ ] **Step 3: Implement** `auth_edge.rs`; thread an `Arc<Authenticator>` through `WorldIngress::new` and `webtransport::serve` to both hello paths. Hello timeout / max bytes unchanged (a 2 KiB JWT fits `HELLO_MAX_BYTES = 4096`).
- [ ] **Step 4: Run** the tests, `just test-one server "--test webtransport_handshake -- --include-ignored"`, `just smoke` — pass.
- [ ] **Step 5: Commit** `feat(server): VOXEL_AUTH=jwt verifies login JWTs at the QUIC and WebTransport edges`.

### Task 8: Password field in the widgets library (`../ui`)

**Files:**
- Modify (other repo `~/Projects/ui`): `crates/widgets/ui/textbox.slint:74-110` (`TextField`) — add `in property <bool> password: false;` mapped to the inner `TextInput`'s `input-type: root.password ? InputType.password : InputType.text`.
- Test: that repo's reftest / widget tests if they cover `TextField`; otherwise a slint-ui test here in Task 10.

- [ ] **Step 1: Write a failing test** in `crates/slint-ui/tests/login_screen.rs` (`login_screen::password_is_masked`, slow-tagged like the other slint-ui tests): render the login screen with `login-password: "secret12345"` and with `""`, and with the field's `password: false` vs `true`; assert the rendered pixels of the password field differ between `password: true` and `false` for the same text (masking changes the glyphs). Pixel equality with a reference image is not required.
- [ ] **Step 2: Run** `just test-one slint-ui "--test it -- --include-ignored login_screen::"` — fails (no `password` property).
- [ ] **Step 3: Implement** in `../ui`; build here (`just smoke` type-checks the client) — note the widgets' Fortress-only overlay in `crates/slint-ui/build_common.rs` copies the widget sources, so no change is needed there unless it filters properties.
- [ ] **Step 4: Run** the test — pass.
- [ ] **Step 5: Commit in `~/Projects/ui`** `feat(widgets): TextField password mode`; nothing to commit here yet (the test lands with Task 10).

### Task 9: Client login API and credential storage

**Files:**
- Create: `crates/client/src/login/{mod.rs,api.rs,store.rs}`
- Modify: `crates/client/Cargo.toml` (`login-api`; `reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }` — on wasm32 reqwest uses `fetch`; enable the `web-sys` features it needs if any), `crates/client/src/netcode.rs:73,93,229` and `netcode_web.rs:228` (`ServerConnection.credential: Option<Credential>` replaces `token`; `None` = load the token as today)

**Interfaces:**
- `pub struct LoginApi { base: String, http: reqwest::Client }` — `new(base)`, `async fn register(&self, email, password) -> Result<(), LoginFailure>`, `login(email, password) -> Result<LoginResponse, LoginFailure>`, `refresh(token) -> Result<LoginResponse, LoginFailure>`, `logout(token)`, `worlds() -> Result<Vec<WorldEntry>, LoginFailure>`, `join(token, world) -> Result<JoinResponse, LoginFailure>`, `request_reset(email)`.
- `pub enum LoginFailure { Network(String), Api(ApiError), Protocol(String) }` with a player-facing `Display` ("Wrong email or password", "Can't reach the login server", …).
- `pub fn login_url() -> Option<String>` (decision 15; native env/`option_env!`, wasm `?login=` / build / https origin + `/login`).
- `store.rs`: `pub fn load_refresh() -> Option<String>`, `save_refresh(&str)`, `clear_refresh()` — native file `login_refresh` next to `player_token` (0600, atomic), wasm `localStorage["mmo.login_refresh"]`; `pub const REFRESH_STORAGE_KEY: &str = "mmo.login_refresh"`.
- `ConnectPolicy`/`ServerConnection` gains `credential: Option<Credential>`; `Command::ConnectToServerAndScene` carries it; `sim_driver::start_session` (wasm) takes it.

- [ ] **Step 1: Failing tests** (`--lib`, explicit client opt-in; pure functions only — the client must not depend on `login`, and the HTTP round trip is covered by Task 5's API tests plus the live check): `login::api::tests::error_body_maps_to_failure` (parse a JSON `ApiError` body into `LoginFailure::Api`), `login::api::tests::login_url_resolution` (pure function `resolve_login_url(env, built, page) -> Option<String>` over the decision-15 cases), `login::store::tests::refresh_round_trips_in_temp_dir` (native; `VOXEL_PLAYER_TOKEN_FILE`-style override env `VOXEL_LOGIN_REFRESH_FILE`).
- [ ] **Step 2: Run** `just test-one client "--lib login::"` (ask first: it builds the client test binary) — fails.
- [ ] **Step 3: Implement.** HTTP response handling in one helper (`status.is_success()` → body as T, else `ApiError` JSON, else `Protocol`). Netcode: native and wasm send `credential.unwrap_or(Credential::Token(load_or_create_token()))`.
- [ ] **Step 4: Run** the tests; `just smoke` (type-checks the client); `nice -n 10 taskset -c 0-15 cargo check -p client --target wasm32-unknown-unknown` — pass.
- [ ] **Step 5: Commit** `feat(client): login API client, refresh-token storage, credential in the connection`.

### Task 10: Login and world-picker screens (native + browser)

**Files:**
- Create: `crates/slint-ui/ui/login.slint` (`LoginScreen`), `crates/slint-ui/ui/worlds.slint` (`WorldsScreen`), `crates/client/src/login/flow.rs`; Test: `crates/slint-ui/tests/login_screen.rs` (in `tests/it.rs`)
- Modify: `crates/slint-ui/ui/frontend.slint` (screens 5 = login, 6 = worlds; `back-button-x/y` arms; properties + callbacks below), `crates/slint-ui/ui/main-menu.slint` (a "Play online" button, `visible: root.online-available && !root.join-only`), `crates/client/src/session.rs:41` (`AppState::Login`, `AppState::Worlds`), `crates/client/src/frontend/mod.rs:36,53,65-72,134-222` (actions, `screen_index`, wiring), `crates/client/src/main.rs` (wasm initial state: `Login` when `login_url()` is some, else `MainMenu`)

**Interfaces:**
- Slint `FrontendUi` additions: `in property <bool> online-available`, `in-out property <string> login-email`, `in-out property <string> login-password`, `in property <string> login-error`, `in property <bool> login-busy`, `in property <string> login-account` (signed-in email), `in property <[WorldRow]> worlds` (`struct WorldRow { id: string, name: string, open: bool }` in the frontend crate), `callback login-submit()`, `callback login-register()`, `callback login-forgot()`, `callback world-join(string)`, `callback logout()`, `callback play-online()`.
- `FrontendAction` additions: `PlayOnline`, `LoginSubmit`, `Register`, `ForgotPassword`, `JoinWorld(String)`, `Logout`.
- `flow.rs`: resource `LoginSession { api: LoginApi, refresh: Option<String>, account: Option<String>, worlds: Vec<WorldEntry>, pending: Option<Task<…>> }` — deliberately NOT registered with `add_session_reset`: the login outlives a world session (say so in its doc comment); systems `start_login`, `poll_login`. Native: login calls run on one dedicated `login-http` thread holding a current-thread tokio runtime (add tokio's `rt` feature to the client), requests in / results out over crossbeam; wasm: `wasm_bindgen_futures::spawn_local` + the same crossbeam channel back. On join success: insert `PendingJoin { ticket: JoinTicket }` and go to `Starting`; native connects to `ticket.quic` with `Credential::Jwt(ticket.jwt)`; wasm builds a `PendingServer` from `ticket.webtransport` (+ `cert_hash` pinning when present) with the JWT credential. Stored refresh token present at startup → skip straight to `Worlds` after a silent `refresh`.
- A reconnect after the session ends returns to `Worlds` (not `MainMenu`) when logged in.

- [ ] **Step 1: Failing tests** (slint-ui, headless, slow-tagged like `frontend.rs`): `login_screen::submit_fires_with_fields` (type email/password, click Log in → `login-submit` fired, properties hold the text); `login_screen::password_is_masked` (from Task 8); `login_screen::error_line_shows`; `login_screen::worlds_list_join_fires_id`; `login_screen::play_online_hidden_without_login`. Client `--lib` (opt-in): `frontend::tests::screen_index_login_and_worlds` (5, 6).
- [ ] **Step 2: Run** `just test-one slint-ui "--test it -- --include-ignored login_screen::"` — fails.
- [ ] **Step 3: Implement** the screens (fortress theme, existing widgets: `TextField`, buttons, a list like `WorldRow` in the Load screen) and the flow.
- [ ] **Step 4: Run** the slint-ui tests; `just smoke`; `just slint-wasm`; wasm `cargo check -p client --target wasm32-unknown-unknown` — pass.
- [ ] **Step 5: Commit** `feat(client): login and world-picker screens (native + browser)`.

### Task 11: Dev wiring, docs, live check

**Files:**
- Modify: `justfile` (`web` honours `login=` → appends `?login=` hint to the printed URL; `dev-login` from Task 6 also starts the web flow when `web=1`), `CLAUDE.md` (client login screens, `VOXEL_AUTH`, `VOXEL_WORLD_ID`, `VOXEL_LOGIN_URL`, `?login=`), `TODO.md` (R2a done; R2b next; GDPR in R5; rate limiting deferred)

- [ ] **Step 1:** Update recipes + docs (index-only staging for CLAUDE.md / justfile).
- [ ] **Step 2: Run** `just smoke`, `just test-one login "--test it"`, `just test-one auth "--test it"`, `just test-one server "--test it auth_edge::"` — pass.
- [ ] **Step 3: Ask the user, then live check** (one client, `adapter=Intel`): `just dev-login`; register + verify via the logged link; native client "Play online" → log in → world `dev` → Join → in the world; second native login of the same account takes the first over (first shows "logged in elsewhere"); `just run` with no login service still starts single-player; browser: `just web` + `?login=http://127.0.0.1:8790` → log in → join over WebTransport. Record results in the commit message / TODO.md.
- [ ] **Step 4: Commit** `docs: R2a login service, client login, dev-login`.

---

## Self-review notes

- Spec coverage: auth + Argon2id + JWT/JWKS (§0.7a) → Tasks 2, 3, 5, 7; accounts global / players per world, sessions `(account, world)`, newest wins (§0.9b, §0.10.4) → Tasks 3, 5, 7; world registry (§0.9a) → Tasks 3, 6; email (§0.8 q8) → Tasks 4, 5; client login native + browser (§0.8 q19) → Tasks 9, 10; single-player unaffected → Tasks 1, 7. Deferred and named: revocation kick + `SpawnPlayer` push (R2b), GDPR (R5), rate limiting (out), panel (own plan).
- `PlayerToken` stops being a secret on JWT hosts only; token hosts (single-player) keep today's trust model (§0.7a last bullet).
- Types used across tasks: `Credential` (T1) → T7, T9; `JoinClaims`/`JoinTicket`/DTOs (T2) → T5, T7, T9, T10; `Verifier`/`Jwks`/`Signer` (T2) → T5, T6, T7; `LoginStore`/`Hasher` (T3) → T5, T6; `LoginApi`/`LoginFailure`/`login_url` (T9) → T10.
