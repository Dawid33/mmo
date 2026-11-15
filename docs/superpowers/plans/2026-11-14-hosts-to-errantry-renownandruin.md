# Every Host Moves Under `errantry.renownandruin.com` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every public host of the project leaves `factoriosurvivalworld.com` and becomes a name under `errantry.renownandruin.com`: the game on `game.`, the website on `www.`, and each service on its own label. The name itself redirects to the website.

**Architecture:** Each service's hostname is a module variable whose default is the production value, and most modules create their own Route 53 record from a `dns_zone` variable. The move changes those defaults, adds a small `dns` root for the records no module makes, and applies the roots in dependency order inside one maintenance window, because logins break between the Keycloak apply and the applies of everything that trusts its issuer URL.

**Tech Stack:** OpenTofu, Route 53, cert-manager (HTTP-01), Traefik, Keycloak, Zulip, Grafana, Garage, static website.

**Spec:** none. Decisions are from the conversation of 2026-11-14 and are recorded in Global Constraints. Companion plan: `2026-11-14-errantry-player-facing-name.md` (the name; run its Tasks 1–3 first — this plan edits some of the same files).

## Global Constraints

- Base name `B` = `errantry.renownandruin.com`. Exact hosts:

  | Service | Old host | New host |
  |---|---|---|
  | Game (browser build, WebTransport) | `orion.factoriosurvivalworld.com` | `game.errantry.renownandruin.com` |
  | Website | `factoriosurvivalworld.com` | `www.errantry.renownandruin.com` |
  | Keycloak | `auth.factoriosurvivalworld.com` | `auth.errantry.renownandruin.com` |
  | Login service | `login.factoriosurvivalworld.com` | `login.errantry.renownandruin.com` |
  | Grafana | `grafana.factoriosurvivalworld.com` | `grafana.errantry.renownandruin.com` |
  | Zulip | `chat.factoriosurvivalworld.com` | `chat.errantry.renownandruin.com` |
  | PicoShare | `share.factoriosurvivalworld.com` | `share.errantry.renownandruin.com` |
  | Garage S3 API | `s3.factoriosurvivalworld.com` | `s3.errantry.renownandruin.com` |
  | Garage website buckets | `<bucket>.s3web.factoriosurvivalworld.com` | `<bucket>.s3web.errantry.renownandruin.com` |

- Names that redirect (301) to the website: `errantry.renownandruin.com` (the base name itself), `renownandruin.com`, `www.renownandruin.com`, and the old `factoriosurvivalworld.com` and `www.factoriosurvivalworld.com` (so old links and search results keep working).
- DNS zone for every new record: `renownandruin.com` (Route 53 zone `Z036063115VMA4MBR4HET`, registered through Route 53 on 2026-11-14). All records are `A` → `5.9.7.138`, TTL 300.
- **Mail does not move** (Dawid, 2026-11-14: e-mail is out of scope for now; Zulip itself does move). Senders stay `accounts-noreply@factoriosurvivalworld.com` and `chat-noreply@factoriosurvivalworld.com` on the existing SES identity. `mail_domain`, `mail_sender` and the Zulip `ses` block are not touched. Moving mail needs a new SES identity with DKIM and is separate work.
- `orion` stays the code name: no root, recipe, bucket, IAM name or context is renamed.
- A straight cutover: the other old service hosts (`auth.`, `login.`, `chat.`, `grafana.`, `share.`, `s3.`) stop answering. Only the website's old names redirect.
- Every apply to the live cluster or DNS is run by Dawid or with his go-ahead after he has read the plan output. `just orion-plan <root>` always before `just orion-apply <root>`. Never print `terraform.tfvars` or `.env`.
- No git worktree; work on `develop`; commit only this plan's hunks (many of these files are untracked or carry unrelated edits — never `git add -A`).
- Dated documents (`docs/superpowers/{plans,specs}/*`, `docs/memories/*`) are not edited.

## Review Focus

1. **A consumer left on the old issuer.** Keycloak's issuer URL is in every token. The login service, Zulip, Grafana and the website each hold it; one left behind fails every login with an issuer mismatch. Task 4 Step 6 checks each.
2. **A certificate asked for before its name resolves.** The cluster issuer is HTTP-01. A new record queried before it exists is cached as missing for up to 15 minutes; cert-manager retries, so a pending certificate is normal for a while, but a root applied while the registration is unfinished fails every challenge. Task 1 Step 1 gates on the delegation.
3. **Zulip after a hostname change.** Old message links, e-mail links, the desktop and mobile apps' saved server and the push-notification registration all carry the old host. Task 5 Step 2 follows Zulip's own "changing hostname" procedure.
4. **A root override that beats the new default.** The Zulip root has its own `host` default, and `terraform.tfvars` files can override anything. Task 2 Step 3 and Task 4 Step 1 look.
5. **The alert webhook.** Alertmanager posts to Zulip by its public URL from `deploy/envs/orion/monitoring/terraform.tfvars`; left on the old host, alerts vanish silently. Task 5 Step 4.

---

### Task 1: The `dns` root — records no module makes

Replaces Task 5 of the companion plan (do this one, not that one).

**Files:**
- Create: `deploy/envs/orion/dns/{backend,versions,providers,main}.tf`
- Modify: `justfile` (root list in the comment above `orion-init`; header comment of the orion section)

**Interfaces:**
- Produces: root `dns`, state key `orion/dns.tfstate`. Records: `game.errantry.renownandruin.com`, `errantry.renownandruin.com`, `www.errantry.renownandruin.com`, `share.errantry.renownandruin.com`, `renownandruin.com`, `www.renownandruin.com`.

- [ ] **Step 1: Check the delegation.** `dig +short NS renownandruin.com @1.1.1.1` must list `ns-499.awsdns-62.com`, `ns-977.awsdns-58.net`, `ns-1621.awsdns-10.co.uk`, `ns-1208.awsdns-23.org`. If it lists nothing, the registration has not finished (`aws route53domains list-operations --region us-east-1`; Route 53 mails a confirmation link that must be clicked): wait. **No later task starts before this passes.**

- [ ] **Step 2: Write the root** by copying `deploy/envs/orion/registry/{backend,versions,providers}.tf` (AWS provider only; keep the unused `kube_context` variable so the generic recipes work; tag `stack = "orion-dns"`). `main.tf`:
  - `variable "zone_name"` default `"renownandruin.com"`; `variable "base"` default `"errantry.renownandruin.com"`; `variable "node_ip"` default `"5.9.7.138"`.
  - `data "aws_route53_zone" "game" { name = var.zone_name }` (the zone exists; it is looked up, not managed, like every module does).
  - `resource "aws_route53_record" "host"` with `for_each = toset(["game.${var.base}", var.base, "www.${var.base}", "share.${var.base}", var.zone_name, "www.${var.zone_name}"])`: `name = each.key`, `type = "A"`, `ttl = 300`, `records = [var.node_ip]`.
  - `output "hosts"` = the record names.

- [ ] **Step 3: Plan, apply, check.** `just orion-init dns && just orion-plan dns` — expected: 6 to add, nothing else. After the apply: `for h in game.errantry errantry www.errantry share.errantry; do dig +short $h.renownandruin.com @1.1.1.1; done; dig +short renownandruin.com @1.1.1.1` — expected: `5.9.7.138` five times.

- [ ] **Step 4: Commit.** `git add deploy/envs/orion/dns` and the `justfile` lines (`git add -p`): `feat(deploy): the dns root — renownandruin.com records no module makes`.

---

### Task 2: Change every hostname default (no apply)

**Files:**
- Modify: `deploy/modules/garage/variables.tf` (`s3_host`, `web_root_domain`, `dns_zone`)
- Modify: `deploy/modules/keycloak/variables.tf` (`host`, `site_url`, `dns_zone`)
- Modify: `deploy/modules/keycloak-realm/variables.tf` (`public_url`, `zulip_host`, `grafana_host`, `site_url`; `game_web_origins` is the companion plan's)
- Modify: `deploy/modules/login/variables.tf` (`host`, `dns_zone`; the `issuer` description's example)
- Modify: `deploy/modules/monitoring/variables.tf` (`grafana_host`, `dns_zone`, `manage_dns_record`)
- Modify: `deploy/modules/zulip/variables.tf` (`host`, `dns_zone`), `deploy/envs/orion/zulip/main.tf:16` (the root's own `host` default), `deploy/envs/orion/zulip/terraform.tfvars.example:5`
- Modify: `deploy/modules/picoshare/variables.tf` (`host`)
- Modify: `deploy/modules/site/variables.tf` (`host`, `redirect_hosts`, the `host` description)
- Modify: `website/site/js/config.js:7,9,10`, `website/site/index.html:35`
- Modify: `deploy/envs/orion/monitoring/terraform.tfvars.example:11`
- Modify: header comments naming a URL in `deploy/envs/orion/{garage,keycloak,login,picoshare,site,zulip}/main.tf`, `justfile:1704-1714`
- Modify: `CLAUDE.md` (Deployment paragraph and the Grafana paragraph: every URL)

**Interfaces:**
- Consumes: nothing.
- Produces: the defaults below; Tasks 3–6 apply them.

- [ ] **Step 1: Set the defaults.** Exact values:

  | Module | Variable | New default |
  |---|---|---|
  | garage | `s3_host` | `"s3.errantry.renownandruin.com"` |
  | garage | `web_root_domain` | `"s3web.errantry.renownandruin.com"` |
  | keycloak | `host` | `"auth.errantry.renownandruin.com"` |
  | keycloak, keycloak-realm | `site_url` | `"https://www.errantry.renownandruin.com"` |
  | keycloak-realm | `public_url` | `"https://auth.errantry.renownandruin.com"` |
  | keycloak-realm | `zulip_host` | `"chat.errantry.renownandruin.com"` |
  | keycloak-realm | `grafana_host` | `"grafana.errantry.renownandruin.com"` |
  | login | `host` | `"login.errantry.renownandruin.com"` |
  | monitoring | `grafana_host` | `"grafana.errantry.renownandruin.com"` |
  | monitoring | `manage_dns_record` | `true` (the old record lived in the survival-world infra; the new one is made here) |
  | zulip (module and root) | `host` | `"chat.errantry.renownandruin.com"` |
  | picoshare | `host` | `"share.errantry.renownandruin.com"` |
  | site | `host` | `"www.errantry.renownandruin.com"` |
  | site | `redirect_hosts` | `["errantry.renownandruin.com", "renownandruin.com", "www.renownandruin.com", "factoriosurvivalworld.com", "www.factoriosurvivalworld.com"]` |
  | garage, keycloak, login, monitoring, zulip | `dns_zone` | `"renownandruin.com"` |

  Not touched: `mail_domain`, `mail_sender`, the Zulip `ses` block.

- [ ] **Step 2: Website.** `config.js`: issuer `https://auth.errantry.renownandruin.com/realms/players`, `siteUrl` `https://www.errantry.renownandruin.com`, `chatUrl` `https://chat.errantry.renownandruin.com/accounts/login/social/oidc/keycloak`. `index.html:35`: the same chat URL. The dev values (`127.0.0.1`) stay.

- [ ] **Step 3: Comments, examples, `CLAUDE.md`.** Replace each old URL with its new one from the table in Global Constraints. In `CLAUDE.md` add `dns` to the chain of roots (`dns` first: "the `renownandruin.com` records no module makes").

- [ ] **Step 4: Check nothing is left.**

  Run: `grep -rn "factoriosurvivalworld" deploy website justfile CLAUDE.md --include='*' -I | grep -v '\.terraform/\|terraform\.tfvars:'`
  Expected: only the mail lines (`mail_domain`, `mail_sender`, `ses`, `accounts-noreply@`, `chat-noreply@`), the two old names in `redirect_hosts`, and comments that describe mail.

- [ ] **Step 5: Tests.** `just site-test` — expected: pass. `for r in garage keycloak login monitoring zulip picoshare site; do tofu -chdir=deploy/envs/orion/$r validate; done` — expected: each `Success`.

- [ ] **Step 6: Commit** the tracked files of this task (`git add -p` where a file has other edits): `feat(deploy): every host moves under errantry.renownandruin.com`. Tell Dawid which edited files are untracked and so not in the commit.

---

### Task 3: Object store hosts (`garage`, `garage-buckets`)

No login depends on these, so they go first and outside the window. Pods reach Garage by its in-cluster endpoint; only the public names change.

**Files:** none.

- [ ] **Step 1: `garage`.** `just orion-plan garage` — expected: the Route 53 records replaced (old zone → new zone, new names), the ingress hosts and the certificate changed; no change to the StatefulSet's volumes. Apply. Check: `curl -sI https://s3.errantry.renownandruin.com` answers with a valid certificate (Garage's 403 is the right answer).
- [ ] **Step 2: `garage-buckets`.** Needs `just orion-garage-admin`. `just orion-plan garage-buckets` — expected: outputs only (`s3_url`, `website_urls`) or no change. Apply so the roots that read this state see the new URLs.

---

### Task 4: The login window (`keycloak`, `keycloak-realm`, `login`, `site`)

Run these four back to back. From Step 2 until Step 5 nobody can log in.

**Files:** none.

- [ ] **Step 1: Overrides.** Read the variable NAMES set in `deploy/envs/orion/{keycloak,keycloak-realm,login,site}/terraform.tfvars`; none may be a host, URL or `dns_zone`. (2026-11-14: `keycloak-realm` sets only `keycloak_client_secret`.)
- [ ] **Step 2: `keycloak`.** `just orion-plan keycloak` — expected: the Route 53 record replaced, the hostname and `ORION_SITE_URL` in the pod's environment, ingress and certificate; the PostgreSQL StatefulSet untouched. Apply; wait for the certificate (`just orion-kube -n keycloak get certificate`).
- [ ] **Step 3: `keycloak-realm`.** Needs `just orion-keycloak-admin`. Plan — expected: redirect URIs and web origins of the `zulip`, `grafana`, `site` and `game` clients, nothing about users. Apply.
- [ ] **Step 4: `login`.** Plan — expected: record replaced, ingress and certificate, `VOXEL_OIDC_ISSUER` (read from the realm root's state) and the CORS origin; no image change. Apply.
- [ ] **Step 5: `site`.** Plan — expected: the files ConfigMap (new `config.js`), host, redirect hosts, certificate. Apply.
- [ ] **Step 6: Check every consumer of the issuer.**
  - `curl -s https://auth.errantry.renownandruin.com/realms/players/.well-known/openid-configuration | grep -o '"issuer":"[^"]*"'` — expected: the new URL.
  - `https://www.errantry.renownandruin.com/`: log in, land on the profile page.
  - `curl -sI https://errantry.renownandruin.com/ https://renownandruin.com/ https://factoriosurvivalworld.com/` — expected: `301` to `https://www.errantry.renownandruin.com/` each.
  - `just orion-login-logs`: both pods started; no issuer or JWKS error.
  - Wherever `VOXEL_LOGIN_URL` is set for a public client build or a game host, it becomes `https://login.errantry.renownandruin.com` (it is not in this repository's files; Dawid knows where).

---

### Task 5: Staff and team services (`monitoring`, `zulip`, `picoshare`)

**Files:** none (plus one line in the git-ignored `deploy/envs/orion/monitoring/terraform.tfvars`, edited by Dawid).

- [ ] **Step 1: `monitoring`.** Plan — expected: a new Route 53 record, Grafana's host, root URL, OIDC issuer (`staff_issuer` from the realm state), ingress and certificate; no PVC change. Apply. Check: `https://grafana.errantry.renownandruin.com` logs in through Keycloak; with the token from `.env`, `GET /api/health` answers.
- [ ] **Step 2: `zulip`.** Before the apply read Zulip's "Changing hostname" page for the running version (`docs/production/` in the Zulip docs) and follow what it says about the push-notification registration. Plan — expected: record replaced, `SETTING_EXTERNAL_HOST`, the avatars URL (from Garage's new website host), OIDC issuer, ingress and certificate; no change to the PostgreSQL or data volumes. Apply. Check: log in through Keycloak at `https://chat.errantry.renownandruin.com`, an old uploaded file opens, an avatar shows, `just orion-zulip-test-email <address>` arrives.
- [ ] **Step 3: `picoshare`.** Plan — expected: ingress host and certificate. Apply. Check: `https://share.errantry.renownandruin.com` answers.
- [ ] **Step 4: Alert webhook.** Dawid changes the host in `zulip_alert_webhook_url` (`deploy/envs/orion/monitoring/terraform.tfvars`) to `chat.errantry.renownandruin.com` and re-applies `monitoring`. Check: fire a test alert (`amtool` or a silenced-then-unsilenced rule) and see it in the `alerts` stream.
- [ ] **Step 5: Tell the people who use them.** The Zulip desktop and mobile apps need the new server URL; bookmarks to Grafana and PicoShare are dead.

---

### Task 6: Clean up the old zone (survival-world repository)

**Files:**
- Modify: `~/Projects/mod/mods/survival-world/infra/main.tf` (records `orion.`, `share.`, and `grafana.` in the `for_each` at line 176 if it is listed there)

- [ ] **Step 1: Remove the records of hosts that no longer answer**: `orion.`, `share.`, `grafana.`. **Keep** the apex and `www.` records (they redirect to the new website) and everything about mail (MX, SES DKIM, `_dmarc`, MAIL FROM).
- [ ] **Step 2: Plan and apply** with that repository's own commands. Expected: only those records destroyed. That repository has uncommitted changes of its own: touch only these resources.
- [ ] **Step 3: Final check from outside.** For each new host in the Global Constraints table: `curl -sI https://<host>/` shows a valid certificate. For each old service host: it no longer resolves, or answers with Traefik's 404.

**Not in this plan:** serving the game itself at `game.errantry.renownandruin.com` (porting `just deploy` to k3s: an ingress for the bundle, a certificate for the WebTransport endpoint), and moving mail to the new domain.
