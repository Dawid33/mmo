# Main Menu Redesign — One Client for Web and Native Implementation Plan

> **As implemented 2026-11-20:** reconciled against the landed Play Online,
> coordinator HTTP/mailbox, rejoin, world-start retry, map URL, and graphics
> settings work. The later `0005_map_url` migration means world region is
> migration `0006_world_region`; the existing graphics `SettingsWindow` remains
> available from the new account Settings screen rather than being replaced.
> The regression-only repository policy supersedes this old plan's proposed
> feature tests; verification uses existing targeted builds and safe checks.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Web and native show the same menus: a main menu of Single player / Multiplayer / Settings / Exit, a single-player sub-menu (New world / Load world), a Factorio-style server browser with a region box (`eu-central-1`) listing the worlds the player can join, and a Settings screen with the account and Log in / Log out.

**Architecture:** The frontend keeps one Slint layer (`FrontendUi`) and `AppState`. Two states are added (`SinglePlayer`, `Settings`); `Worlds` becomes the server browser and opens without signing in (the world list is a public `GET /v1/worlds`); signing in happens from Join or from Settings, and the login screen returns to where it was entered from. There is ONE login service, now and later; a **region** is a world deployment (coordinator + hosts, later possibly on another cluster), recorded per world in the login service's registry and sent with each `WorldEntry`; the region box filters the list. The platform differences are two constants (`SINGLE_PLAYER_AVAILABLE`, `CAN_EXIT`); the browser's join-by-address menu goes, and `just web`'s local dev server is reached only through a dev-only, localhost-only page parameter.

**Tech Stack:** Rust, Bevy 0.19.1 states, Slint (the `@widgets` library from `../ui`, `fortress` theme), the login service (`crates/login`, sqlx on PostgreSQL and SQLite) and its DTOs (`login-api`).

**Spec:** Dawid's requests of 2026-11-15 (no separate design document), quoted:

> I want web and native to have the same client, with some slight differences. web will have single player button disabled, greyed out. Plan redesign of main menu flow. main menu will have four options, single player, multiplayer, settings and exit. Exit only applies to native also. Single player changes the menu options to new world, load world. new world goes into worldgen, load world shows worlds, this should be implemented already. Clicking multiplayer will show a server list browser, same look as factorio's. top right will have selection box with one option, the default, eu-central-1. eu-central-1 will be our current k8s deployment. the list will show the worlds that are on this server that the player can join. look at elastic region hosts plan for reference to significant changes to how servers work, the plan you are making will be executed after it. settings for now can be empty, it should show the players username if they are logged in, if not they should have a login button or log out button if logged in.

> just web shouldn't have that connect to address, just web can only play on the official k8s server. just web for local dev is an exception and should be treated as such, only in dev. there will be only one login service now and in the future, however there might be multiple regions with the world coordinator / regions host deployment, likely on a different cluster. that is far future work, for now there is one region, everything on one cluster. the login service will have to work with all of them.

**Runs after:** `docs/superpowers/plans/2026-11-15-elastic-1-hosts-as-deployments.md` (E1). What E1 changes that this plan relies on, or must not undo:
- The login service links to each world's coordinator (`worlds.host_link`); a world row has no `--quic` / `--wt`; the join's `SpawnAck` names the host (E1 Task 6, 10, 12). The client's join path (`LoginCall::Join` → `JoinTicket`) does not change shape here.
- E1 Task 12 rewrites `world-register` (prints `world add <id> --name … --link coordinator.<ns>.svc.cluster.local:6490 --link-cert …`). This plan adds `--region` to the line E1 leaves — edit E1's form, not today's.
- E1 Task 1 / 9 touch `connections.rs`, `instance.rs`, `harness.rs`, `renderer/drain_notice.rs`, `game.slint`; this plan touches none of them.
- After E1 `Reliable::SessionEnded` is dead wire, so nothing counts players online: **no Players column** (see "To confirm").
- Rejoin (`login::flow` `Rejoin`, `REJOIN_*`) is unchanged; its end states land on the browser (`Worlds`) or the login screen as today.

## Decisions settled in planning

1. **The browser opens without a sign-in.** `GET /v1/worlds` presents no token. Join while signed out opens the login screen; after the sign-in the player is back on the browser with the same world selected and presses Join again (a join spends and rotates the token: it should follow a press).
2. **"Worlds the player can join" = open worlds of the chosen region.** The client drops `WorldStatus::Closed` rows and rows of other regions. There are no per-account world permissions to filter by.
3. **One login service.** The client has one login URL: today's resolution (`login::login_url()`: `VOXEL_LOGIN_URL` at runtime / build time, the page's `{origin}/login` on an https non-loopback page, `?login=` on a localhost page), falling back to `OFFICIAL_LOGIN_URL` = `https://login.errantry.renownandruin.com`. One `LoginSession` therefore always exists (the "no login service → no Play online" branch goes); one kept refresh token serves every region.
4. **A region is a world's deployment, recorded in the login registry.** `worlds.region` (login migration `0005`, `NOT NULL DEFAULT 'eu-central-1'`), `WorldEntry.region` (`#[serde(default)]` → `eu-central-1`, so an older service still parses), `login world add … [--region <id>]` (default `eu-central-1`). The region box lists `DEFAULT_REGION` first, then every other region named by the list, sorted. Joining a world of any region is the same call: the login service links to that world's `host_link`. Today all worlds are `eu-central-1` on orion.
5. **Login returns where it came from.** `LoginSession::return_to: AppState` (`Worlds` or `Settings`): a completed sign-in and Back on the login screen both go there. Log out stays on the screen it was pressed on.
6. **Native refreshes a kept token on entering Multiplayer or Settings** (as Play online does today), never at startup (two clients on one machine). The browser keeps its startup refresh (`REFRESH_AT_STARTUP`).
7. **The web plays only on the official servers.** The player UI has no join-by-address on either platform: `JOIN_ONLY`, the join-only menu, `FrontendAction::JoinServer`, `web_endpoint::default_server_address` and the build-time `VOXEL_WEB_SERVER` (`BUILT_FOR_SERVER`) go. The web starts on the main menu (`initial_state` loses `login_first`); Single player is drawn disabled; Exit is not drawn.
8. **`just web`'s local server is a dev exception**, the browser's counterpart of `VOXEL_SKIP_MENU`: a page parameter `?dev_server=<address>`, honoured only in a debug build (`cfg!(debug_assertions)`) on a loopback page (`login_override_allowed`, the rule `?login=` uses), starts the app in `Starting` with `PendingServer(address)` — no menu. Anywhere else it is ignored with one WARN that never prints the value. `just web` prints that URL.
9. **`AppState::Worlds` keeps its name** (matched in `session.rs`, `debug_launch.rs` and the flow's tests); its doc says it is the server browser. Log out moves from the browser to Settings.
10. **A single-player session ends on the main menu** (unchanged); an online session ends on the browser (unchanged, `LoginSession::home`).

## To confirm with Dawid (the plan proceeds on the defaults above)

- Decision 2: hide closed worlds rather than list them greyed out.
- Decision 3: a native build with no `VOXEL_LOGIN_URL` talks to production login (`just play`'s Multiplayer lists the k8s world). A localhost page without `?login=` also tries production and is refused by CORS (`DEFAULT_CORS_ORIGINS` are the dev origins): its browser shows the error line — the localhost page is the dev exception.
- Decision 7: the OLD compose game's web page (`just deploy`, `scripts/web_release.sh` with `VOXEL_WEB_SERVER`) can no longer join anything. It already shows Play online against a `/login` it does not serve; this plan retires its page for good.
- Columns: Factorio's browser shows Players / Version / Tags. Only Name (and the region) is real data today. A Players column needs an online count from the login service or the coordinator — a later `WorldEntry` field.

## Global Constraints

- No git worktrees; work on `develop`. Dawid commits concurrently and has uncommitted hunks (`crates/client/**`, `justfile`, `CLAUDE.md` among them): never `git add -A`, never `git stash`; a file that also holds his hunks is staged with a filtered patch (`-U3`, `git apply --cached --recount`), never `--unidiff-zero`; check `git diff --cached --stat` before every commit.
- UI is **Slint only**. Never bind `visible` / `opacity` / shadows on a component's ROOT. Every root component calls `GameTheme.apply()` in `init` (only `FrontendUi` is a root here). An element that may be absent and must still be measured sits in a wrapper slot (the `main-slot := VerticalLayout { if …: Button {…} }` pattern of `menu.slint`).
- In-out control properties (`Dropdown.selected`, `PickList.selected`, text fields) are bound TWO-WAY by parents (`controls.slint` header rule).
- Player-facing text says **Errantry**. Exact copy (verbatim): main menu `Single player`, `Multiplayer`, `Settings`, `Exit`; single-player page title `Single player`, buttons `New world`, `Load world`, `Back`; browser title `Multiplayer`, region caption `Region`, search placeholder `Search`, column `Name`, buttons `Back`, `Refresh`, `Join`; empty list `No worlds are open in this region right now`, busy `Fetching the world list…`; details when signed out `You will be asked to sign in when you join.`, signed in `Signed in as {name}`; Settings title `Settings`, heading `Account`, `Signed in as {name}`, `Not signed in`, buttons `Log in`, `Log out`, `Back`, busy `Checking your sign-in…`.
- Constants: `login_api::DEFAULT_REGION: &str = "eu-central-1"`; `client::login::OFFICIAL_LOGIN_URL: &str = "https://login.errantry.renownandruin.com"`; `client::web_endpoint::DEV_SERVER_QUERY: &str = "dev_server"`.
- Login migrations are immutable and identical in shape in `migrations/pg` and `migrations/sqlite`; `0001`–`0004` are never edited.
- Never log a refresh token, access token, device code, PKCE verifier, ticket, JWT, or a `?dev_server=` / `?login=` value read off a non-dev page.
- The login state outlives sessions and is NOT session-reset (as today).
- Tests: Slint screens headless in `crates/slint-ui/tests/` (`just test-one slint-ui "--test it -- <file>::"`; allowed); the login service `just test-one login ""` (allowed). **Client tests are ask-first**: build them with `just test-one client "--lib --no-run"` and give Dawid the names. After every task `just smoke`; after a Slint change `just slint-wasm`; after a `cfg(wasm32)` change `just web-check`.
- The justfile is canonical: a changed command or its printed hint changes in the same commit.

## Review Focus

1. **The web build receives a Single player action anyway** (a stale queued click, a future keyboard path). Expected: nothing happens, no generator opens. Pinned in Task 2 (`single_player_is_ignored_where_unavailable`).
2. **`?dev_server=` on the production page** (a shared link). Expected: ignored, the main menu shows, the value is never logged; a release build ignores it even on localhost. Pinned in Task 2 (`dev_server_only_on_a_loopback_debug_page`).
3. **A kept token the service refuses, on entering Multiplayer or Settings.** Expected: the token is cleared, the screen shows the signed-out view, no error line, no loop of refreshes. Pinned in Task 5 (`a_refused_kept_token_on_settings_signs_out_quietly`).
4. **The world list changes under a selection** (a refresh reorders it, the world closes, the search or the region hides it). Expected: the selection follows the world id; a world no longer shown clears it and disables Join. Pinned in Task 4 (`selection_follows_the_world_id`).
5. **An older login service without `region`, or a region the client has never heard of.** Expected: a missing field reads as `eu-central-1`; an unknown region appears in the box after `eu-central-1` and its worlds are joinable like any other. Pinned in Task 3 (`a_world_entry_without_a_region_is_eu_central_1`) and Task 4 (`regions_list_the_default_first_then_the_rest`).

## File structure

| Path | Responsibility after this plan |
|---|---|
| `crates/login/migrations/{pg,sqlite}/0005_world_region.sql` (new) | `worlds.region` |
| `crates/login/src/{store,api,ctl}.rs`, `crates/login-api/src/lib.rs` | the region in the registry, the list and `world add` |
| `crates/slint-ui/ui/main-menu.slint` | the main menu's two pages (root, single player) |
| `crates/slint-ui/ui/worlds.slint` | `ServerBrowser`: region box, search, world table, details pane, bottom bar |
| `crates/slint-ui/ui/settings.slint` (new) | `SettingsScreen`: the account section |
| `crates/slint-ui/ui/controls.slint` | `PickList` gains a column header (`header: string`) |
| `crates/slint-ui/ui/frontend.slint` | screens wired: 0 menu (page via `menu-page`), 6 browser, 7 settings |
| `crates/client/src/login/{api,flow}.rs` | `OFFICIAL_LOGIN_URL`; intents `Multiplayer` / `Settings` / `LogIn` / `RefreshWorlds`; `return_to` |
| `crates/client/src/frontend/mod.rs` | actions, back targets, platform constants, browser region / selection / search |
| `crates/client/src/session.rs`, `web_endpoint.rs`, `main.rs` | `AppState::{SinglePlayer, Settings}`; `initial_state`; the dev-only `?dev_server=` |
| `crates/client/src/debug_launch.rs` | `debug online` drives the new intents |
| `justfile` (`web`, `web-release`, `world-register`, dev `world add` lines), `scripts/web_release.sh`, `CLAUDE.md` | recipes and docs |

---

### Task 1: The main menu's two pages (Slint)

**Files:**
- Modify: `crates/slint-ui/ui/main-menu.slint`, `crates/slint-ui/ui/frontend.slint`
- Test: `crates/slint-ui/tests/frontend.rs`

**Interfaces:**
- Produces, on `MainMenu` and forwarded on `FrontendUi` under the same names:
  - `in property <int> menu-page` (0 = root, 1 = single player), `in property <bool> single-player-available`, `in property <bool> can-exit`, `in property <bool> can-load`, `in property <string> menu-error`.
  - Callbacks `single-player()`, `multiplayer()`, `settings()`, `exit()`, `new-world()`, `load-world()`; page 1's Back is `FrontendUi.back()`.
  - Out coordinates (absolute centres): `single-player-button-x/y`, `multiplayer-button-x/y`, `settings-button-x/y`, `exit-button-x/y` (page 0); `new-button-x/y`, `load-button-x/y`, `menu-back-button-x/y` (page 1). `FrontendUi.back-button-x/y` returns page 1's Back when `screen == 0 && menu-page == 1`.
- Removed from both components: `join-only`, `online-available`, `server-address`, `join-server`, `play-online`, `join-button-*`, `play-online-button-*`.
- Frame title: `title` (`Errantry`) on page 0, `Single player` on page 1. Single player is `enabled: root.single-player-available` (the widget's disabled look is the grey-out). Exit sits in a slot, drawn only when `can-exit`.

- [ ] **Step 1: Write the failing tests** in `frontend.rs` (replace `main_menu_buttons_fire` and delete `join_only_menu_joins_the_typed_server`):
  - `root_menu_buttons_fire`: page 0, all available → each of the four buttons fires its callback exactly once.
  - `single_player_greyed_out_does_not_fire`: `single-player-available: false` → its click fires nothing; Multiplayer still fires.
  - `exit_is_absent_where_the_app_cannot_exit`: `can-exit: false` → a click at the `can-exit: true` Exit position fires nothing.
  - `single_player_page_offers_new_load_and_back`: `menu-page: 1` → New world, Load world and Back fire `new-world`, `load-world`, `back`; Multiplayer's page-0 position fires nothing.
  - `load_disabled_without_worlds` and `menu_error_is_shown` updated to page 1 / page 0.
- [ ] **Step 2:** `just test-one slint-ui "--test it -- frontend::"`. Expected: FAIL to compile (`set_menu_page` not found).
- [ ] **Step 3:** Rewrite `main-menu.slint` (two `VerticalFlow`s in one `FrameFace`, switched by `visible` on the flows, never on the root) and its wiring in `frontend.slint`.
- [ ] **Step 4:** Same command: PASS. `just slint-wasm`.
- [ ] **Step 5:** Do not commit yet: the client does not compile against the removed properties until Task 2. Commit Tasks 1 and 2 together.

### Task 2: States, actions, the platform differences and the dev-only local server (client)

**Files:**
- Modify: `crates/client/src/session.rs` (`AppState`, `initial_state` ~350, the wasm-only `PendingServer` join ~960, the `OnTransition` lists ~872, ~1072), `crates/client/src/frontend/mod.rs`, `crates/client/src/web_endpoint.rs` (~29, ~150), `crates/client/src/main.rs` (~300), `crates/client/src/frontend/{generator,load_world}.rs` (their Back), `justfile` (`web`, `web-release`), `scripts/web_release.sh`
- Test: `crates/client/src/frontend/mod.rs` `tests`, `crates/client/src/session.rs` tests (~1584), `crates/client/src/web_endpoint.rs` tests

**Interfaces:**
- Consumes: Task 1's properties and callbacks.
- Produces:
  - `AppState::SinglePlayer` and `AppState::Settings` (appended; `Worlds`' doc: "the server browser").
  - `pub fn initial_state(skip_menu: bool, dev_server: bool) -> AppState`: `InWorld` when `skip_menu` (native dev bypass, unchanged), `Starting` when `dev_server`, else `MainMenu`.
  - `web_endpoint`: `pub const DEV_SERVER_QUERY: &str = "dev_server";` `pub fn dev_server_from(search: &str, hostname: Option<&str>, debug_build: bool) -> Option<String>` — the trimmed, non-empty `?dev_server=` value only when `debug_build && login_override_allowed(hostname)`. The browser's startup calls it with `cfg!(debug_assertions)`; `Some(addr)` inserts `PendingServer(addr)` before the first frame. `BUILT_FOR_SERVER`, `default_server_address` and `LOCAL_SERVER_ADDRESS`'s use for the menu field are removed (`LOCAL_SERVER_ADDRESS` stays if `web_server_for` needs it).
  - `frontend`: `pub const SINGLE_PLAYER_AVAILABLE: bool = cfg!(not(target_arch = "wasm32"));`, `pub const CAN_EXIT: bool = cfg!(not(target_arch = "wasm32"));` (`JOIN_ONLY` removed); `pub const SETTINGS_SCREEN: i32 = 7;`; `pub fn menu_page(state: AppState) -> i32` (1 for `SinglePlayer`, else 0); `screen_index(SinglePlayer) == 0`, `screen_index(Settings) == SETTINGS_SCREEN`.
  - `FrontendAction`: adds `SinglePlayer`, `Multiplayer`, `Settings`, `LogIn`, `RefreshWorlds`, `PickRegion(String)`, `SelectWorld(String)`, `Search(String)`; removes `PlayOnline`, `JoinServer` (and the `JoinAddress` non-send). `JoinWorld(String)` stays.
  - `pub fn back_target(state: AppState, return_to: AppState) -> AppState`: `MapGenerator | LoadWorld → SinglePlayer`; `SinglePlayer | Worlds | Settings → MainMenu`; `Login → return_to`; `Starting | InWorld` keep today's rule (`LoginSession::home`, in `apply_frontend_actions`).
  - `pub fn allowed_on(action: &FrontendAction, single_player: bool, can_exit: bool) -> bool` (false for `SinglePlayer`, `NewWorld`, `LoadWorld` without single player, for `Exit` without exit); `apply_frontend_actions` drops a disallowed action first (`allowed_on(a, SINGLE_PLAYER_AVAILABLE, CAN_EXIT)`).
  - `action_target`: `SinglePlayer → Goto(SinglePlayer)`; `Multiplayer | Settings | LogIn | RefreshWorlds | JoinWorld | Logout | SignIn | OpenBrowser | CancelSignIn → Login` (the flow decides the state); `PickRegion | SelectWorld | Search → ActionEffect::Browser` (Task 4).
  - `just web` prints `http://127.0.0.1:1334/?dev_server=127.0.0.1` (plus `&name=` / `&login=` as today) and says it is the dev path; `web-release` and `scripts/web_release.sh` stop passing `VOXEL_WEB_SERVER` (the `web_server` justfile variable goes if nothing else reads it).
- [ ] **Step 1: Write the failing tests** (client lib — ask-first):
  - `back_goes_up_one_level`: every row of `back_target`, including `Login` with `return_to` `Worlds` and `Settings`.
  - `single_player_is_ignored_where_unavailable`: `allowed_on` for `SinglePlayer`, `NewWorld`, `LoadWorld`, `Exit` under both flags; everything else always allowed.
  - `dev_server_only_on_a_loopback_debug_page`: `("?dev_server=127.0.0.1", Some("127.0.0.1"), true)` → `Some("127.0.0.1")`; the same with `false` → `None`; host `game.errantry.renownandruin.com` → `None`; `localhost.evil.com` → `None`; `?dev_server=` empty → `None`.
  - `initial_state_is_the_menu_on_every_platform` (replaces the `login_first` assertion at `session.rs` ~1584): `(false, false) → MainMenu`, `(false, true) → Starting`, `(true, _) → InWorld`.
  - `screens_follow_the_state` extended with `SinglePlayer → (0, page 1)`, `Settings → 7`.
- [ ] **Step 2:** `just test-one client "--lib --no-run"`. Expected: FAIL to compile.
- [ ] **Step 3:** Implement; `push_frontend_state` sets `menu-page`, `single-player-available`, `can-exit` on change; `connect_frontend` wires the new callbacks. Until Task 5 adds its intents, `login_intent` maps `Multiplayer` to today's `LoginIntent::PlayOnline`, and `Settings` / `LogIn` / `RefreshWorlds` are `Goto(Settings)` / `Goto(Login)` / no-op — Task 5 replaces these lines, so every commit compiles and runs.
- [ ] **Step 4:** `just test-one client "--lib --no-run"`, `just smoke`, `just web-check`; `bash -n scripts/web_release.sh`. Tell Dawid the test names.
- [ ] **Step 5:** Commit (with Task 1): `feat(client): one main menu for web and native; the browser's local server is a dev-only page parameter`.

### Task 3: The login registry records each world's region

**Files:**
- Create: `crates/login/migrations/pg/0005_world_region.sql`, `crates/login/migrations/sqlite/0005_world_region.sql`
- Modify: `crates/login-api/src/lib.rs` (`WorldEntry`, `DEFAULT_REGION`), `crates/login/src/store.rs` (`WorldRow` ~75, `worlds` ~521, `world` ~526, the upsert), `crates/login/src/api.rs` (`worlds` ~369), `crates/login/src/ctl.rs` (usage ~24, `world add`, `world list` ~151), `justfile` (`world-register` in E1's form; the dev `world add dev …` lines need nothing — the default applies)
- Test: `crates/login/src/ctl.rs` tests, `crates/login/tests/` (the file that tests `/v1/worlds`), `crates/login-api` unit test

**Interfaces:**
- Produces:
  - Migration (both engines): `ALTER TABLE worlds ADD COLUMN region TEXT NOT NULL DEFAULT 'eu-central-1';`
  - `login_api`: `pub const DEFAULT_REGION: &str = "eu-central-1";` `WorldEntry { id, name, status, #[serde(default = "default_region")] region: String }`.
  - `WorldRow.region: String`; `world add <id> --name <n> … [--region <id>]` (default `DEFAULT_REGION`; a region id matches `^[a-z0-9][a-z0-9-]{0,31}$`, else the command fails naming the rule); `world list` prints the region column.
  - `world-register` prints `--region eu-central-1` (a `world_region` justfile variable, default `eu-central-1`, so a later cluster's world registers with its own).
- [ ] **Step 1: Write the failing tests:**
  - `a_world_entry_without_a_region_is_eu_central_1` (`login-api`: `{"id":"a","name":"A","status":"open"}` parses with `region == "eu-central-1"`; a round trip keeps `"us-east-2"`).
  - `ctl::world_add_takes_a_region` (default and explicit; `--region Bad_Id` refused).
  - `the_world_list_carries_each_region` (two worlds, `eu-central-1` and `ap-south-1`, both on `/v1/worlds` with their region), on SQLite and on PostgreSQL where the file's harness runs both.
  - `a_migrated_registry_puts_old_worlds_in_eu_central_1` (a row added before `0005` reads back `eu-central-1`).
- [ ] **Step 2:** `just test-one login ""` (and `just test-one login-api ""`). Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** The login suite on both engines; `just smoke`; `bash -n` is not needed (recipe only) — run `just --dry-run world-register` to see the printed line.
- [ ] **Step 5:** Commit: `feat(login): each world records its region (migration 0005); world add --region`.

### Task 4: The server browser (Slint + client state)

**Files:**
- Modify: `crates/slint-ui/ui/worlds.slint` (component renamed `ServerBrowser`), `crates/slint-ui/ui/controls.slint` (`PickList.header`), `crates/slint-ui/ui/frontend.slint`, `crates/client/src/frontend/mod.rs`, `crates/client/src/login/api.rs` (`OFFICIAL_LOGIN_URL`), `crates/client/src/login/flow.rs` (`setup_login` only)
- Test: `crates/slint-ui/tests/login_screen.rs`, `crates/client/src/frontend/mod.rs` tests, `crates/client/src/login/api.rs` tests

**Interfaces:**
- Consumes: `WorldEntry.region`, `DEFAULT_REGION` (Task 3).
- Produces:
  - `login::api`: `pub const OFFICIAL_LOGIN_URL: &str = "https://login.errantry.renownandruin.com";` `pub fn service_url(resolved: Option<String>) -> String` (`resolved` = `login_url()`, wins when `Some`). `setup_login` always inserts a `LoginSession` for `service_url(login_url())`.
  - `OnlineWorldRow { id: string, name: string }` (`open` removed — closed worlds never reach the screen).
  - `ServerBrowser` properties: `regions: [string]`, `region: int` (in-out, two-way), `search: string` (in-out), `worlds: [OnlineWorldRow]`, `selected: int` (in-out, -1 = none), `region-name: string` (shown in the details pane), `account: string` ("" = signed out), `error: string`, `busy: bool`, out `empty-line: string`. Callbacks: `region-chosen(int)`, `search-edited(string)`, `select(int)`, `join()`, `refresh()`, `back()`. Out coordinates for tests: `row-x`, `row0-y`, `row-pitch`, `join-x/y`, `refresh-x/y`, `back-x/y`, `region-x/y`.
  - Layout (Factorio's "Browse public games"): a full-screen `FrameFace` with the margins of `load-world.slint`. Top row: `TextField` (placeholder `Search`) on the left; on the right a `Region` caption and a `Dropdown` of `regions`. Body split as `load-world.slint` (`split` at 0.58 of the width): left a `PickList` with `header: "Name"` (the `browse-games-table` look where the `PickList` face allows, else `ListBoxFace` as today); right a details pane: the selected world's name (heading), its id (caption), `region-name`, and `You will be asked to sign in when you join.` when `account == ""`, else `Signed in as {account}`. Bottom bar: `Back` (`Styles.back-button`) left; `Refresh` and `Join` (`Styles.confirm-button`) right. `Join` enabled when a row is selected and `!busy`; a double-click on a row is Join. With no rows the list's first row draws `empty-line` (`Fetching the world list…` while busy, else `No worlds are open in this region right now`).
  - Client: `#[derive(Resource)] pub struct Browser { pub region: String, pub selected: Option<String>, pub search: String }` in `frontend` (default region `DEFAULT_REGION`), NOT session-reset; `pub fn regions(worlds: &[WorldEntry]) -> Vec<String>` (`DEFAULT_REGION` first, then the other distinct regions sorted); `pub fn shown_worlds(worlds: &[WorldEntry], region: &str, search: &str) -> Vec<(String, String)>` (open, of `region`, name contains `search` case-insensitively, in the service's order); `pub fn selected_index(shown: &[(String, String)], selected: Option<&str>) -> i32` (-1 when absent). `PickRegion(id)` sets `Browser.region` and clears `selected`; `SelectWorld(id)` / `Search(text)` set theirs; Join is `FrontendAction::JoinWorld(Browser.selected)`, ignored when `None`.
- [ ] **Step 1: Write the failing tests:**
  - Slint (`login_screen.rs`, replacing the old worlds-screen tests): `browser_lists_rows_and_join_needs_a_selection`; `browser_double_click_joins`; `browser_busy_disables_join_but_not_back_or_refresh`; `browser_shows_the_empty_line` (no rows, not busy → `empty-line` is the empty copy; busy → the fetching copy); `region_box_lists_eu_central_1` (`regions: ["eu-central-1"]` → picking it fires `region-chosen(0)`). (No address field: Task 7's grep and the review of `worlds.slint` — its only `TextField` is the search — check that.)
  - Client (ask-first): `shown_worlds_keeps_open_worlds_of_the_region` (`[a Open eu "Alpha", b Closed eu "Beta", c Open eu "alphabet", d Open ap "Alpine"]`, region `eu-central-1`, search `"ALPHA"` → `[a, c]`; search `""` → `[a, c]`; region `ap-south-1` → `[d]`); `regions_list_the_default_first_then_the_rest` (no worlds → `["eu-central-1"]`; worlds in `us-east-2`, `ap-south-1`, `eu-central-1` → `["eu-central-1", "ap-south-1", "us-east-2"]`); `selection_follows_the_world_id` (selected `c`, list reordered → index of `c`; `c` closed on refresh → -1; filtered out by search → -1; another region picked → `selected == None`); `service_url_prefers_the_resolved_login` (`None` → `OFFICIAL_LOGIN_URL`; `Some("http://127.0.0.1:8090")` wins).
- [ ] **Step 2:** `just test-one slint-ui "--test it -- login_screen::"`: FAIL; `just test-one client "--lib --no-run"`: FAIL to compile.
- [ ] **Step 3:** Implement. Keep the `Option<Res<LoginSession>>` system parameters (cheap; `debug_launch` relies on them).
- [ ] **Step 4:** PASS / compiles; `just slint-wasm`, `just smoke`, `just web-check`.
- [ ] **Step 5:** Commit: `feat(client): the Multiplayer server browser — region box, search, open worlds, details`.

### Task 5: The login flow — browse signed out, sign in from Join or Settings, return where you came from

**Files:**
- Modify: `crates/client/src/login/flow.rs`, `crates/client/src/frontend/mod.rs` (`login_intent`), `crates/client/src/debug_launch.rs` (~400–660)
- Test: `crates/client/src/login/flow.rs` tests, `crates/client/src/debug_launch.rs` tests

**Interfaces:**
- Consumes: the `FrontendAction`s of Tasks 2 and 4.
- Produces:
  - `LoginIntent`: `Multiplayer` (replaces `PlayOnline`), `Settings`, `LogIn`, `RefreshWorlds`; `JoinWorld`, `SignIn`, `OpenBrowser`, `CancelSignIn`, `Logout`, `Back` stay.
  - `LoginSession.return_to: AppState` (default `Worlds`); `pub fn return_to(&self) -> AppState`.
  - `intent` rules:
    - `Multiplayer` → `goto Worlds`, `LoginCall::Worlds`; when `!logged_in()`, a kept token and `!busy`: also `LoginCall::Refresh { silent: true }`.
    - `Settings` → `goto Settings`; the same silent refresh rule.
    - `LogIn` → `return_to = Settings`, `goto Login`.
    - `JoinWorld(id)` signed out → `return_to = Worlds`, `goto Login`. Signed in: as today.
    - `RefreshWorlds` → `LoginCall::Worlds` (not while one is in flight).
    - `Logout` → as today but never changes the screen (`logged_out_state` removed).
    - `Back` on `Login` → `back()` as today; the frontend's `back_target` sends the screen to `return_to`.
  - `apply`: a sign-in that completes (`LoginOidc`, or a non-silent `Refresh`) on the `Login` screen goes to `return_to` (was `Worlds`). A silent refresh never changes the screen; refused, it clears the token and account with NO error line. A failed `Worlds` call sets the error line and nothing else.
  - `refresh_worlds` (`OnEnter(Worlds)`): fetches the list whether or not signed in; `back_on_world_list` stays first.
  - `presents_token`: `Multiplayer | Settings | JoinWorld | Logout`.
  - `debug_launch` `online`: from `MainMenu` it writes `LoginIntent::Multiplayer` once; `online_login_verdict(state, busy, logged_in, refreshed, error)` — `Ok` on `Worlds` signed in; `Err("login failed: the kept login was refused")` on `Worlds`, not busy, not signed in, after its refresh answered.
- [ ] **Step 1: Write the failing tests** (client lib — ask-first; the flow is a pure state step):
  - `multiplayer_lists_worlds_signed_out` (no token → `goto Worlds`, calls `[Worlds]`, no `Refresh`).
  - `multiplayer_with_a_kept_token_refreshes_silently` (calls `Worlds` and `Refresh { silent: true }`; the reply signs in, the screen stays `Worlds`).
  - `a_refused_kept_token_on_settings_signs_out_quietly` (refused → `account == None`, `refresh == None`, `StoreOp::Clear`, `error == ""`, state `Settings`; a second `Settings` intent sends no `Refresh`).
  - `join_signed_out_asks_for_a_sign_in_and_returns_to_the_browser`.
  - `login_returns_to_where_it_was_entered_from` (`JoinWorld` signed out → `return_to == Worlds`; Back; `LogIn` → `return_to == Settings`; the sign-in completes → `goto Settings`).
  - `logout_stays_on_the_screen` (on `Settings`: `goto None`, `StoreOp::Clear`, a `Logout` call).
  - `a_failed_world_list_keeps_the_browser_usable` (a `Worlds` failure → error line, `busy() == false`, `RefreshWorlds` sends `Worlds` again).
  - `debug_launch::online_verdicts` for the new signature.
  - The existing tests that use `LoginIntent::PlayOnline` move to `Multiplayer`; no assertion weakened (list each in the commit body).
- [ ] **Step 2:** `just test-one client "--lib --no-run"`. Expected: FAIL to compile.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `just test-one client "--lib --no-run"`, `just smoke`, `just web-check`. Tell Dawid the names; a live `client debug online` is his.
- [ ] **Step 5:** Commit: `feat(client): browse worlds signed out; sign in from Join or Settings and return there`.

### Task 6: Settings — the account

**Files:**
- Create: `crates/slint-ui/ui/settings.slint`
- Modify: `crates/slint-ui/ui/frontend.slint`, `crates/client/src/frontend/mod.rs` (`push_login_state`, `connect_frontend`)
- Test: `crates/slint-ui/tests/login_screen.rs`

**Interfaces:**
- Produces: `SettingsScreen { in property <string> account; in property <bool> busy; in property <string> error; out property <string> account-line; callback log-in(); callback log-out(); callback back(); out property <length> log-in-x, log-in-y, log-out-x, log-out-y, back-x, back-y; }` — a centred `FrameFace` (`Settings`, at most 520 px wide), an `Account` heading, `account-line` = `Signed in as {account}` / `Not signed in` / `Checking your sign-in…` (busy and signed out), ONE button slot holding `Log out` when signed in, else `Log in` (both disabled while `busy`), the error line, Back. `FrontendUi` forwards with a `settings-` prefix and the shared `login-account` / `login-busy` / `login-error`; `log-in` → `FrontendAction::LogIn`, `log-out` → `FrontendAction::Logout`. Screen 7. The account shown is the display name, else the email (today's rule in `push_login_state`).
- [ ] **Step 1: Write the failing Slint tests:** `settings_signed_out_offers_log_in`; `settings_signed_in_offers_log_out` (`account: "Dawid"` → `account-line == "Signed in as Dawid"`, `log-out` fires); `settings_busy_disables_both_but_not_back`.
- [ ] **Step 2:** `just test-one slint-ui "--test it -- login_screen::"`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** PASS; `just slint-wasm`, `just smoke`, `just web-check`.
- [ ] **Step 5:** Commit: `feat(client): Settings shows the account, with Log in / Log out`.

### Task 7: Docs and the hand check

**Files:**
- Modify: `CLAUDE.md` (Client → "Native QUIC trust": the login is `VOXEL_LOGIN_URL`, else `{origin}/login` on an https non-loopback page, else `OFFICIAL_LOGIN_URL` — "otherwise no Play online" goes; "Debug launches": `?dev_server=` is the browser's dev bypass; the Auth paragraph: one login service for every region, `world add --region`), `TODO.md` (open To-confirm items; the future-region notes below), this plan ("As built").
- [ ] **Step 1:** `grep -rn "Play online\|play_online\|JOIN_ONLY\|join-only\|PlayOnline\|logged_out_state\|VOXEL_WEB_SERVER\|default_server_address" crates scripts justfile CLAUDE.md` → only dated documents remain.
- [ ] **Step 2:** `just smoke`, `just slint-wasm`, `just web-check`, `just test-one slint-ui "--test it"`, `just test-one login ""`, `just test-one client "--lib --no-run"`.
- [ ] **Step 3:** Stage `CLAUDE.md` and `justfile` with filtered patches; commit: `docs: the new main menu flow; one login service, worlds by region`.
- [ ] **Step 4:** Report to Dawid what is his: the client test names of Tasks 2, 4, 5; `just play` (Single player → New world / Load world and Back up each level; Multiplayer lists the k8s world under `eu-central-1` from production login; Join asks for a sign-in and returns to the browser; Settings shows the name; Log out); `just web` via the printed `?dev_server=` URL (straight into the local world, no menu) and the plain URL (Single player grey, no Exit, no address field); the login migration `0005` on orion's login database at the next `orion-login-push` (it runs at start); the k8s page once released.

---

## Not in this plan (future regions)

- A second region on another cluster. What it will need, recorded in `TODO.md`: the login service's link to that world's coordinator crosses clusters (`worlds.host_link` reachable from the login cluster; the pinned `svc-link` already authenticates it); the page's `/login` proxy belongs with the one login service, not inside a region's world module (today the world module's `web` Deployment serves it); a join's ticket names a host in that region (unchanged: the `SpawnAck` carries it).
- Players / Version / Tags columns (an online count from the login service or coordinator).
- Settings beyond the account (graphics, keys, audio). Native or web join-by-address for players; favourites; LAN browsing; hosting from the menu.

## Self-review notes

- **Spec → tasks:** same client with web differences (T2 constants; T1 grey-out / no Exit); four options (T1, T2); Single player → New world / Load world reusing the generator and the load screen (T1 page 1, T2 back targets); Multiplayer → Factorio-style browser (T4) with a region box whose only option is `eu-central-1` = the k8s deployment (T3 records the region per world, T4 lists and filters); joinable worlds of that region (T4 `shown_worlds`, Decision 2); Settings with the username and Log in / Log out (T6, T5); no connect-to-address, the web plays only official servers, `just web`'s local server dev-only (T2, Decisions 7–8); one login service serving every region (Decisions 3–4, T3); after E1 (header "Runs after").
- **Names across tasks:** `menu-page`, `single-player-available`, `can-exit` (T1) → T2; `SINGLE_PLAYER_AVAILABLE`, `CAN_EXIT`, `SETTINGS_SCREEN`, `back_target`, `allowed_on`, `dev_server_from`, `DEV_SERVER_QUERY`, `FrontendAction::{SinglePlayer, Multiplayer, Settings, LogIn, RefreshWorlds, PickRegion(String), SelectWorld, Search}` (T2) → T4–T6; `DEFAULT_REGION`, `WorldEntry.region` (T3) → T4; `OFFICIAL_LOGIN_URL`, `service_url`, `Browser`, `regions`, `shown_worlds`, `selected_index`, `OnlineWorldRow { id, name }` (T4) → T5–T6; `LoginIntent::{Multiplayer, Settings, LogIn, RefreshWorlds}`, `return_to` (T5) → T2's `back_target`, T6.
- **Proportion:** the request is two paragraphs; the plan is longer because the login flow's existing rules (token rotation, generations, rejoin) must survive the re-wiring and the registry gains a column. Bodies are left to the implementer; the steps carry names, copy, layout and tests.
