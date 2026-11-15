# Website: landing page, themed Keycloak login, profile page — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `https://factoriosurvivalworld.com` is the game's landing page with Log in / Create account buttons and a profile page; every login, sign-up and password screen (for the site, Zulip and the game) is Keycloak wearing the site's own design, and players never reach a Keycloak-branded page or Keycloak's account console.

**Architecture:** A static site (HTML, one stylesheet, three small ES modules, no backend, no build step) served by a stock nginx image from a ConfigMap. Its login button is a browser redirect to Keycloak (public client `site`, authorization code + PKCE); the profile page shows the claims of the ID token that came back. Keycloak's pages get the same look from a native folder theme `orion` (one layout template + the SAME stylesheet file), mounted into the stock Keycloak image from a ConfigMap. Keycloak's account console is switched off.

**Tech Stack:** HTML/CSS/ES modules (no framework, no npm dependencies); `node --test` (Node ≥ 22) for the JS; Keycloak 26.8.2 folder theme (FreeMarker, parent `base`); OpenTofu ≥ 1.10 with `hashicorp/kubernetes ~> 3.2`, `keycloak/keycloak ~> 5.10`; image `nginxinc/nginx-unprivileged:1.28-alpine`; dev Keycloak through `deploy/dev/compose.yml`.

**Spec:** none written. The research in the session of 2026-11-13 and the decisions below (user, 2026-11-13) are the spec. Builds on `2026-11-12-keycloak-1-identity-provider-zulip.md` (done); independent of `2026-11-12-keycloak-2-game-login.md` except for the shared dev Keycloak (Task 1).

## Decisions settled in planning

1. **Keycloak stays the identity provider and renders the login pages** (user). A login page hosted by the site itself is not possible with Keycloak; the theme makes its pages look like the site. The address bar shows `auth.factoriosurvivalworld.com` during login.
2. **The site is at the apex, `https://factoriosurvivalworld.com`; `www.` redirects to it** (user). One origin, so one redirect URI. The DNS records for both already exist in `~/Projects/mod/mods/survival-world/infra` and point at the orion box; this plan creates no DNS.
3. **Static site, no backend** (user). The client `site` is public (no secret). Nothing on the site is protected by the login: the profile page only displays what Keycloak told this browser.
4. **Native folder theme, shared stylesheet** (user), not Keycloakify. `website/theme/orion/login/resources/css/site.css` is the one stylesheet; the site reaches it through a symlink.
5. **Profile page v1: username, email, verified mark, "Change password", "Log out", links to Chat and Play.** Change password is Keycloak's own page, opened with `kc_action=UPDATE_PASSWORD`.
6. **Not in v1, on purpose:** changing the email (Zulip matches accounts by email until Zulip 13: a changed address would make a second Zulip account), deleting the account (Zulip and game rows would be orphaned), a sessions list, a themed mail template, languages other than English, single logout with Zulip (its OIDC backend has none).
7. **Keycloak's account console is disabled server-wide** (`KC_FEATURES_DISABLED=account,account-api`; user, 2026-11-13: "disable anything you want so long as I have access to admin / master realm for management"). The admin console and admin API are separate features and stay: managing users, realms and clients through `just orion-keycloak-admin` is unchanged. What goes away for every realm, `staff` and `master` included, is the self-service page `/realms/<realm>/account/`: a staff member changes their password through "Forgot password", and an administrator resets an authenticator in the admin console (Users → the user → Credentials). Blocking only the players' path at the Ingress was considered and dropped: it depends on Traefik's prefix priority, which could not be checked before an apply.
8. **Zulip is not changed.** The site's Chat link is `https://chat.factoriosurvivalworld.com/accounts/login/social/oidc/keycloak`, which skips Zulip's own login page (verified live: 302 into the OIDC flow).
9. **Visual design and copy are placeholders the user reviews in the dev setup** (Task 3 Step 6) before anything is applied to orion.

## Facts verified against `quay.io/keycloak/keycloak:26.8.2` (2026-11-13, local container)

- A theme mounted with ConfigMap-style symlinks (`login -> ..data/login`) renders and serves its CSS.
- `kc*Class` properties set in the theme's `theme.properties` reach the `base` templates (72 such names exist; list: `unzip -p <themes jar> theme/keycloak/login/theme.properties`).
- `siteUrl=${env.ORION_SITE_URL}` in `theme.properties` is substituted from the container's environment.
- `prompt=create` on the authorization request opens the registration form.
- With features `account,account-api` disabled, `/realms/<realm>/account/` answers 404, and login, token exchange, `kc_action=UPDATE_PASSWORD` and logout still work. So do the admin console page (`/admin/master/console/` → 200), its login page, and the admin API (list users, create a realm, a client and a user).
- The callback after an action carries `kc_action=UPDATE_PASSWORD&kc_action_status=success|cancelled`; every callback carries `iss=<issuer>`.
- A public client's ID token has `iss`, `aud` (= client id), `nonce`, `preferred_username`, `email`, `email_verified`; it lives 300 s. The token endpoint answers CORS for the client's web origin.

## Global Constraints

- Work on `develop` in the main checkout. Never create a git worktree. The tree holds the user's uncommitted work: `git add` only a task's own files.
- No npm dependency, no bundler, no CDN: every byte the site or theme serves is in `website/`.
- Text files only under `website/site` and `website/theme` (SVG for images), and each directory stays under 900 000 bytes: both travel as ConfigMaps.
- `website/site/css/site.css` and `website/site/img/logo.svg` are symlinks into the theme; never copy them.
- OpenTofu, not Terraform, through the `just orion-*` recipes. **Plan before every apply, and the user runs every `just orion-apply …`**: an agent stops at each apply step, shows the plan summary and waits.
- Never print or commit secrets (`deploy/**/*.tfvars` is git-ignored). This plan adds none.
- Follow the existing modules' shape (`deploy/modules/picoshare`): `versions.tf`, `variables.tf` with a description on every non-obvious variable, `main.tf`, `outputs.tf`; a root has `backend.tf`, `versions.tf`, `providers.tf`, `main.tf` and declares `kube_context`.
- `tofu fmt -recursive` on touched directories before each commit. Keep the `justfile` and `CLAUDE.md` in sync in the task that adds a recipe or a root.
- Exact values: client id `site`; scope `openid profile email`; production issuer `https://auth.factoriosurvivalworld.com/realms/players`; dev issuer `http://127.0.0.1:8180/realms/players`; dev site `http://127.0.0.1:8182`; theme name `orion`.

## Review Focus

1. **A crafted link sends a player somewhere else after login** (`returnTo`). Expected: only `/` and `/profile.html` are accepted, anything else becomes `/`. Test in Task 2.
2. **The callback page is reloaded or opened from history** (code already used, nothing pending). Expected: a plain message and a link home, no exception, no half-made session. Test in Task 2.
3. **The player presses Cancel on the change-password page.** Expected: back on the profile, still logged in, "Password not changed". Test in Task 2.
4. **A Keycloak page nobody looked at is broken by the theme** (the device-code page the game opens, the error page, the expired page). Expected: every page in Task 1's list renders inside the site layout. Pinned by `scripts/site_theme_check.sh`, which also runs after any Keycloak image bump.
5. **A visitor arrives on `www.` or plain HTTP.** Expected: one redirect to `https://factoriosurvivalworld.com/…`, so the redirect URI always matches. Pinned by Task 6 Step 6.

## File structure

```
website/theme/orion/login/theme.properties       parent, styles, kc*Class mapping, siteUrl
website/theme/orion/login/template.ftl           the one layout every Keycloak page uses
website/theme/orion/login/messages/messages_en.properties   wording overrides
website/theme/orion/login/resources/css/site.css THE stylesheet (site + theme)
website/theme/orion/login/resources/img/logo.svg
website/site/{index,profile,callback}.html
website/site/css/site.css, img/logo.svg          symlinks into the theme
website/site/js/config.js                        issuer, client id, links (dev vs production by hostname)
website/site/js/oidc.js                          the login protocol, no DOM
website/site/js/app.js                           DOM wiring for the three pages
website/tests/oidc.test.mjs                      node --test
scripts/site_theme_check.sh                      curls every themed page of the dev Keycloak
deploy/dev/compose.yml, deploy/dev/keycloak/players-realm.json   dev Keycloak (shared with plan keycloak-2)
deploy/modules/keycloak/{main,variables}.tf      themes ConfigMap, features off, ORION_SITE_URL
deploy/envs/orion/keycloak/main.tf               passes the theme
deploy/modules/keycloak-realm/{main,clients,variables,outputs}.tf   login_theme, client `site`
deploy/modules/site/, deploy/envs/orion/site/    nginx + ConfigMaps + Ingress
justfile, CLAUDE.md
```

---

### Task 1: Dev Keycloak and the `orion` theme

**Files:**
- Create: `website/theme/orion/login/{theme.properties,template.ftl}`, `…/messages/messages_en.properties`, `…/resources/css/site.css`, `…/resources/img/logo.svg`, `scripts/site_theme_check.sh`
- Create or extend: `deploy/dev/keycloak/players-realm.json`, `deploy/dev/compose.yml` (service `keycloak`), `deploy/dev/README.md`
- Modify: `justfile` (`kc-up`, `kc-down`, `site-theme-check`)

**Interfaces:**
- Produces: compose service `keycloak` exactly as `2026-11-12-keycloak-2-game-login.md` Task 5 describes it (`start-dev --import-realm`, `127.0.0.1:8180:8080`, admin / admin, realm file under `/opt/keycloak/data/import/`, a health check), plus: `website/theme/orion` mounted read-only at `/opt/keycloak/themes/orion`, env `ORION_SITE_URL=http://127.0.0.1:8182`. If that service already exists, add to it; never make a second one.
- Produces: dev realm `players` with `loginTheme: "orion"`, `registrationAllowed: true`, user `dev` / `dev-password` (email `dev@localhost`, verified), and client `site`: public, standard flow, PKCE `S256`, redirect URI `http://127.0.0.1:8182/callback.html`, post-logout redirect URI `http://127.0.0.1:8182/`, web origin `http://127.0.0.1:8182`. Keep any `game` client already in the file.
- Produces: CSS class names in `site.css` that Task 3 uses: `.page`, `.site-header`, `.brand`, `.card`, `.field`, `.btn`, `.btn-primary`, `.btn-quiet`, `.alert`, `.alert-error`, `.alert-success`, `.site-footer`.
- Produces: `just kc-up` / `kc-down` (`docker compose -f deploy/dev/compose.yml up -d --wait keycloak` / `stop keycloak`), `just site-theme-check`.

- [ ] **Step 1: Write `scripts/site_theme_check.sh`** (fails first: no theme). Env: `KC` (default `http://127.0.0.1:8180`), `SITE` (default `http://127.0.0.1:8182`, used for the redirect URI and the brand link), `SKIP_LOGIN=1` (skips the update-password page: for a server without the `dev` user). Against realm `players`, client `site`, it fetches each page below with a cookie jar and fails naming the page unless the body contains `data-orion-theme` and a `<link … css/site.css` whose URL answers 200:
  - login: `/protocol/openid-connect/auth?client_id=site&response_type=code&scope=openid&redirect_uri=http://127.0.0.1:8182/callback.html&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256`
  - register: the same with `&prompt=create` (must also contain `id="kc-register-form"`)
  - reset password: `/login-actions/reset-credentials?client_id=site`
  - error: the login URL with `redirect_uri=http://127.0.0.1:8182/evil.html` (HTTP 400, still themed)
  - device code: `/device`
  - update password: log in as `dev` by posting the login form, then the login URL with `&kc_action=UPDATE_PASSWORD`, following redirects (must contain `name="password-new"`)
  - every page: must contain `href="http://127.0.0.1:8182/"` (the brand link from `ORION_SITE_URL`) and must NOT contain `Keycloak` (case-insensitive) in visible text.
- [ ] **Step 2: Add the dev Keycloak** (compose service, realm JSON, the two recipes) and the theme directory with only `theme.properties` = `parent=base` in it (the mount source must exist, or Docker creates it owned by root). Run `just kc-up && just site-theme-check` — expected: FAIL at `login` (no `data-orion-theme`).
- [ ] **Step 3: Write the theme.**
  - `theme.properties`: `parent=base`, `locales=en`, `styles=css/site.css`, `siteUrl=${env.ORION_SITE_URL}`, and a value for each `kc*Class` the `base` templates of the pages in Step 1 use, mapped onto the class names in Interfaces (get the 72 names with `docker run --rm --entrypoint sh quay.io/keycloak/keycloak:26.8.2 -c 'unzip -p /opt/keycloak/lib/lib/main/org.keycloak.keycloak-themes-26.8.2.jar theme/keycloak/login/theme.properties'`).
  - `template.ftl`: start from the image's `theme/base/login/template.ftl` (same `unzip -p`). Keep the macro signature `registrationLayout bodyClass="" displayInfo=false displayMessage=true displayRequiredFields=false`, every `<script>` block, the message block, and all five nested sections (`header`, `form`, `info`, `socialProviders`, `show-username`) with their conditions. Replace only the markup around them: `<body data-orion-theme>`, a `.site-header` whose `.brand` links to `${properties.siteUrl}/` with `img/logo.svg`, the page content in one `.card`, a `.site-footer`. `<title>` is `${msg("loginTitle",(realm.displayName!''))}`.
  - `messages_en.properties`: `loginTitle=Orion account`, `loginTitleHtml=Orion account`, `doRegister=Create account`, `backToApplication=Back to Orion` — and any message Step 1's "no Keycloak" rule trips on.
  - `site.css`: design tokens on `:root` (colours, radius, spacing, one system font stack), light and dark via `prefers-color-scheme`, the classes in Interfaces, form controls and buttons styled by element inside `.card` so pages nobody mapped still look right, a 16 px side gutter at phone width. Use the `frontend-design:frontend-design` skill for the look; the game is a toy-brick fortress builder called Orion.
- [ ] **Step 4:** `just kc-down && just kc-up && just site-theme-check` — expected: `OK` for all six pages.
- [ ] **Step 5: Commit** `feat(website): the orion Keycloak login theme and a dev Keycloak to see it in`.

### Task 2: The login protocol (`oidc.js`)

**Files:**
- Create: `website/site/js/oidc.js`, `website/site/js/config.js`, `website/tests/oidc.test.mjs`
- Modify: `justfile` (`site-test` = `node --test website/tests/`)

**Interfaces:**
- Produces `config.js`: `export const CONFIG` = `{ issuer, clientId: "site", siteUrl, chatUrl, playUrl }`; when `location.hostname` is `127.0.0.1` or `localhost`: issuer `http://127.0.0.1:8180/realms/players`, siteUrl `http://127.0.0.1:8182`; otherwise issuer `https://auth.factoriosurvivalworld.com/realms/players`, siteUrl `https://factoriosurvivalworld.com`. Both: chatUrl `https://chat.factoriosurvivalworld.com/accounts/login/social/oidc/keycloak`, playUrl `https://orion.factoriosurvivalworld.com/`. The production issuer string appears literally in the file (Task 6 checks it).
- Produces `oidc.js` (no `window`, `document` or globals; everything through `env`):
  - `env` = `{ config, pending: Storage, session: Storage, crypto: Crypto, fetch, now: () => number }` (`pending` = sessionStorage, `session` = localStorage in the browser).
  - `async beginLogin(env, { prompt?: "create", kcAction?: "UPDATE_PASSWORD", returnTo?: string }) → string` — the authorization URL (`<issuer>/protocol/openid-connect/auth`; `response_type=code`, `scope=openid profile email`, `redirect_uri=<siteUrl>/callback.html`, `state`, `nonce`, `code_challenge`, `code_challenge_method=S256`, plus `prompt` / `kc_action` when given). Stores `{ state, nonce, verifier, returnTo }` under `orion.pending`.
  - `async completeLogin(env, callbackUrl: string) → { returnTo: string, action: null | { name: string, status: "success" | "cancelled" | "error" } }` — removes `orion.pending` first, checks `state` and the `iss` parameter, exchanges the code at `<issuer>/protocol/openid-connect/token`, reads the ID token's payload (no signature check: it came straight from the token endpoint over TLS, OpenID Connect Core §3.1.3.7), checks `iss`, `aud`, `nonce`, stores `{ username, email, emailVerified, idToken, at }` under `orion.session`. Access and refresh tokens are dropped.
  - `currentSession(env) → { username, email, emailVerified } | null` — null when absent, unparsable or older than `SESSION_MAX_AGE_MS` (8 h; the entry is removed).
  - `logoutUrl(env) → string` — `<issuer>/protocol/openid-connect/logout?client_id=site&post_logout_redirect_uri=<siteUrl>/&id_token_hint=…`; removes `orion.session`.
  - `class OidcError extends Error` with `code`: `no_pending_login`, `state_mismatch`, `issuer_mismatch`, `provider_error`, `token_exchange_failed`, `bad_id_token`.
  - `safeReturnTo(path) → "/" | "/profile.html"`.

- [ ] **Step 1: Write the failing tests** in `website/tests/oidc.test.mjs` (`node:test`, `node:assert/strict`, `globalThis.crypto`; a `fakeEnv({ fetch })` helper with two `Map`-backed storages, `now` settable, issuer `https://kc.test/realms/players`, siteUrl `https://site.test`; a `idToken(claims)` helper building `x.<base64url json>.y`):

```js
test("pkce challenge matches RFC 7636 appendix B", async () => {
  assert.equal(await challengeFor("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", crypto),
               "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
});
test("beginLogin builds the request and remembers it", async () => {
  const url = new URL(await beginLogin(env, { prompt: "create", returnTo: "/profile.html" }));
  assert.equal(url.origin + url.pathname, "https://kc.test/realms/players/protocol/openid-connect/auth");
  assert.equal(url.searchParams.get("redirect_uri"), "https://site.test/callback.html");
  assert.equal(url.searchParams.get("scope"), "openid profile email");
  assert.equal(url.searchParams.get("prompt"), "create");
  const p = JSON.parse(env.pending.getItem("orion.pending"));
  assert.equal(url.searchParams.get("state"), p.state);
  assert.equal(p.returnTo, "/profile.html");
});
test("safeReturnTo accepts only the two pages", () => {
  for (const bad of ["https://evil.test/", "//evil.test", "/profile.html?x=1", "javascript:alert(1)", undefined])
    assert.equal(safeReturnTo(bad), "/");
  assert.equal(safeReturnTo("/profile.html"), "/profile.html");
});
test("completeLogin stores the session and drops the pending login", …);      // session has username/email/emailVerified; pending gone; fetch body has code_verifier
test("completeLogin without a pending login", …);                             // rejects OidcError code no_pending_login; no session written
test("completeLogin rejects a wrong state / wrong iss parameter / error=access_denied", …); // state_mismatch, issuer_mismatch, provider_error; no fetch made
test("completeLogin rejects an id token with a wrong aud / nonce / iss", …);  // bad_id_token; no session written
test("a cancelled password change keeps the session", …);                      // callback has kc_action=UPDATE_PASSWORD&kc_action_status=cancelled → action {name, status:"cancelled"}, session present
test("currentSession expires after SESSION_MAX_AGE_MS", …);                    // now += 8h + 1 → null and the entry removed
test("logoutUrl carries the hint and clears the session", …);
```
  (`challengeFor(verifier, crypto)` is exported for the first test.)
- [ ] **Step 2:** `just site-test` — expected: FAIL, module not found.
- [ ] **Step 3: Implement `config.js` and `oidc.js`** to the Interfaces above. Random values are 32 bytes from `env.crypto.getRandomValues`, base64url without padding.
- [ ] **Step 4:** `just site-test` — expected: all pass.
- [ ] **Step 5: Commit** `feat(website): browser login against Keycloak (authorization code + PKCE), tested under node`.

### Task 3: The pages

**Files:**
- Create: `website/site/{index,profile,callback}.html`, `website/site/js/app.js`, symlinks `website/site/css/site.css → ../../theme/orion/login/resources/css/site.css` and `website/site/img/logo.svg → ../../theme/orion/login/resources/img/logo.svg`
- Modify: `website/theme/orion/login/resources/css/site.css` (landing and profile styles), `justfile` (`site-dev`), `website/tests/oidc.test.mjs` only if a helper moves

**Interfaces:**
- Consumes: `CONFIG`, `beginLogin`, `completeLogin`, `currentSession`, `logoutUrl`, `OidcError` (Task 2); the class names of Task 1.
- Produces: `just site-dev` = `kc-up`, then `python3 -m http.server 8182 --bind 127.0.0.1 --directory website/site`.
- Every page: `<script type="module" src="js/app.js">`, no inline script or style (the production CSP is `script-src 'self'; style-src 'self'`), the same `.site-header` / `.site-footer` markup as the theme's `template.ftl`. Elements carry `data-action="login|signup|logout|change-password"` and `data-show="in|out"`; `app.js` wires them and hides the wrong half.

- [ ] **Step 1: `index.html`** — header (brand; logged out: "Log in", "Create account"; logged in: the username linking to `profile.html`, "Log out"), a hero (`<h1>Orion</h1>`, one paragraph: "Dig, build and defend a fortress of toy bricks in a huge shared world.", buttons "Play in browser" → `CONFIG.playUrl`, "Create account" (`signup`), "Join the chat" → `CONFIG.chatUrl`), a three-item feature strip, footer.
- [ ] **Step 2: `profile.html`** — logged out: one line and a "Log in" button (`returnTo: "/profile.html"`). Logged in: username, email, "verified" / "not verified", buttons "Change password" (`beginLogin` with `kcAction: "UPDATE_PASSWORD"`, `returnTo: "/profile.html"`), "Log out"; a status line that shows "Password changed." / "Password not changed." from `sessionStorage["orion.notice"]` once.
- [ ] **Step 3: `callback.html` + its branch in `app.js`** — runs `completeLogin(env, location.href)`, then `history.replaceState` to drop the query, sets `orion.notice` from `action`, and `location.replace(returnTo)`. On `OidcError`: a `.alert-error` with one sentence per code and a link home; `no_pending_login` says "This sign-in link was already used." Nothing is thrown uncaught.
- [ ] **Step 4:** `just site-test` — expected: still all pass.
- [ ] **Step 5: Drive it in a browser** (`just site-dev`, open `http://127.0.0.1:8182/`): Create account → Keycloak's form in the site's look → back on the site logged in; Log out → logged out and Keycloak's session gone (Log in asks for the password again); Log in as `dev` / `dev-password` → profile shows `dev`, `dev@localhost`, verified; Change password → success notice; again and Cancel → "Password not changed."; reload `callback.html` → the "already used" message; `http://127.0.0.1:8180/realms/players/protocol/openid-connect/auth?…` started from another tab while logged in returns without a prompt. Note each result in the commit body.
- [ ] **Step 6: STOP — the user looks at the site and the login pages in the dev setup** and signs off on design and copy before Task 4.
- [ ] **Step 7: Commit** `feat(website): landing page, profile page and login callback`.

### Task 4: Keycloak on orion carries the theme, without the account console

**Files:**
- Modify: `deploy/modules/keycloak/main.tf`, `deploy/modules/keycloak/variables.tf`, `deploy/envs/orion/keycloak/main.tf`

**Interfaces:**
- Produces module variables: `themes` (`map(map(string))`, default `{}`: theme name → { path relative to the theme → file content }, mounted at `/opt/keycloak/themes/<name>`), `site_url` (`string`, default `"https://factoriosurvivalworld.com"`, set as `ORION_SITE_URL`), `features_disabled` (`list(string)`, default `["account", "account-api"]`, joined into `KC_FEATURES_DISABLED`).
- The root passes `themes = { orion = { for f in fileset(local.theme_dir, "**") : f => file("${local.theme_dir}/${f}") } }` with `theme_dir = abspath("${path.root}/../../../../website/theme/orion")`.

- [ ] **Step 1: Module.** One `kubernetes_config_map_v1` per theme (key = path with `/` replaced by `__`), one volume + read-only mount per theme with `items { key, path }` restoring the paths; a pod-template annotation `checksum/themes = sha256(jsonencode(var.themes))` so a theme change restarts the pod (Keycloak caches themes; `Recreate` = about a minute without logins, say so in the variable's description). A `precondition` refuses a theme whose contents exceed 900 000 bytes. Extend the `image` variable's description: after a bump, re-diff `template.ftl` against the new `base` and run `just site-theme-check` with the new tag in `deploy/dev/compose.yml`. Update the comment at the top of `main.tf`: the account console is off, and why.
- [ ] **Step 2:** `tofu fmt -recursive deploy/modules/keycloak deploy/envs/orion/keycloak && just orion-plan keycloak` — expected: 1 ConfigMap to add, the Deployment updated in place (env, volume, annotation), nothing destroyed.
- [ ] **Step 3: STOP — the user runs `just orion-apply keycloak`.**
- [ ] **Step 4: Verify.** `curl -s -o /dev/null -w '%{http_code}\n' https://auth.factoriosurvivalworld.com/realms/players/account/` → `404`, and the same for `…/realms/staff/account/`; `…/realms/players/.well-known/openid-configuration` → `200`; `just orion-kube -n keycloak exec deploy/keycloak -- ls /opt/keycloak/themes/orion/login` lists `template.ftl`, `theme.properties`, `messages`, `resources`; log in to Grafana and to Zulip once (both still work, still the old look). With `just orion-keycloak-admin` running, open `http://127.0.0.1:8180/admin/`, log in as `admin` and open realm `players` → Users: the admin console works as before.
- [ ] **Step 5: Commit** `feat(deploy): Keycloak mounts folder themes from a ConfigMap; account console off`.

### Task 5: Realm: the theme and the `site` client

**Files:**
- Modify: `deploy/modules/keycloak-realm/{main,clients,variables,outputs}.tf`, `deploy/envs/orion/keycloak-realm/main.tf` (output `site_client_id`)

**Interfaces:**
- Produces variables `login_theme` (default `"orion"`) and `site_url` (default `"https://factoriosurvivalworld.com"`); output `site_client_id`.
- `keycloak_realm.players`: `login_theme = var.login_theme`. The `staff` realm keeps Keycloak's default theme.
- `keycloak_openid_client.site`: `client_id = "site"`, `name = "Website"`, `access_type = "PUBLIC"`, `standard_flow_enabled = true`, `pkce_code_challenge_method = "S256"`, `direct_access_grants_enabled = false`, `root_url = var.site_url`, `base_url = "/"`, `valid_redirect_uris = ["${var.site_url}/callback.html"]`, `valid_post_logout_redirect_uris = ["${var.site_url}/"]`, `web_origins = [var.site_url]`.

- [ ] **Step 1:** Write the changes; `tofu fmt`; with `just orion-keycloak-admin` running, `just orion-plan keycloak-realm` — expected: 1 to add (`site`), 1 to change (`players`: `login_theme`), 0 to destroy.
- [ ] **Step 2: STOP — the user runs `just orion-apply keycloak-realm`.**
- [ ] **Step 3: Verify.** `KC=https://auth.factoriosurvivalworld.com SITE=https://factoriosurvivalworld.com SKIP_LOGIN=1 scripts/site_theme_check.sh` → `OK` for login, register, reset password, error, device code. Open Zulip's login in a private window: Keycloak's page now has the site's look.
- [ ] **Step 4: Commit** `feat(deploy): players realm uses the orion theme; public client for the website`.

### Task 6: The site on orion

**Files:**
- Create: `deploy/modules/site/{versions,variables,main,outputs}.tf`, `deploy/modules/site/nginx.conf.tftpl`, `deploy/envs/orion/site/{backend,versions,providers,main}.tf`
- Modify: `justfile` (the root lists of `orion-init` / `orion-apply` comments), `CLAUDE.md` (Deployment paragraph: root `site` = https://factoriosurvivalworld.com, sources in `website/`, `just site-dev|site-test|site-theme-check`, the theme rule from Task 4)

**Interfaces:**
- Consumes: remote state `orion/keycloak-realm.tfstate` outputs `issuer`, `site_client_id`.
- Module variables: `namespace` (`"site"`), `host` (`"factoriosurvivalworld.com"`), `redirect_hosts` (`["www.factoriosurvivalworld.com"]`), `site_dir`, `issuer`, `image` (`"nginxinc/nginx-unprivileged:1.28-alpine"`), `cluster_issuer` (`"letsencrypt"`), `ingress_class` (`"traefik"`). Output `url`.

- [ ] **Step 1: Module.**
  - ConfigMap `site-files` from `fileset(var.site_dir, "**")` (keys flattened like Task 4, paths restored by `items`; `file()` follows the two symlinks), mounted read-only at `/site`. Preconditions: total under 900 000 bytes; `strcontains(file("${var.site_dir}/js/config.js"), var.issuer)` with the message "website/site/js/config.js must name the realm's issuer".
  - ConfigMap `site-nginx` from `nginx.conf.tftpl`: listen 8080; a server for `redirect_hosts` answering `301 https://${host}$request_uri`; the main server answering the same redirect when `X-Forwarded-Proto` is `http` (Traefik serves an Ingress on both entrypoints), otherwise with `root /site`, `location = /healthz { return 200; }`, and on every response `Cache-Control: no-cache`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src <origin of var.issuer>; form-action 'none'; frame-ancestors 'none'; base-uri 'none'`.
  - Deployment (1 replica, rolling update, `checksum/site` annotation over both ConfigMaps, readiness and liveness on `/healthz`, requests `10m` / `16Mi`, memory limit `64Mi`, `automount_service_account_token = false`), Service, Ingress for `host` and every `redirect_hosts` entry with one TLS secret `site-tls` covering all of them and the `cert-manager.io/cluster-issuer` annotation, `wait_for_load_balancer = false`. No DNS resources.
- [ ] **Step 2: Root** shaped like `deploy/envs/orion/picoshare` (state key `orion/site.tfstate`), `site_dir = abspath("${path.root}/../../../../website/site")`, `issuer` from the realm root's remote state.
- [ ] **Step 3:** `tofu fmt -recursive deploy/modules/site deploy/envs/orion/site && just orion-init site && just orion-plan site` — expected: namespace, 2 ConfigMaps, Deployment, Service, Ingress to add; nothing else.
- [ ] **Step 4: STOP — the user runs `just orion-apply site`.**
- [ ] **Step 5:** Wait for the certificate: `just orion-kube -n site get certificate site-tls` → `READY True` (HTTP-01 for the apex and `www`; both names already resolve to the box).
- [ ] **Step 6: Verify.**
  - `curl -s -o /dev/null -w '%{http_code} %{redirect_url}\n' https://www.factoriosurvivalworld.com/profile.html` → `301 https://factoriosurvivalworld.com/profile.html`; `http://factoriosurvivalworld.com/` → a redirect to `https://`.
  - `curl -sI https://factoriosurvivalworld.com/ | grep -i content-security-policy` shows `connect-src https://auth.factoriosurvivalworld.com`.
  - In a browser, the walk of Task 3 Step 5 on the real site with a real account (sign-up mail arrives, the verify link returns to the site), with the console open: no CSP violation.
  - "Join the chat" while logged in lands in Zulip without a password prompt.
- [ ] **Step 7: Commit** `feat(deploy): the website on orion (https://factoriosurvivalworld.com)`.

## Self-review notes

- **Coverage:** landing page (Task 3), login and sign-up through Keycloak in the site's look (Tasks 1, 5), profile page (Task 3), players kept away from Keycloak's own UI (Task 4: account console off; Task 1: no "Keycloak" wording), Zulip through the same login (Decision 8, Task 5 Step 3, Task 6 Step 6).
- **Order on orion:** `keycloak` (theme present) → `keycloak-realm` (theme selected, client made) → `site`. Selecting the theme before it is mounted would only fall back to Keycloak's default look.
- **Shared with plan keycloak-2:** the dev compose service, its port and `players-realm.json`. Whichever plan lands second extends them.
- **Left open on purpose:** email change and account deletion (Decision 6) want Zulip 13's match-by-subject first; the mail template still has Keycloak's default wording and layout.
