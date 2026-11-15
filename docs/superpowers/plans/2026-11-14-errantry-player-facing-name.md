# Errantry: Player-Facing Name and the `game.errantry.renownandruin.com` Hostname — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Players see the game as **Errantry: Renown and Ruin** and reach it at `game.errantry.renownandruin.com`; `orion` stays the project's code name in everything players do not see.

**Architecture:** A text-and-configuration change in three places — the client (window, page, main menu), the website with its Keycloak login theme, and every reference to the game's hostname — followed by a rollout to the `orion` cluster. The DNS record and the move of every other host are the companion plan `2026-11-14-hosts-to-errantry-renownandruin.md`. No identifier, path, recipe, bucket, IAM name, storage key or theme folder is renamed.

**Tech Stack:** Rust (Bevy client), Slint, static HTML/JS, Keycloak theme (FreeMarker, `.properties`), OpenTofu, `just`.

**Spec:** none. The decisions come from the naming conversation of 2026-11-14 and are recorded in Global Constraints.

## Global Constraints

- Full name, exact: `Errantry: Renown and Ruin`. Short name, exact: `Errantry`.
- **Full name** only on the website's home page: its `<title>` and its hero `<h1>`.
- **Short name** everywhere else a player reads the name: window title, the game page's tab title and loading label, the main menu title (its frame is 260 px wide and sizes from the buttons, not the title — the full name would clip), the website's brand link, the account wording, the Keycloak realm name and mail sender.
- **`orion` stays** (code name, not player-facing): `deploy/envs/orion/`, every `just orion-*` recipe, kube context `orion`, the state bucket and state keys, IAM names, AWS tags, `cluster_name`, the theme folder `website/theme/orion` and realm setting `loginTheme: orion`, CSS class `orion`, `data-orion-theme`, `ORION_SITE_URL`, the message KEY `orionFooter`, browser storage keys `orion.session` / `orion.pending.*` / `orion.notice`, `console.error("orion: …")` prefixes, `__orionWasm`, `orion_worker_main`, worker names `orion-sim` / `orion-compute-N`, and code comments.
- Game hostname: `game.errantry.renownandruin.com` replaces `orion.factoriosurvivalworld.com` everywhere, with no alias kept. Nothing serves the old name today (checked 2026-11-14: the box answers it with Traefik's self-signed certificate; the compose deploy is stopped), so there is nothing to migrate or redirect.
- Dated documents are history and are not edited: `docs/superpowers/{plans,specs}/*`, `docs/claude-md-full-reference-2026-11-08.md`, `docs/memories/*`.
- No git worktree; work on `develop`. Commit only the hunks of the task (the tree has unrelated uncommitted changes — never `git add -A`).
- Applies to the live cluster and to DNS are run by Dawid, or with his go-ahead after he has read the plan output (`just orion-plan <root>` always before `just orion-apply <root>`).

## Review Focus

1. **A redirect URI that no longer matches.** The Keycloak `game` client and the login service's CORS list both move to the new origin. If only one root is applied, a browser login fails. Task 4 applies `keycloak-realm` and `login` together and checks both.
2. **A stale theme in production.** Keycloak caches themes outside `start-dev`. The Deployment carries `checksum/themes`, so the plan for the `keycloak` root must show the pod template changing; Task 4 Step 2 checks for it.
3. **A player-facing "Orion" that the greps miss.** Task 2 Step 6 opens the three website pages and every login page in the dev setup and reads them.
4. **A code-name string renamed by an over-eager search and replace.** Every task ends with a grep that the kept strings are still there (Task 2 Step 5, Task 3 Step 5).
5. **The mail sender name.** Account mail is player-facing; Task 2 Step 6 reads a verification mail in mailpit, Task 4 Step 5 reads the planned `from_display_name`.

---

### Task 1: The name inside the game

**Files:**
- Modify: `crates/client/src/main.rs:233`
- Modify: `crates/client/index.html:9`, `crates/client/index.html:83`
- Modify: `crates/slint-ui/ui/frontend.slint:26`
- Modify: `crates/slint-ui/ui/main-menu.slint:11`
- Modify: `CLAUDE.md` (the `## Project` section)

**Interfaces:**
- Produces: nothing other tasks use.

- [ ] **Step 1: Change the four strings to the short name.**
  - `main.rs:233`: `title: "Errantry".into(),`
  - `index.html:9`: `<title>Errantry</title>`
  - `index.html:83`: `<div id="loading-label">Loading Errantry&hellip;</div>`
  - `frontend.slint:26`: `in property <string> game-title: "Errantry";`
  - `main-menu.slint:11`: `in property <string> title: "Errantry";`

  Leave `__orionWasm`, `orion_worker_main` and the worker names in the same files alone.

- [ ] **Step 2: Record the rule in `CLAUDE.md`.** Add as the second paragraph of `## Project`:

  ```markdown
  **Name:** the game is **Errantry: Renown and Ruin** ("Errantry" for short) — that is what players read
  (window and page titles, the menu, the website, login pages and mail). **Orion** is the project's code name:
  keep it wherever players do not look (code identifiers, `deploy/envs/orion`, `just orion-*`, the kube
  context, bucket and IAM names, the `orion` login theme folder, storage keys). The game is served at
  `game.errantry.renownandruin.com`.
  ```

- [ ] **Step 3: Check nothing player-facing is left in the two crates.**

  Run: `git grep -n '"Orion"\|>Orion<\|Loading Orion' -- crates`
  Expected: no output.

- [ ] **Step 4: Type-check.**

  Run: `just smoke`
  Expected: passes inside its 60 s limit.

- [ ] **Step 5: Run the Slint front-end tests** (they render the menu).

  Run: `just test-one slint-ui "--test it frontend::"`
  Expected: all pass.

- [ ] **Step 6: Commit.**

  ```bash
  git add crates/client/src/main.rs crates/client/index.html crates/slint-ui/ui/frontend.slint crates/slint-ui/ui/main-menu.slint
  git add -p CLAUDE.md   # this task's paragraph only
  git commit -m "feat(client): the game is called Errantry; Orion stays the code name"
  ```

---

### Task 2: The name on the website, the login pages and account mail

**Files:**
- Modify: `website/site/index.html:8,17,30,59`
- Modify: `website/site/callback.html:9,18,36,42`
- Modify: `website/site/profile.html:8,17,28,57`
- Modify: `website/theme/orion/login/messages/messages_en.properties:1,2,4,5`
- Modify: `website/theme/orion/login/template.ftl:110`
- Modify: `website/theme/orion/login/resources/img/logo.svg:1` (`website/site/img/logo.svg` and `website/site/css/site.css` are symlinks to the theme's copies: edit the theme's file only)
- Modify: `deploy/dev/keycloak/players-realm.json:3,15,37`
- Modify: `deploy/modules/keycloak-realm/variables.tf:24-28` (`display_name` default)

**Interfaces:**
- Produces: `display_name` default `"Errantry"`, which Task 4 rolls out.

- [ ] **Step 1: Website pages.** Exact text:

  | File | Line | New text |
  |---|---|---|
  | `index.html` | 8 | `<title>Errantry: Renown and Ruin</title>` |
  | `index.html` | 30 | `<h1>Errantry: Renown and Ruin</h1>` |
  | `callback.html` | 9 | `<title>Logging in · Errantry</title>` |
  | `callback.html` | 36 | `Back to the Errantry home page` (link text only) |
  | `profile.html` | 8 | `<title>Your account · Errantry</title>` |
  | `profile.html` | 28 | `Log in to see your Errantry account.` |
  | all three | brand `<span>` | `<span>Errantry</span>` |
  | all three | footer | `<p>One Errantry account for the game and this site.</p>` |

  `<html class="orion">` stays. The hero lead and the three feature cards are not touched (they still describe only the fortress side; rewriting them for the two-mode direction is separate work).

- [ ] **Step 2: Login theme.**
  - `messages_en.properties`: `loginTitle=Errantry account`, `loginTitleHtml=Errantry account`, `backToApplication=Back to Errantry`, `orionFooter=One Errantry account for the game and this site.` (the key keeps its name).
  - `template.ftl:110`: `<span>${realm.displayName!'Errantry'}</span>`
  - `logo.svg`: `aria-label="Errantry"`.

- [ ] **Step 3: Dev realm.** In `players-realm.json`: `"displayName": "Errantry"`, `"fromDisplayName": "Errantry"`, and the `site` client's `"name": "Errantry"`. `"loginTheme": "orion"` stays.

- [ ] **Step 4: Production realm default.** In `deploy/modules/keycloak-realm/variables.tf`, `variable "display_name"`: `default = "Errantry"` (it was `"Game account"`; it is the realm's display name and the mail sender name).

- [ ] **Step 5: Greps.**

  Run: `git grep -n 'Orion' -- website deploy/dev/keycloak`
  Expected: exactly one line, the code comment at the top of `website/theme/orion/login/resources/css/site.css` (`/* Orion: one stylesheet …`).

  Run: `git grep -c 'orion.session\|orion.pending\|data-orion-theme\|class="orion"\|ORION_SITE_URL\|orionFooter' -- website scripts deploy/dev`
  Expected: the same counts as before the task (run it once before Step 1 and keep the output).

- [ ] **Step 6: Look at it in the dev setup.**

  Run: `just site-test` — expected: all pass.
  Run: `just kc-down && just kc-up`, then `just site-theme-check` — expected: every page in the theme.
  Run: `just site-dev`, open `http://127.0.0.1:8182/`, and read: the home page (tab title and heading show the full name), the brand link and footer on all three pages, the login and register pages, and the verification mail of a new sign-up at `http://127.0.0.1:8025` (sender name `Errantry`). No "Orion" anywhere a player reads.

- [ ] **Step 7: Commit.**

  ```bash
  git add website/site/index.html website/site/callback.html website/site/profile.html \
          website/theme/orion/login/messages/messages_en.properties website/theme/orion/login/template.ftl \
          website/theme/orion/login/resources/img/logo.svg deploy/dev/keycloak/players-realm.json \
          deploy/modules/keycloak-realm/variables.tf
  git commit -m "feat(site): the website, login pages and account mail say Errantry"
  ```

  `deploy/modules/keycloak-realm/` is untracked today; if it is still untracked, leave it out of this commit and tell Dawid the edit is in his working tree.

---

### Task 3: The game's hostname in the repository

**Files:**
- Modify: `justfile:790` (`web_server`), `justfile:854` (comment)
- Modify: `scripts/deploy_push.sh:50`
- Modify: `deploy/web/compose.yml:6,10,39,40`
- Modify: `website/site/js/config.js:11`, `website/site/index.html:33`
- Modify: `deploy/modules/login/variables.tf:77` (`cors_origins` default)
- Modify: `deploy/modules/keycloak-realm/variables.tf:72` (`game_web_origins` default)
- Modify: `crates/client/src/web_endpoint.rs:5,261,262`
- Modify: `crates/slint-ui/tests/frontend.rs:70,77`

**Interfaces:**
- Produces: defaults `cors_origins = ["https://game.errantry.renownandruin.com"]` and `game_web_origins = ["https://game.errantry.renownandruin.com"]`, which Task 4 rolls out.

- [ ] **Step 1: Replace the hostname.** In every file above, `orion.factoriosurvivalworld.com` becomes `game.errantry.renownandruin.com` — including both halves of the two certificate paths in `compose.yml` (directory and file name) and the two test files. No other string changes.

- [ ] **Step 2: Check none is left outside the dated documents.**

  Run: `git grep -n 'orion\.factoriosurvivalworld' -- . ':!docs'` and the same with `grep -rn` over the untracked `deploy/modules` and `deploy/envs`.
  Expected: no output.

- [ ] **Step 3: Tests.**

  Run: `just test-one slint-ui "--test it frontend::"` — expected: pass.
  Run: `just site-test` — expected: pass.
  The client test `the_field_prefills_the_built_for_server_then_the_page_host` only changes a fixture string; running client tests is a slow opt-in, so ask Dawid before `just test-one client "--lib web_endpoint"`. Without it, `just smoke` (type-check) is the check.

- [ ] **Step 4: Validate the two modules.**

  Run: `just orion-plan login` and `just orion-plan keycloak-realm` (the latter needs `just orion-keycloak-admin` running).
  Expected: `login` changes only the CORS origin in the pod's environment; `keycloak-realm` changes only the `game` client's `valid_redirect_uris` / `web_origins` (plus the display name from Task 2). Do not apply here.

- [ ] **Step 5: The code name is untouched.**

  Run: `git diff --stat` and read it: only the files listed above.
  Run: `just --list | grep -c orion-`
  Expected: the same count as before the task.

- [ ] **Step 6: Commit.**

  ```bash
  git add justfile scripts/deploy_push.sh deploy/web/compose.yml website/site/js/config.js website/site/index.html \
          deploy/modules/login/variables.tf crates/client/src/web_endpoint.rs crates/slint-ui/tests/frontend.rs
  git commit -m "feat(deploy): the game is served at game.errantry.renownandruin.com"
  ```

  `justfile` and `website/site/index.html` carry other tasks' or unrelated hunks: stage with `git add -p`.

---

### Task 4: Roll out to the `orion` cluster

Run by Dawid, or with his go-ahead per apply. Needs `just tunnel` and, for `keycloak-realm`, `just orion-keycloak-admin`.

**Files:** none.

**Interfaces:**
- Consumes: Task 2's theme files and `display_name`; Task 3's two origin defaults.

- [ ] **Step 1: Check no root overrides the changed defaults.** In `deploy/envs/orion/{keycloak-realm,login}/terraform.tfvars` look for `display_name`, `game_web_origins`, `cors_origins` (read the variable names only; do not print the files). An override there wins over the module default and must be changed by hand.

- [ ] **Step 2: `keycloak` (the theme).** `just orion-plan keycloak` — expected: the theme ConfigMap changes and the Deployment's `checksum/themes` annotation changes (the pod restarts). If the annotation does not change, stop: the theme would be served stale. Then `just orion-apply keycloak`.

- [ ] **Step 3: `keycloak-realm` and `login`, one after the other.** `just orion-plan keycloak-realm` then `just orion-apply keycloak-realm`; `just orion-plan login` then `just orion-apply login`. Expected plans: as in Task 3 Step 4. The `login` apply must not show an image change (releases set the image; the Deployment ignores it).

- [ ] **Step 4: `site`.** `just orion-plan site` — expected: the files ConfigMap and the `checksum/site` annotation change. `just orion-apply site`.

- [ ] **Step 5: Check the live pages.**
  - `https://factoriosurvivalworld.com/`: tab title and heading `Errantry: Renown and Ruin`; Play in browser points at `https://game.errantry.renownandruin.com/`.
  - `https://auth.factoriosurvivalworld.com/realms/players/account` (redirects to the login page): heading `Errantry account`, brand `Errantry`.
  - `just orion-login-logs`: the two pods started and serve.
  - A password-reset mail to your own account arrives from sender name `Errantry`.

---

### Task 5: DNS for `game.errantry.renownandruin.com`

Moved. Dawid decided on 2026-11-14 that every host goes under `game.errantry.renownandruin.com`, so the game's DNS record is made by the `dns` root in **Task 1 of `2026-11-14-hosts-to-errantry-renownandruin.md`**, together with the website's. Run that task; there is nothing to do here.

**Not in this plan:** serving the game at `game.errantry.renownandruin.com`. The record only points the name at the box; an ingress for the browser bundle and a certificate for the WebTransport endpoint belong to porting `just deploy` to k3s.
