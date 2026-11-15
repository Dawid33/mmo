# Keycloak 2 — The game takes player identity from Keycloak — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A player signs in to the game with their Keycloak account (browser hand-off, no password typed into the game), and the login service stops owning passwords, sign-up, email verification and password reset.

**Architecture:** The client runs the OAuth device flow against Keycloak's `game` client and gets an access token; it posts that once to the login service (`POST /v1/login/oidc`), which verifies it against Keycloak's keys, finds or creates the account by the token's subject, and answers with the same `LoginResponse` (its own rotating refresh token) as today. Everything after that is unchanged: worlds, `/v1/join`, EdDSA join tokens, host verification, spawn orders, kicks.

**Tech Stack:** Rust (existing crates `login-api`, `login`, `client`, `slint-ui`); `jsonwebtoken` 9 (RS256 through `ring`), workspace `reqwest` 0.12 (`rustls-tls`), axum 0.8, sqlx 0.8; Keycloak 26.8 (`quay.io/keycloak/keycloak:26.8.2`) in dev through `deploy/dev/compose.yml`.

**Spec:** none written. The decisions below (user, 2026-11-12) are the spec. Depends on `2026-11-12-keycloak-1-identity-provider-zulip.md` only for production values (issuer, client id `game`, audience `mmo-login`); dev and tests need nothing from it.

## Verified against the code on 2026-11-13 (`ebacf333`, after R3) — read before starting

- **R3 changed nothing this plan depends on.** `JoinClaims`, `/v1/join`, the host's ticket check, `SpawnPlayer` / `Kick` and the entry-host link are as assumed. Login migrations stop at `0003_pending_kicks`, so `0004` is free. `jsonwebtoken = "9"` and `reqwest` 0.12 (`rustls-tls`, `json`) are workspace dependencies already.
- **Production is ready.** `https://auth.factoriosurvivalworld.com/realms/players` is live and advertises `device_authorization_endpoint`; its `game` client (`deploy/modules/keycloak-realm/clients.tf`) is public, has the device grant on and the `mmo-login` audience mapper.
- **The dev Keycloak already exists** (the website work added it): service `keycloak` in `deploy/dev/compose.yml` on `127.0.0.1:8180` with the `orion` theme mounted, `just kc-up` / `kc-down`, realm file `deploy/dev/keycloak/players-realm.json`. That file has ONLY the `site` client and the user `dev` (`dev@localhost`, verified). Task 5 Step 1 therefore only adds the `game` client; it must not touch the `site` client, the theme mount or `ORION_SITE_URL` (`just site-theme-check` reads them).
- **Three launches log in with a password and no screens**, which the first text of this plan missed: `client debug online` (`crates/client/src/debug_launch.rs`: `OnlineLogin`, `online_login_from`, `LoginIntent::Submit`, env `VOXEL_LOGIN_EMAIL` / `VOXEL_LOGIN_PASSWORD`), `just run-online` and `just go-nogo-r3` (both run `login account ensure` and pass that env). The device flow needs a person in a browser, so they get their own path: Task 4b, which must land BEFORE Task 5 deletes `account ensure` and Task 6 deletes `LoginIntent::Submit`.
- **`just dev-r3` depends on `_dev-login-setup`** like `dev-r2` and needs the same issuer env (Task 5).
- **Overlap with `2026-11-13-r4-takeover.md`:** its Task 17 (`LoginSession::rejoin`) edits `crates/client/src/login/{flow,api}.rs` (as Task 6 here) and its Task 15 edits the `dev-r3` / `go-nogo-r3` recipes (as Tasks 4b and 5 here). The logic does not conflict — a rejoin is a `/v1/join` with the kept refresh token, which this plan keeps — but never run the two plans' implementers in those files at the same time. Land this plan before R4's Phase D; whichever lands second rebases on the other's `LoginCall` / `LoginReply`.
- The plan files are untracked in git; do not `git add` them unless asked.

## Decisions settled in planning

1. **Keycloak is the source of player identity, not the login service** (user). The login service keeps what is game-specific: the world registry, `world_players`, sessions and epochs, pending kicks, its refresh tokens and the join token.
2. **An account is identified by the token's `sub`**, stored in a new `accounts.subject` column. `account_id` (i64) stays the key everywhere else, so `JoinClaims`, the hosts and the world store do not change.
3. **Existing accounts are adopted by email**: the first OIDC login whose verified email equals an account with no subject takes that account over (keeps its players). Tokens with `email_verified != true` are refused.
4. **The player's name is Keycloak's username** (`preferred_username`), stored as `accounts.display_name` and refreshed at each login. It is shown where the email is shown today. Putting it in game (`JoinClaims`, name plates) is not in this plan.
5. **Device flow on native and in the browser** (RFC 8628): the game shows a short code and opens `verification_uri_complete`; the player signs in at Keycloak; the game polls. No loopback HTTP server and no redirect handling in the client.
6. **Only the login-service refresh token is kept on disk**, exactly as today. Keycloak tokens are used once and dropped.
7. **The password paths are deleted**, not hidden: `/v1/register`, `/v1/login`, `/v1/verify`, `/v1/reset/*`, the HTML pages, Argon2, the mailers and the AWS SES dependency of `login`.
8. **Single-player and `VOXEL_AUTH=token` are untouched.**
9. **Known limit, accepted for now:** disabling a user in Keycloak does not end their login-service session (refresh tokens live 30 days). `login account revoke <email>` is the operator's tool until a later plan adds Keycloak logout events.
10. **A launch with no screens starts from a refresh token the operator issued** (added 2026-11-13). Three options: a password grant on the dev realm's `game` client (every probe run then needs the Keycloak container, and the dev realm stops matching production); keeping `/v1/login` for dev only (a password path that must never ship); **chosen:** `login account issue-refresh <email>` — a ctl command, so it needs the login database itself — makes the account if missing and prints one refresh token; the recipe writes it to the file the client already keeps its token in (`VOXEL_LOGIN_REFRESH_FILE`), and `client debug online` starts from the kept token exactly as a returning player does. The account has no subject, so a later Keycloak login as `dev` adopts it by email (Decision 3): one dev account either way.
11. **Which dev stacks need Keycloak** (added 2026-11-13): those with login screens — `dev-login`, `dev-r2`, `dev-r3` (they depend on `kc-up` and set `VOXEL_OIDC_ISSUER`). `run-online` and `go-nogo-r3` do not: their login service runs with no issuer (`/v1/config` answers 404, `/v1/refresh` and `/v1/join` work), so the probe needs no container beyond Postgres.

## Global Constraints

- Work on `develop` in the main checkout. Never create a git worktree. `git add` only a task's own files.
- `login-api` stays serde-only and wasm-safe. The client never depends on `auth`, `login`, `sqlx` or AWS crates.
- The workspace's only rustls crypto provider is **ring**: no dependency or feature that pulls `aws-lc-rs` (so: no `openidconnect` / `oauth2` crates; hand-rolled calls over the workspace `reqwest`).
- Runtime `sqlx::query()` only; every migration exists for both engines (`crates/login/migrations/{pg,sqlite}/`) and `schema::schemas_match` must pass.
- Verify with `just test-one …` for touched tests and `just smoke`; never whole suites. Client tests (`just test-one client "--lib login::"`) need the user's go-ahead first.
- Tag tests slower than 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`.
- Keep the `justfile` and `CLAUDE.md` in sync in the task that changes a command or env var.
- Exact values: audience `mmo-login`; client id `game`; scope `openid profile email`; device grant type `urn:ietf:params:oauth:grant-type:device_code`; leeway `auth::JWT_LEEWAY_SECS` (30); JWKS refetch at most every 30 s.

## Review Focus

1. **A token minted for another client** (e.g. Zulip's) is presented to `/v1/login/oidc`. Expected: refused, because its `aud` lacks `mmo-login`. Test in Task 3.
2. **Two people, one email**: an OIDC login whose email matches an account that already has a different subject. Expected: refused with a clear error, never merged. Test in Task 2.
3. **Keycloak rotates its signing key** while the login service runs. Expected: the unknown `kid` triggers one refetch and the login succeeds. Test in Task 3.
4. **The player closes the browser or never confirms.** Expected: the game shows "Sign-in expired", returns to the login screen and can start again; no thread or task keeps polling. Test in Task 6.
5. **The username changes or contains odd characters** (Keycloak allows what its profile allows; an operator can rename). Expected: `display_name` follows the token on the next login and is capped at 64 chars when stored. Test in Task 2.
6. **A launch with no screens holds a token the service refuses** (expired, revoked, a stale file from another database). Expected: `client debug online` prints why and exits non-zero within the launch, never sits on the login screen (the probe would report INVALID after its whole timeout). Test in Task 4b.

## File structure

```
crates/login-api/src/lib.rs                 + OidcLoginRequest, LoginConfigResponse, paths, LoginResponse.account_name
crates/login/migrations/{pg,sqlite}/0004_oidc_accounts.sql
crates/login/src/store.rs                   + upsert_oidc_account, account_by_subject
crates/login/src/oidc.rs                    new: token verification + key cache
crates/login/src/config.rs, api.rs, lib.rs  OIDC config, two routes; password routes removed
crates/login/src/{password,mail,pages}.rs   deleted (Task 5)
crates/client/src/login/oidc.rs             new: device flow calls
crates/client/src/login/{api,flow}.rs       login by device flow
crates/slint-ui/ui/login.slint              code + "open browser" instead of email / password
crates/login/src/ctl.rs                     + `account issue-refresh` (Task 4b); password commands removed (Task 5)
crates/client/src/debug_launch.rs           `debug online` starts from the kept refresh token (Task 4b)
deploy/dev/keycloak/players-realm.json      + the `game` client (the dev Keycloak itself exists)
justfile, scripts/dev_r3.sh (comments only), CLAUDE.md, TODO.md
```

---

### Task 1: Wire types

**Files:**
- Modify: `crates/login-api/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub const PATH_LOGIN_OIDC: &str = "/v1/login/oidc";` and `pub const PATH_CONFIG: &str = "/v1/config";`
  - `pub struct OidcLoginRequest { pub access_token: String }`
  - `pub struct LoginConfigResponse { pub oidc_issuer: String, pub oidc_client_id: String }`
  - `LoginResponse` gains `pub account_name: String` (`#[serde(default)]`).
  - `ApiErrorCode` gains `IdentityRefused` (bad / foreign / unverified token) and `IdentityConflict` (Review Focus 2).

- [ ] **Step 1: Write the failing test** in the crate's unit tests: `oidc_types_serialize_snake_case` asserts `serde_json::to_string(&ApiErrorCode::IdentityConflict)` is `"\"identity_conflict\""`, and that `LoginResponse` JSON without `account_name` deserializes with `account_name == ""`.
- [ ] **Step 2: Run** `just test-one login-api "oidc_types"` — expected: fails to compile (types missing).
- [ ] **Step 3: Add the items above.**
- [ ] **Step 4: Run again** — expected: PASS.
- [ ] **Step 5: Commit** `feat(login-api): OIDC login request, config answer, account name`.

### Task 2: Accounts by subject

**Files:**
- Create: `crates/login/migrations/pg/0004_oidc_accounts.sql`, `crates/login/migrations/sqlite/0004_oidc_accounts.sql`
- Modify: `crates/login/src/store.rs` (`AccountRow`, new fns)
- Test: `crates/login/tests/store.rs`, `crates/login/tests/schema.rs`

**Interfaces:**
- Produces:
  - Columns `accounts.subject TEXT` (nullable) with a unique index `accounts_subject` over non-null values, and `accounts.display_name TEXT NOT NULL DEFAULT ''`.
  - `AccountRow` gains `subject: Option<String>`, `display_name: String`.
  - `pub const DISPLAY_NAME_MAX_CHARS: usize = 64;`
  - `pub enum OidcUpsert { Created(AccountId), Existing(AccountId), Adopted(AccountId), EmailTaken }`
  - `pub async fn upsert_oidc_account(&self, subject: &str, email: &str, display_name: &str, now: i64) -> Result<OidcUpsert, StoreError>`
  - `pub async fn account_by_subject(&self, subject: &str) -> Result<Option<AccountRow>, StoreError>`

Rules for `upsert_oidc_account`, in one transaction: a row with this subject → update its `email` and `display_name`, `Existing`; else a row with this (normalised) email and `subject IS NULL` → set subject, `display_name`, `email_verified_at = COALESCE(email_verified_at, now)`, `Adopted`; else a row with this email and another subject → `EmailTaken`; else insert (`password_hash = ''`, `email_verified_at = now`) → `Created`. `display_name` is truncated to `DISPLAY_NAME_MAX_CHARS` chars.

- [ ] **Step 1: Write the failing tests** in `tests/store.rs`:
  - `oidc_first_login_creates_account`: `Created(id)`, then `account_by_subject("s1")` has that id, `display_name == "ann"`, `email_verified_at.is_some()`.
  - `oidc_second_login_updates_name_and_email`: second call with the same subject and name `"ann2"` → `Existing(same id)`, row shows `"ann2"`.
  - `oidc_adopts_password_account_by_email`: `create_account("a@x.io", "hash", t)` then upsert `("s1", "a@x.io", …)` → `Adopted(that id)`; its `world_players` row made before the upsert still resolves through `player_token_for`.
  - `oidc_email_of_another_subject_is_refused`: upsert `("s1", "a@x.io")` then `("s2", "a@x.io")` → `EmailTaken`, and `account_by_subject("s2")` is `None`.
  - `oidc_display_name_is_capped`: a 200-char name is stored as 64 chars.
- [ ] **Step 2: Run** `just test-one login "--test it store::oidc_"` — expected: compile failure.
- [ ] **Step 3: Write both migrations and the store code.** SQLite: `ALTER TABLE accounts ADD COLUMN …` twice plus `CREATE UNIQUE INDEX accounts_subject ON accounts(subject) WHERE subject IS NOT NULL`; Postgres the same statements.
- [ ] **Step 4: Run** `just test-one login "--test it store::oidc_"`, `just test-one login "--test it schema::"` and `just test-login-pg "--test it store::oidc_"` — expected: PASS (the Postgres run skips without `MMO_TEST_DATABASE_URL`; say so if it did).
- [ ] **Step 5: Commit** `feat(login): accounts keyed by an identity-provider subject, adopted by verified email`.

### Task 3: Verifying Keycloak's access token

**Files:**
- Create: `crates/login/src/oidc.rs`, `crates/login/tests/oidc.rs`, `crates/login/tests/fixtures/oidc_rsa.pem`, `crates/login/tests/fixtures/oidc_jwks.json`
- Modify: `crates/login/src/lib.rs` (`pub mod oidc;`), `crates/login/tests/it.rs` (`mod oidc;`), `crates/login/Cargo.toml` (`jsonwebtoken`, `reqwest` from the workspace)

**Interfaces:**
- Produces:
  - `pub struct Identity { pub subject: String, pub email: String, pub name: String }`
  - `pub enum OidcError { Malformed, UnknownKid, BadSignature, Expired, WrongIssuer, WrongAudience, EmailUnverified, MissingClaim(&'static str), Fetch(String) }`
  - `pub struct OidcVerifier` with
    - `pub fn from_jwks(issuer: &str, audience: &str, jwks_json: &str) -> Result<Self, OidcError>` (tests, no network)
    - `pub async fn discover(issuer: &str, audience: &str) -> Result<Self, OidcError>` (GET `{issuer}/.well-known/openid-configuration`, then its `jwks_uri`; 5 s timeout)
    - `pub fn verify(&self, access_token: &str, now_secs: u64) -> Result<Identity, OidcError>`
    - `pub async fn verify_refreshing(&self, access_token: &str, now_secs: u64) -> Result<Identity, OidcError>` (on `UnknownKid`, refetch the keys at most once per `JWKS_REFETCH_MIN_SECS = 30`, then verify again)
- Verification rules: algorithm RS256 only; `iss` equals the issuer exactly; `aud` (string or array) contains the audience; `exp` with 30 s leeway; `email_verified == true`; `sub`, `email`, `preferred_username` present and non-empty (`MissingClaim`). `Identity.email` is passed through `normalize_email`'s logic (move that one function from `password.rs` into `store.rs` first; Task 5 deletes `password.rs`).

- [ ] **Step 1: Make the fixtures** (committed; a test key, never used elsewhere):
  `openssl genrsa -out crates/login/tests/fixtures/oidc_rsa.pem 2048`, then write `oidc_jwks.json` with one key (`kty` RSA, `alg` RS256, `use` sig, `kid` `test-1`, `n` / `e` from `openssl rsa -in … -noout -modulus`, base64url without padding).
- [ ] **Step 2: Write the failing tests** in `tests/oidc.rs`, with a helper `token(claims: serde_json::Value, kid: &str) -> String` that signs with the fixture key:
  - `accepts_a_good_token` → `Identity { subject: "u-1", email: "ann@x.io", name: "ann" }` (the token's email is `" Ann@X.io "`).
  - `refuses_wrong_audience` (aud `"account"`) → `WrongAudience` (Review Focus 1); `accepts_audience_array` (`["account","mmo-login"]`).
  - `refuses_wrong_issuer`, `refuses_expired` (exp = now − 31), `accepts_within_leeway` (exp = now − 29).
  - `refuses_unverified_email` → `EmailUnverified`; `refuses_missing_username` → `MissingClaim("preferred_username")`.
  - `refuses_hs256_token` (same claims signed HS256 with the public modulus as secret) → `Malformed` or `BadSignature`, never `Ok`.
  - `unknown_kid_is_reported` (kid `test-2`) → `UnknownKid` (Review Focus 3; the refetch path is exercised in Task 4's test with a local axum JWKS server).
- [ ] **Step 3: Run** `just test-one login "--test it oidc::"` — expected: compile failure.
- [ ] **Step 4: Implement `oidc.rs`.** Keys as `HashMap<String, DecodingKey>` built with `DecodingKey::from_rsa_components(n, e)`; behind a `tokio::sync::RwLock` with the last-fetch instant for the refresh path.
- [ ] **Step 5: Run again** — expected: PASS.
- [ ] **Step 6: Commit** `feat(login): verify identity-provider access tokens (RS256, issuer, audience, verified email)`.

### Task 4: `POST /v1/login/oidc` and `GET /v1/config`

**Files:**
- Modify: `crates/login/src/config.rs` (`LoginConfig`), `crates/login/src/api.rs` (`AppState`, `router()`, two handlers), `crates/login/src/lib.rs` (`serve`)
- Test: `crates/login/tests/api.rs`

**Interfaces:**
- Consumes: Task 1 types, Task 2 `upsert_oidc_account`, Task 3 `OidcVerifier`.
- Produces:
  - `LoginConfig` gains `oidc: Option<OidcConfig>`; `pub struct OidcConfig { pub issuer: String, pub audience: String, pub client_id: String }` from env `VOXEL_OIDC_ISSUER` (unset = `None`), `VOXEL_OIDC_AUDIENCE` (default `mmo-login`), `VOXEL_OIDC_CLIENT_ID` (default `game`).
  - `AppState` gains `oidc: Option<Arc<OidcVerifier>>` and the `OidcConfig`.
  - `POST /v1/login/oidc` (`OidcLoginRequest` → `LoginResponse`): verify (refreshing) → `upsert_oidc_account` → on `EmailTaken` answer 409 `IdentityConflict`; on any `OidcError` answer 401 `IdentityRefused` (log the variant, do not echo it); otherwise issue a refresh token exactly as `login` does today (`insert_refresh`) and answer `LoginResponse { refresh_token, account_email, email_verified: true, account_name }`. `refresh` fills `account_name` from the row too.
  - `GET /v1/config` → `LoginConfigResponse`; 404 when OIDC is not configured.
  - `serve`: with `oidc` set, call `OidcVerifier::discover` at startup; a failure is fatal with the issuer URL in the message.

- [ ] **Step 1: Write the failing tests** in `tests/api.rs` (the test app builder gains an `OidcVerifier::from_jwks` over the Task 3 fixture):
  - `oidc_login_creates_account_and_returns_refresh`: 200, non-empty `refresh_token`, `account_name == "ann"`; then `POST /v1/join` with it reaches the same outcome a password login's token does today (reuse the existing join test's assertions).
  - `oidc_login_with_foreign_audience_is_401` → body code `identity_refused`.
  - `oidc_login_email_conflict_is_409` → `identity_conflict`.
  - `oidc_login_refetches_keys_on_unknown_kid`: a local axum server serves discovery + a JWKS that gains kid `test-2` after the first request; a `discover`ed verifier accepts a `test-2` token on the first try.
  - `config_reports_issuer_and_client`: `{ "oidc_issuer": …, "oidc_client_id": "game" }`.
- [ ] **Step 2: Run** `just test-one login "--test it api::oidc_"` and `"--test it api::config_"` — expected: FAIL (404 / compile).
- [ ] **Step 3: Implement** config, state, handlers and the startup discovery.
- [ ] **Step 4: Run again**, plus the whole login `it` binary once (`just test-one login "--test it"`) since `AppState` changed — expected: PASS.
- [ ] **Step 5: Commit** `feat(login): log in with an identity-provider token; /v1/config tells clients where it is`.

### Task 4b: A login with no screens (`run-online`, the go / no-go probe)

Added 2026-11-13 (Decision 10). Lands before Task 5: after this task nothing outside the login screens uses a password.

**Files:**
- Modify: `crates/login/src/ctl.rs` (`USAGE`, `run_ctl_with`), `crates/login/src/api.rs` (make the token minting reachable: see Interfaces), `crates/login/tests/ctl.rs`
- Modify: `crates/client/src/debug_launch.rs` (`OnlineLogin`, `online_login_from`, `LOGIN_EMAIL_ENV`, `LOGIN_PASSWORD_ENV`, `USAGE`, the module doc, `drive_online_login`, the launch's `online` field near line 391, the tests near line 898)
- Modify: `justfile` (`dev_password` and its comment, `_dev-login-setup`, `run-online`, `go-nogo-r3`), `TODO.md` (the `run-online` line: no password any more)

**Interfaces:**
- Produces, login: `pub async fn mint_refresh(store: &LoginStore, account: AccountId, now: i64) -> Result<String, LoginError>` in `api.rs` — a new token of a new family, stored by `insert_refresh` with the normal expiry (`REFRESH_TTL_DAYS`); the handlers' private `issue_refresh` calls it, so a ctl token and a login token are made by one function.
- Produces, ctl: `login account issue-refresh <email>` — normalises the email; if no account has it, `create_account(email, "", now)` and mark it verified; then `mint_refresh` and write the token plus `\n` to `out`, and nothing else (the recipe redirects it into a file). It reads no stdin. `USAGE` gains `account issue-refresh <email>   (prints a new refresh token; creates the account if missing)`.
- Produces, client: `pub fn online_login_from(login_url: Option<&str>, kept: Option<&str>) -> Result<(), String>` — errors: no `login_url` → names `VOXEL_LOGIN_URL` (as today); no kept token → `` `debug online` needs a kept login: set VOXEL_LOGIN_REFRESH_FILE to a file made by `login account issue-refresh` ``. `OnlineLogin`, `LOGIN_EMAIL_ENV` and `LOGIN_PASSWORD_ENV` are removed; the kept token is read with `login::store::load_refresh()`.
- `drive_online_login`, step `Login` (state `AppState::Login`, reached as today): it sends no intent — entering the login screens already sends the silent `LoginCall::Refresh` of the kept token — and moves to `LoggingIn`. In `LoggingIn`, besides today's rule: state still `AppState::Login`, `!login.busy()`, `!login.logged_in()` → fail with `login failed: the kept login was refused` when `login.error` is empty, else with the error (a refused kept token is dropped silently by `flow.rs`, so the error line alone does not show it). Pull that decision into a pure `fn online_login_verdict(state: AppState, busy: bool, logged_in: bool, error: &str) -> Option<Result<(), String>>` (`None` = wait) so it tests without a Bevy app.
- Recipes: `dev_password` goes. `_dev-login-setup` ends with `$login account issue-refresh {{dev_email}} >target/dev-login/refresh.tmp && chmod 600 target/dev-login/refresh.tmp && mv target/dev-login/refresh.tmp target/dev-login/refresh` in place of `account ensure`. `run-online` runs the client with `VOXEL_LOGIN_URL=$login_url VOXEL_LOGIN_REFRESH_FILE=$PWD/target/dev-login/refresh`. `go-nogo-r3` does the same under `$out/login/refresh` (its own login database). The file is rewritten at every start: the client rotates the token in it while it runs.

- [ ] **Step 1: Write the failing tests.**
  - `crates/login/tests/ctl.rs`: `issue_refresh_creates_a_verified_account_and_a_working_token` (run `account issue-refresh dev@localhost` on an empty database: the output is one line; `account_by_email` is verified with `subject == None`; `POST /v1/refresh` with that line answers 200 through the test app of `tests/api.rs`); `issue_refresh_twice_keeps_one_account_and_both_tokens_work` (two different tokens, one account row, the first still refreshes — a second family, not a rotation); `issue_refresh_prints_only_the_token` (the output has no spaces and matches the length of a login's `refresh_token`).
  - `crates/client/src/debug_launch.rs` tests, replacing the `online_login_from` cases: `online_needs_a_login_url_and_a_kept_token` (the two error texts above; `Ok(())` with both); `online_verdict_table`: `(Login, busy)` → `None`; `(Worlds, idle, logged in)` → `Some(Ok(()))`; `(Login, idle, not logged in, "")` → `Some(Err("login failed: the kept login was refused"))`; `(Login, idle, not logged in, "network: …")` → an `Err` holding that text (Review Focus 6).
- [ ] **Step 2: Run** `just test-one login "--test it ctl::issue_refresh"` — expected: FAIL (unknown command). **Ask the user** before the client test, then `just test-one client "--lib debug_launch::"` — expected: compile failure.
- [ ] **Step 3: Implement** the ctl command, `mint_refresh`, the client change and the three recipes.
- [ ] **Step 4: Run** the Step 2 commands and `just smoke` — expected: PASS. `just --dry-run run-online 2>&1 | grep -c VOXEL_LOGIN_PASSWORD` prints `0`; `grep -c "account ensure" justfile` prints `0`.
- [ ] **Step 5: Live check — stop and ask the user first** (a client launch and a measurement tool): `just run-online` lands in the world as `dev@localhost` with no screens, twice in a row (the second run re-issues the token and returns to the same fortress). `just go-nogo-r3 0 20` reaches a verdict (any of exit 0 / 1; exit 2 or 3 with a login error in `target/go-nogo-r3/` is this task's bug).
- [ ] **Step 6: Commit** `feat(login,client): a launch with no screens starts from an operator-issued refresh token`.

### Task 5: The dev realm's `game` client, then delete the password paths

**Files:**
- Modify: `deploy/dev/keycloak/players-realm.json` (add the `game` client only), `deploy/dev/README.md`, `justfile` (`dev-login`, `dev-r2`, `dev-r3`, the `login` recipe's help comment; `kc-up` / `kc-down` exist), `scripts/dev_r3.sh` (header comment: the stack needs `just kc-up`, which `dev-r3` runs), `CLAUDE.md` (Auth bullet)
- Delete: `crates/login/src/password.rs`, `mail.rs`, `pages.rs`; their tests (`tests/password.rs`, `tests/mail.rs`, the password / register / verify / reset cases in `tests/api.rs`, `tests/kicks.rs` cases driven by password reset)
- Modify: `crates/login/src/api.rs`, `lib.rs`, `config.rs`, `ctl.rs`, `store.rs`, `crates/login/Cargo.toml`, `crates/login-api/src/lib.rs`

**Interfaces:**
- Consumes: Task 4b (no recipe or launch uses `account ensure` or a password any more).
- Produces: in the existing dev realm `players`, client `game`, mirroring `deploy/modules/keycloak-realm/clients.tf`: `publicClient` true, `standardFlowEnabled` true with attribute `pkce.code.challenge.method` = `S256`, `directAccessGrantsEnabled` false, attribute `oauth2.device.authorization.grant.enabled` = `"true"`, `webOrigins` and `redirectUris` (`…/*`) for `http://127.0.0.1:1334` and `http://localhost:1334`, one protocol mapper `login-service-audience` (`oidc-audience-mapper`, `included.custom.audience` = `mmo-login`, `access.token.claim` = `"true"`). The `site` client, the user `dev` (password `dev-password`, in the realm file only) and the compose service stay as they are.
- Produces: dev issuer `http://127.0.0.1:8180/realms/players`; `dev-login`, `dev-r2` and `dev-r3` export `VOXEL_OIDC_ISSUER` to it and depend on `kc-up`. `run-online` and `go-nogo-r3` set no issuer (Decision 11).
- Removes: routes `/v1/register`, `/v1/login`, `/v1/verify`, `/v1/reset/request`, `/v1/reset/confirm`, pages `/verify`, `/reset`; `RegisterRequest`, `LoginRequest`, `VerifyRequest`, `ResetRequest`, `ResetConfirm` and their path constants; `LoginConfig.require_verified`, `.mailer`; env `VOXEL_MAILER`, `VOXEL_MAIL_FROM`, `VOXEL_LOGIN_REQUIRE_VERIFIED`; ctl `account set-password|ensure|verify`; store fns `set_password*`, `reset_password`, `mark_verified` and the email-token fns (`create_account` stays: Task 2's adoption test uses it); crates `argon2`, `aws-sdk-sesv2` and the smithy HTTP client from `login`.
- Keeps: tables and columns as they are (no destructive migration); `revoke_account_with_kicks`; ctl gains `account revoke <email>` (calls it) for Decision 9.

- [ ] **Step 1: Add the `game` client to the dev realm file.**
  Run: `just kc-down && just kc-up`, then `curl -s -d client_id=game -d 'scope=openid profile email' http://127.0.0.1:8180/realms/players/protocol/openid-connect/auth/device | grep -c user_code` — expected: `1`; and `just site-theme-check` — expected: still PASS (the `site` client and the theme are untouched).
- [ ] **Step 2: Prove the server path end to end before deleting anything.** With `just dev-login` running: get a token by device flow by hand (`curl -d client_id=game -d 'scope=openid profile email' …/protocol/openid-connect/auth/device`, open the printed `verification_uri_complete`, sign in as `dev`, then the token call), post it to `/v1/login/oidc` — expected: 200 with `account_name` `dev`.
- [ ] **Step 3: Write the failing test** `password_routes_are_gone` in `tests/api.rs`: `POST /v1/login`, `/v1/register`, `/v1/reset/request` each answer 404. Add `account_revoke_ends_sessions` in `tests/ctl.rs`.
- [ ] **Step 4: Run** `just test-one login "--test it api::password_routes_are_gone"` — expected: FAIL (the routes answer).
- [ ] **Step 5: Delete** everything under "Removes", fix the fallout (`CtlDeps.hasher` and `.stdin` go with the password commands; `account issue-refresh` from Task 4b stays), update the justfile comments and the `CLAUDE.md` Auth bullet (`jwt` = login service + Keycloak; name the three `VOXEL_OIDC_*` variables and `login account issue-refresh`). The client still compiles against `LoginRequest` / `RegisterRequest` until Task 6: keep `LoginApi::login`, `register` and `request_reset` building in this task by moving those three request structs into `crates/client/src/login/api.rs` as private types (Task 6 deletes them with their callers), so this commit and the next each pass `just smoke`.
- [ ] **Step 6: Run** `just test-one login "--test it"`, `just test-one server "--test it auth_edge::"`, `just smoke` — expected: PASS; `cargo tree -p login | grep -c -E 'argon2|aws-'` prints `0`; `just --dry-run dev-r3 2 2>&1 | grep -c VOXEL_OIDC_ISSUER` prints at least `1`.
- [ ] **Step 7: Commit** `feat(login)!: Keycloak owns passwords; register / password / verify / reset removed; dev Keycloak in compose`.

### Task 6: Client device flow

**Files:**
- Create: `crates/client/src/login/oidc.rs`
- Modify: `crates/client/src/login/api.rs` (`LoginApi`), `crates/client/src/login/flow.rs` (`LoginIntent`, `LoginCall`, `LoginReply`, `LoginSession`, `start_login`, `poll_login`), `crates/client/src/login/mod.rs`

**Interfaces:**
- Consumes: `PATH_CONFIG`, `PATH_LOGIN_OIDC`, `OidcLoginRequest`, `LoginConfigResponse`.
- Produces in `oidc.rs` (pure data + parsing, no IO, so it tests without a client build of the world):
  - `pub struct DeviceStart { pub device_code: String, pub user_code: String, pub verification_uri: String, pub verification_uri_complete: String, pub expires_in: u64, pub interval: u64 }` (`interval` defaults to 5 when absent)
  - `pub enum DevicePoll { Pending, SlowDown, Token(String), Expired, Denied, Failed(String) }`
  - `pub fn parse_device_start(body: &str) -> Result<DeviceStart, String>`
  - `pub fn parse_device_poll(status: u16, body: &str) -> DevicePoll` (200 → `Token(access_token)`; error `authorization_pending` → `Pending`; `slow_down` → `SlowDown`; `expired_token` → `Expired`; `access_denied` → `Denied`; anything else → `Failed`)
  - `pub fn next_interval(current: u64, poll: &DevicePoll) -> u64` (+5 on `SlowDown`, else unchanged)
- Produces in `api.rs`: `LoginApi::config()`, `LoginApi::device_start(issuer, client_id)`, `LoginApi::device_poll(issuer, client_id, device_code)`, `LoginApi::login_oidc(access_token)`. Endpoints are `{issuer}/protocol/openid-connect/auth/device` and `…/token`, form-encoded.
- Produces in `flow.rs`: `LoginCall::Login { .. }`, `Register`, `Reset` are removed; new `LoginCall::DeviceStart`, `LoginCall::DevicePoll { device_code: String }`, `LoginCall::LoginOidc { access_token: String }`; `LoginIntent::SignIn`, `LoginIntent::OpenBrowser`, `LoginIntent::CancelSignIn` replace the submit / register / forgot intents; `LoginSession` gains `pending_device: Option<PendingDevice { start: DeviceStart, deadline: f64, next_poll: f64, interval: u64 }>` and `account_name: String`.
- Unchanged on purpose: `LoginIntent::PlayOnline`, `JoinWorld`, `Back`, `Logout`, `LoginCall::Refresh` / `Join` and the silent refresh of a kept token — `client debug online` (Task 4b) and R4's rejoin (`2026-11-13-r4-takeover.md` Task 17) both rest on them. If R4's Task 17 has landed first, `LoginSession::rejoin` and its `LoginCall` are kept as they are.
- Behaviour: `SignIn` → `DeviceStart`; on its reply store `pending_device`, and on native open the browser at `verification_uri_complete` (`open` crate, new native-only dependency `open = "5"`); `poll_login` issues one `DevicePoll` when `now >= next_poll` and none is in flight; `Token` → `LoginOidc` → the existing post-login path (save the refresh token, fetch worlds); `Expired` / `Denied` / deadline passed → clear `pending_device`, error text `"Sign-in expired"` / `"Sign-in was refused"`; `CancelSignIn` and leaving the login screen clear it (register it with the session reset so nothing polls after the screen is gone).

- [ ] **Step 1: Write the failing tests** (unit tests in `oidc.rs`): `parses_device_start` (Keycloak's real field set), `interval_defaults_to_five`, `poll_pending`, `poll_slow_down_adds_five_seconds` (`next_interval(5, &SlowDown) == 10`), `poll_token`, `poll_expired`, `poll_denied`, `poll_garbage_is_failed`. In `flow.rs`'s tests: `expired_device_clears_pending_and_reports` and `cancel_stops_polling` (Review Focus 4), driving the existing reply-handling functions directly.
- [ ] **Step 2: Ask the user** before running client tests, then run `just test-one client "--lib login::"` — expected: compile failure.
- [ ] **Step 3: Implement.** Native calls run on the existing `login-http` thread, wasm through the existing `spawn_local` path; no new thread or task type.
- [ ] **Step 4: Run** the same command and `just smoke` — expected: PASS.
- [ ] **Step 5: Commit** `feat(client): sign in through the identity provider's device flow`.

### Task 7: Login screen

**Files:**
- Modify: `crates/slint-ui/ui/login.slint` (`LoginScreen`), `crates/slint-ui/ui/worlds.slint` (the "Signed in as" line), `crates/client/src/frontend/mod.rs:175-180` (wiring), `crates/slint-ui/tests/login_screen.rs`

**Interfaces:**
- Consumes: Task 6 intents and `LoginSession.pending_device` / `.account_name`.
- Produces `LoginScreen`: properties `in user-code: string`, `in verify-url: string`, `in waiting: bool`, `in error: string`, `in notice: string`, `in busy: bool`; callbacks `sign-in()`, `open-browser()`, `cancel()`, `back()`. The `email`, `password`, `show-password` properties and the `submit`, `register`, `forgot` callbacks are removed.
- Copy (exact): button `Sign in`; while waiting, heading `Finish signing in in your browser`, line `If asked, enter this code:` above the code, buttons `Open browser` and `Cancel`; footnote `New here? You can create an account on the sign-in page.` `worlds.slint`: `Signed in as <account_name>` (falls back to the email when the name is empty).

- [ ] **Step 1: Write the failing tests** in `crates/slint-ui/tests/login_screen.rs`, replacing the email / password cases: `idle_shows_sign_in_only` (no code text, `sign-in` fires on click), `waiting_shows_code_and_buttons` (`user-code = "ABCD-EFGH"` is visible, `open-browser` and `cancel` fire), `error_is_shown`.
- [ ] **Step 2: Run** `just test-one slint-ui "--test it -- --include-ignored login_screen::"` — expected: FAIL.
- [ ] **Step 3: Rewrite the screen** (root calls `GameTheme.apply()` in `init`; nothing bound to `visible` / `opacity` on a component root) and the frontend wiring. In the browser build `open-browser` opens `verify-url` in a new tab from the click itself (a user gesture), which is why the button exists on every platform.
- [ ] **Step 4: Run** the same command, `just slint-wasm` and `just smoke` — expected: PASS.
- [ ] **Step 5: End to end, by hand** (`just kc-up`, `just dev-login` in one terminal, `just run-online` in another): Sign in → browser opens Keycloak → sign in as `dev` → the game lists worlds and shows `Signed in as dev` → Join enters the world. Restart the client: it goes straight to the world list (kept refresh token). Repeat once in the browser build (`just web`) and confirm the device and token calls are not blocked by CORS; if they are, the fix is the `game` client's `web_origins` (the dev realm file here, `clients.tf` in plan 1), not a proxy.
- [ ] **Step 6: Commit** `feat(client): login screen hands off to the browser`.

## Self-review notes

- Decisions → tasks: 1, 2, 3 → Tasks 2–4; 4 → Tasks 1, 2, 7; 5 → Tasks 6, 7; 6 → Task 6 (no new stored token); 7 → Task 5; 8 → untouched by construction (`AuthMode::Local` paths are not edited); 9 → Task 5 (`account revoke`).
- Amendments of 2026-11-13: Decisions 10 and 11 → Task 4b (headless launches) and Task 5 (which stacks need Keycloak); Review Focus 6 → Task 4b's `online_verdict_table`. Task order is 1, 2, 3, 4, 4b, 5, 6, 7; every commit passes `just smoke` (Task 5 Step 5 says how the client survives the removal of the request types until Task 6).
- Names added: `mint_refresh` (4b, used by the handlers and the ctl), `account issue-refresh` (4b → 5's docs), `online_login_from(login_url, kept)`, `online_login_verdict` (4b only).
- Not verified while amending: the exact JSON attribute spelling Keycloak 26.8 imports for the device grant and the audience mapper (Task 5 Step 1's `curl` settles it; the fallback is to set them once in the dev admin console and copy them from a partial export); that entering `AppState::Login` on a `debug online` launch already sends the silent refresh (if the preset reaches that screen another way, it writes `LoginIntent::PlayOnline` itself).
- Hosts and `auth` are not modified: `JoinClaims` is unchanged, so `server` needs no task; Task 5 Step 6 runs its auth tests as the guard.
- Names checked across tasks: `OidcLoginRequest`, `LoginConfigResponse`, `PATH_LOGIN_OIDC`, `PATH_CONFIG`, `account_name` (1 → 4, 6, 7); `OidcUpsert`, `upsert_oidc_account` (2 → 4); `OidcVerifier::{from_jwks, discover, verify_refreshing}`, `Identity` (3 → 4); `DeviceStart`, `DevicePoll`, `LoginIntent::{SignIn, OpenBrowser, CancelSignIn}` (6 → 7).
- Not verified while planning: that Keycloak applies the `game` client's web origins to the device-authorization endpoint for browser calls (Task 7 Step 5 settles it); that the workspace `reqwest` `rustls-tls` feature resolves to ring in the `login` crate's graph (Task 3: `cargo tree -p login -i aws-lc-rs` must print nothing).
- Production rollout after both plans: set `VOXEL_OIDC_ISSUER=https://auth.factoriosurvivalworld.com/realms/players` on the login service; existing players register at Keycloak with the email they used before and keep their characters (Decision 3).

## As built (2026-11-13/14, `227a95e4..bc385535` on `develop`) — where it differs from the text above

- **Device flow carries PKCE** (S256 challenge on device start, verifier on every poll): Keycloak 26.8 refuses the `game` client's device request without it. Task 5 Step 1's `curl` therefore needs a `code_challenge`.
- **Adoption** (Task 2) also sets `password_hash = ''`, and for an account that was never verified revokes its refresh tokens and kicks its sessions. A returning subject whose new email belongs to another account gets `EmailTaken`.
- **`client debug online`** (Task 4b) writes `LoginIntent::PlayOnline` itself and waits one frame: entering the login screen does not refresh a kept token on native. `go-nogo-r3` issues a token before each client and exits 3 if it cannot.
- **`login account issue-refresh`** marks verified only an account it creates.
- **A key fetch failure** at `/v1/login/oidc` answers 503, not 401.
- **Browser build:** `?login=` is honoured only on a localhost page. The verification URL must be http(s). The tab is opened from the Slint callback (one frame after the click).
- **Screen:** the notice line is gone; `Sign in` while already signed in goes to the world list.
- **Task 7 Step 5 as written cannot work** (`run-online` starts from a token and refuses while `dev-login` holds 8790): use `just dev-login` + `VOXEL_LOGIN_URL=http://127.0.0.1:8790 just play`.
- **Not run by the implementers:** every client test (type-checked and hand-traced only), `run-online`, `go-nogo-r3`, `site-theme-check`, any by-hand sign-in. Rulings, deferred minors and per-task reports: `.superpowers/sdd/2026-11-12-keycloak-2-game-login/`.
