# Keycloak 1 — Identity provider on orion, Zulip signs in through it — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Keycloak server on the orion k3s box holds every player account (sign-up, email verification, password reset), and Zulip at `chat.factoriosurvivalworld.com` logs people in through it over OpenID Connect, creating their Zulip account on first login.

**Architecture:** Two new OpenTofu roots. `keycloak` runs the server (one Keycloak pod + its own small PostgreSQL, Traefik Ingress exposing only the public paths). `keycloak-realm` configures it through Keycloak's own OpenTofu provider: realm `players`, the user profile, outgoing mail through the existing SES identity, and two clients (`zulip`, confidential; `game`, public, for plan 2). The `zulip` root then reads the realm root's outputs from remote state and turns on Zulip's generic OpenID Connect backend.

**Tech Stack:** OpenTofu ≥ 1.10; providers `hashicorp/kubernetes ~> 3.2`, `hashicorp/aws ~> 6.0`, `hashicorp/random ~> 3.7`, `keycloak/keycloak ~> 5.10`; images `quay.io/keycloak/keycloak:26.8.2`, `postgres:17-alpine`; Zulip chart 2.4.0 (server 12.4) as deployed.

**Spec:** none written. The decisions below (user, 2026-11-12, in the session that deployed Zulip) are the spec.

## Why two plans

The work has two subsystems that share one seam, the realm's `game` client: infrastructure (this plan) and the game's login path (`2026-11-12-keycloak-2-game-login.md`). This plan ships on its own: when it is done, people register at Keycloak and use Zulip with that account, and the game still logs in the old way.

## Decisions settled in planning

1. **Keycloak is the identity provider**, self-hosted on orion. Public URL `https://auth.factoriosurvivalworld.com`.
2. **Its PostgreSQL is its own** (a plain StatefulSet in the same module). The CNPG operator planned in `platform` is for the game world only (user, 2026-11-12); do not use it here.
3. **One realm, `players`.** The `master` realm is for administration only and is never exposed.
4. **The username is the player's public name.** Sign-up asks for username (3–24 chars, `[A-Za-z0-9_-]`), email and password; no first / last name. Zulip's full name is the username.
5. **Email must be verified** before any login. Mail goes through the SES domain identity `factoriosurvivalworld.com` (eu-west-1, owned by `~/Projects/mod/mods/survival-world/infra`), From `accounts-noreply@factoriosurvivalworld.com`, with a send-only IAM user made here.
6. **Admin console and admin API are not public.** The Ingress routes only `/realms/`, `/resources/`, `/.well-known/`. (Corrected 2026-11-12: the `master` realm's login pages under `/realms/master/` ARE public. The first version blocked them and the admin console could not log in, because it always authenticates against the public address.) Admin access is a port-forward to `http://127.0.0.1:8180` (`just orion-keycloak-admin`); the `keycloak-realm` root talks to that address, so the forward must be running for its plan / apply.
7. **Zulip matches accounts by email** (server 12.4; matching by subject needs 13.0). The existing Zulip owner keeps their account by registering in Keycloak with the same email.
8. **Zulip password login is switched off in a second step**, after the owner has logged in through Keycloak once (`password_login`, Task 4).
9. **In-game chat is out of scope.** So is migrating the game's login (plan 2).

10. **The realm root logs in as a service account** (client `opentofu` in the `master` realm, client credentials, secret in the root's git-ignored `terraform.tfvars`), not as `admin` (added 2026-11-12: the human `admin` got two-factor login, which the provider's password grant cannot pass). The client is made once by hand; `deploy/envs/orion/keycloak-realm/terraform.tfvars.example` has the steps.

## Global Constraints

- Work on `develop` in the main checkout. Never create a git worktree. The tree holds the user's uncommitted work: `git add` only a task's own files.
- OpenTofu, not Terraform. Every command goes through the `just orion-*` recipes, which pin the `orion` kube context.
- **Plan before every apply, and the user runs every `just orion-apply …`.** An agent stops at each apply step, shows the plan summary and waits.
- Never print or commit secrets. `deploy/**/*.tfvars` is git-ignored; secrets live in state and Kubernetes Secrets.
- Follow the existing modules' shape (`deploy/modules/picoshare`, `deploy/modules/zulip`): `versions.tf`, `variables.tf` with a description on every non-obvious variable, `main.tf`, `outputs.tf`; a root has `backend.tf`, `versions.tf`, `providers.tf`, `main.tf`.
- IAM tag values may only contain letters, digits, spaces and `_ . : / = + - @` (no parentheses).
- `local-path` volumes cannot grow: sizes are final.
- Keep the `justfile` and the Deployment paragraph of `CLAUDE.md` in sync in the same task that adds a root.
- Run `tofu fmt -recursive` on touched directories before each commit.

## Review Focus

1. **Zulip pod cannot reach `https://auth.factoriosurvivalworld.com`** (a pod calling the node's own public IP). Expected: OIDC discovery works from inside `zulip-0`. Pinned by Task 4 Step 5, with the fallback named there.
2. **An unverified email logs in.** Expected: Keycloak blocks login until the email is verified, so Zulip can never match an account on an unproven address. Pinned by Task 3 Step 6.
3. **The admin console is reachable from the internet.** Expected: `https://auth.factoriosurvivalworld.com/admin/` and `/realms/master/` answer 404. Pinned by Task 1 Step 6.
4. **Keycloak restarts and loses everything.** Expected: realm and users survive a pod delete (state is in PostgreSQL on a PV). Pinned by Task 1 Step 7.
5. **The owner is locked out of Zulip** when password login is turned off before their Keycloak login works. Expected: the two-step switch in Task 4 makes this impossible in the documented order. Pinned by Task 4 Steps 6–8.

## File structure

```
deploy/modules/keycloak/            server: namespace, postgres, keycloak, ingress, DNS, ServiceMonitor
deploy/envs/orion/keycloak/         root (state key orion/keycloak.tfstate)
deploy/modules/ses-smtp-user/       a send-only SES SMTP login for one From address (reusable)
deploy/modules/keycloak-realm/      realm `players`, user profile, SMTP, clients, mappers
deploy/envs/orion/keycloak-realm/   root (state key orion/keycloak-realm.tfstate)
deploy/modules/zulip/oidc.tf        new: Zulip's OpenID Connect settings
deploy/modules/zulip/{main,variables}.tf, deploy/envs/orion/zulip/main.tf   modified
justfile, CLAUDE.md                 modified
```

---

### Task 1: Keycloak server root

**Files:**
- Create: `deploy/modules/keycloak/{versions,variables,main,postgres,outputs}.tf`
- Create: `deploy/envs/orion/keycloak/{backend,versions,providers,main}.tf`
- Modify: `justfile` (orion section, after `orion-zulip-test-email`), `CLAUDE.md` (Deployment paragraph)

**Interfaces:**
- Produces (module outputs, re-exported by the root): `url` = `"https://${var.host}"`; `admin_url` = `"http://127.0.0.1:8180"`; `admin_username` = `"admin"`; `admin_password` (sensitive); `namespace`.
- Produces: Service `keycloak` in namespace `keycloak`, port `http` 8080 and `management` 9000.
- Produces recipes: `orion-keycloak-admin` (port-forward `svc/keycloak 8180:8080`), `orion-keycloak-admin-password`.

Module variables and defaults: `namespace = "keycloak"`, `host = "auth.factoriosurvivalworld.com"`, `image = "quay.io/keycloak/keycloak:26.8.2"`, `postgres_image = "postgres:17-alpine"`, `storage_class = "local-path"`, `postgres_volume_size = "10Gi"`, `memory_limits = { keycloak = "2Gi", postgres = "1Gi" }`, `cluster_issuer = "letsencrypt"`, `ingress_class = "traefik"`, `manage_dns_record = true`, `dns_zone = "factoriosurvivalworld.com"`, `node_ip = "5.9.7.138"`, `metrics = true`.

- [ ] **Step 1: Write the module.**
  - `postgres.tf`: `random_password.db`; Secret `keycloak-db` (`password`); headless Service + StatefulSet `keycloak-postgres` (1 replica, `POSTGRES_DB=keycloak`, `POSTGRES_USER=keycloak`, `POSTGRES_PASSWORD` from the Secret, `PGDATA=/var/lib/postgresql/data/pgdata`, volume claim template of `postgres_volume_size`, readiness `pg_isready -U keycloak`).
  - `main.tf`: `random_password.admin`; Secret `keycloak-admin`; Deployment `keycloak` (1 replica, strategy `Recreate`, args `["start"]`) with env exactly:
    `KC_DB=postgres`, `KC_DB_URL=jdbc:postgresql://keycloak-postgres:5432/keycloak`, `KC_DB_USERNAME=keycloak`, `KC_DB_PASSWORD` (Secret), `KC_HOSTNAME=https://${var.host}`, `KC_HOSTNAME_ADMIN=http://127.0.0.1:8180`, `KC_PROXY_HEADERS=xforwarded`, `KC_HTTP_ENABLED=true`, `KC_HEALTH_ENABLED=true`, `KC_METRICS_ENABLED=true`, `KC_BOOTSTRAP_ADMIN_USERNAME=admin`, `KC_BOOTSTRAP_ADMIN_PASSWORD` (Secret).
    Ports `http` 8080, `management` 9000. Startup probe `GET /health/started` on 9000 (period 5 s, failure threshold 60), readiness `/health/ready`, liveness `/health/live`. Requests `cpu 100m, memory 768Mi`; limit memory `var.memory_limits["keycloak"]`. `automount_service_account_token = false`.
  - Service `keycloak` (both ports). Ingress `keycloak` (cert-manager annotation, TLS secret `keycloak-tls`) with three `Prefix` paths only: `/realms/`, `/resources/`, `/.well-known/`. A second Ingress rule is NOT added for `/admin/`.
  - Keep the admin realm private too: a fourth, longer Ingress path `/realms/master/` points at a Service `keycloak-blackhole` whose selector matches no pod, so Traefik answers 503 for it. Comment why in the file.
  - Route53 A record behind `manage_dns_record`, as `deploy/modules/zulip/s3.tf` does it.
  - `kubernetes_manifest` is not used anywhere in `deploy/`; for the ServiceMonitor (behind `var.metrics`) ship a tiny local chart `deploy/modules/keycloak/charts/keycloak-monitor/` and a `helm_release`, the way `deploy/modules/traefik-config` ships its custom resource. Endpoint: port `management`, path `/metrics`, interval `30s`. This adds the `helm` provider to `versions.tf` and the root.
- [ ] **Step 2: Write the root** by copying `deploy/envs/orion/zulip/{backend,versions,providers}.tf` (drop the `aws.ses` alias and `ses_region`; set the stack tag to `orion-keycloak`); `main.tf` instantiates the module and re-exports the outputs. Header comment: needs `../platform` and (for the ServiceMonitor) `../monitoring` applied first.
- [ ] **Step 3: Add the recipes and docs.** `orion-keycloak-admin port="8180"`: `kubectl --context {{orion_context}} -n keycloak port-forward svc/keycloak {{port}}:8080`. `orion-keycloak-admin-password`: `tofu … output -raw admin_password`. Add `keycloak` and `keycloak-realm` to the root lists in the orion comment block, the `orion-init` comment and the apply-order comment (`platform`, then `monitoring`, then `keycloak` → `keycloak-realm` → `zulip`), and to `CLAUDE.md`'s roots list (`keycloak` = https://auth.factoriosurvivalworld.com).
- [ ] **Step 4: Validate and plan.**
  Run: `just orion-init keycloak && tofu -chdir=deploy/envs/orion/keycloak validate && just orion-plan keycloak`
  Expected: `Success! The configuration is valid.` and a plan that only adds (namespace, 2 Secrets, 2 passwords, StatefulSet, Deployment, Services, Ingress, record, helm release).
- [ ] **Step 5: Stop. The user runs `just orion-apply keycloak`.** Then wait for `kubectl --context orion -n keycloak rollout status deploy/keycloak --timeout=10m`.
- [ ] **Step 6: Verify exposure.**
  Run: `for p in /realms/master/.well-known/openid-configuration /admin/ /metrics /health; do curl -s -o /dev/null -w "%{http_code} $p\n" https://auth.factoriosurvivalworld.com$p; done`
  Expected: none is `200` (`404` or `503`). And `curl -s https://auth.factoriosurvivalworld.com/resources/ -o /dev/null -w '%{http_code}\n'` is not a TLS error (certificate issued).
- [ ] **Step 7: Verify persistence and admin access.** With `just orion-keycloak-admin` running in another terminal, `curl -s http://127.0.0.1:8180/realms/master/.well-known/openid-configuration | grep -c issuer` prints `1`. Then `just orion-kube -n keycloak delete pod -l app.kubernetes.io/name=keycloak`, wait for rollout, and the same curl still prints `1` and the admin password from `just orion-keycloak-admin-password` still logs in at `http://127.0.0.1:8180/admin/`.
- [ ] **Step 8: Commit.** `git add deploy/modules/keycloak deploy/envs/orion/keycloak justfile CLAUDE.md` (hunks of this task only) — `feat(deploy): Keycloak server on orion (own PostgreSQL, public paths only)`.

### Task 2: Reusable SES SMTP login module

**Files:**
- Create: `deploy/modules/ses-smtp-user/{versions,variables,main,outputs}.tf`

**Interfaces:**
- Consumes: providers `aws` and `aws.ses` (`configuration_aliases = [aws.ses]`), as `deploy/modules/zulip/versions.tf` declares them.
- Produces: variables `name` (IAM user name), `domain` (existing SES identity), `sender` (the one From address), `purpose` (tag text); outputs `host` (`email-smtp.<ses region>.amazonaws.com`), `port` (`587`), `username`, `password` (sensitive, `ses_smtp_password_v4`), `sender`.

- [ ] **Step 1: Write the module** as a generalisation of `deploy/modules/zulip/ses.tf` (same policy: `ses:SendRawEmail`, `ses:SendEmail` on the identity ARN, condition `ses:FromAddress` = `var.sender`; the access key uses `provider = aws.ses`). `deploy/modules/zulip/ses.tf` is NOT migrated to it in this plan (it is applied state; moving it is a separate change).
- [ ] **Step 2: Validate** inside Task 3 (a module has no root of its own).
- [ ] **Step 3: Commit** with Task 3.

### Task 3: Realm `players`, mail, clients

**Files:**
- Create: `deploy/modules/keycloak-realm/{versions,variables,main,clients,outputs}.tf`
- Create: `deploy/envs/orion/keycloak-realm/{backend,versions,providers,main}.tf`

**Interfaces:**
- Consumes: Task 1's `admin_url`, `admin_username`, `admin_password` through `data "terraform_remote_state" "keycloak"` (backend `s3`, same bucket / region as `deploy/envs/orion/backend.s3.tfbackend`, key `orion/keycloak.tfstate`); Task 2's module.
- Produces (root outputs, read by Task 4 and by plan 2):
  - `issuer` = `"https://auth.factoriosurvivalworld.com/realms/players"`
  - `zulip_client_id` = `"zulip"`, `zulip_client_secret` (sensitive)
  - `game_client_id` = `"game"`, `login_audience` = `"mmo-login"`

Provider block in the root: `provider "keycloak" { client_id = "admin-cli", username = <remote state>, password = <remote state>, url = <remote state admin_url> }`. Header comment: `just orion-keycloak-admin` must be running.

- [ ] **Step 1: Write `main.tf`** (realm):
  - `keycloak_realm.players`: `realm = "players"`, `display_name = "Game account"`, `registration_allowed = true`, `registration_email_as_username = false`, `login_with_email_allowed = true`, `duplicate_emails_allowed = false`, `verify_email = true`, `reset_password_allowed = true`, `remember_me = true`, `edit_username_allowed = false`, `ssl_required = "external"`; `security_defenses { brute_force_detection { max_login_failures = 10 } }`; `smtp_server` from the `ses-smtp-user` module (`starttls = true`, `from = var.mail_sender`, `from_display_name = "Game account"`, `auth { username, password }`).
  - `module "smtp"` (Task 2) with `name = "mmo-orion-keycloak-smtp"`, `domain = "factoriosurvivalworld.com"`, `sender = "accounts-noreply@factoriosurvivalworld.com"`.
  - `keycloak_realm_user_profile.players`: attribute `username` (required for `user`, validators `length { min = 3, max = 24 }` and `pattern = "^[A-Za-z0-9_-]+$"`, edit permission `admin` only after creation is covered by `edit_username_allowed = false`), attribute `email` (required, validator `email`). No `firstName` / `lastName` attributes.
- [ ] **Step 2: Write `clients.tf`**:
  - `keycloak_openid_client.zulip`: `client_id = "zulip"`, `access_type = "CONFIDENTIAL"`, `standard_flow_enabled = true`, `valid_redirect_uris = ["https://${var.zulip_host}/complete/oidc/"]`, `web_origins = []`. `var.zulip_host` default `chat.factoriosurvivalworld.com`.
  - `keycloak_openid_user_property_protocol_mapper.zulip_name`: on the `zulip` client, `user_property = "username"`, `claim_name = "name"`, in ID token and userinfo (Zulip takes the full name from `name`).
  - `keycloak_openid_client.game`: `client_id = "game"`, `access_type = "PUBLIC"`, `standard_flow_enabled = true` with `pkce_code_challenge_method = "S256"` and `valid_redirect_uris` = each web origin + `/*` (the provider refuses `web_origins` on a client with no browser flow; found at apply), `direct_access_grants_enabled = false`, `oauth2_device_authorization_grant_enabled = true`, `web_origins = var.game_web_origins` (default `["https://orion.factoriosurvivalworld.com"]`).
  - `keycloak_openid_audience_protocol_mapper.game_login`: on `game`, `included_custom_audience = "mmo-login"`, `add_to_access_token = true`.
- [ ] **Step 3: Write the root** (providers: `aws` eu-central-1, `aws.ses` eu-west-1, `keycloak`; no kubernetes provider) and the outputs above.
- [ ] **Step 4: Validate and plan** (with the port-forward running).
  Run: `just orion-init keycloak-realm && tofu -chdir=deploy/envs/orion/keycloak-realm validate && tofu -chdir=deploy/envs/orion/keycloak-realm plan -input=false`
  Expected: valid; plan only adds. `just orion-plan` passes `-var kube_context=…`, which this root does not declare: add `variable "kube_context" { default = "orion" }` (unused, documented as such) so the shared recipe works.
- [ ] **Step 5: Stop. The user runs `just orion-apply keycloak-realm`.**
- [ ] **Step 6: Verify the realm.**
  Run: `curl -s https://auth.factoriosurvivalworld.com/realms/players/.well-known/openid-configuration | python3 -c 'import sys,json; d=json.load(sys.stdin); print(d["issuer"]); print("device" , "device_authorization_endpoint" in d)'`
  Expected: `https://auth.factoriosurvivalworld.com/realms/players` and `device True`.
  Manual: open `https://auth.factoriosurvivalworld.com/realms/players/account/`, register with a real address; the form asks for username, email, password only; login is refused until the mailed link is opened; the mail arrives from `accounts-noreply@factoriosurvivalworld.com`.
- [ ] **Step 7: Commit.** `feat(deploy): Keycloak realm "players" (user profile, SES mail, zulip + game clients)`.

### Task 4: Zulip logs in through Keycloak

**Files:**
- Create: `deploy/modules/zulip/oidc.tf`
- Modify: `deploy/modules/zulip/variables.tf`, `deploy/modules/zulip/main.tf` (`local.app_secrets`, `local.secret_env`, `local.plain_env`), `deploy/envs/orion/zulip/main.tf`, `deploy/envs/orion/zulip/terraform.tfvars.example`

**Interfaces:**
- Consumes: Task 3 outputs through `data "terraform_remote_state" "keycloak_realm"` in the zulip ROOT (key `orion/keycloak-realm.tfstate`), behind root variable `keycloak_login` (bool, default `true`).
- Produces module variables: `oidc` (object `{ issuer = string, client_id = string, display_name = optional(string, "Game account") }`, default `null`), `oidc_client_secret` (string, sensitive, default `null`), `password_login` (bool, default `true`).

- [ ] **Step 1: Write `oidc.tf`** with one local, `oidc_env`, empty when `var.oidc == null`, else:
  - `SETTING_SOCIAL_AUTH_OIDC_ENABLED_IDPS` = the single-line Python literal
    `{"keycloak": {"oidc_url": "<issuer>", "display_name": "<display_name>", "display_icon": None, "client_id": "<client_id>", "secret": get_secret("social_auth_oidc_secret"), "auto_signup": True}}`
    (docker-zulip's entrypoint writes a value shaped `{…}` into `settings.py` verbatim, where `get_secret` is in scope).
  - `SETTING_SOCIAL_AUTH_OIDC_FULL_NAME_VALIDATED` = `"True"` (new users skip Zulip's registration form).
  - `ZULIP_AUTH_BACKENDS` = `"EmailAuthBackend,GenericOpenIdConnectBackend"` when `var.password_login`, else `"GenericOpenIdConnectBackend"`.
- [ ] **Step 2: Wire it in `main.tf`**: merge `local.oidc_env` into `local.plain_env`; add `"social-auth-oidc-secret" = var.oidc_client_secret` to `local.app_secrets` when `var.oidc != null`; add `SECRETS_social_auth_oidc_secret = "social-auth-oidc-secret"` to the map `local.secret_env` is built from, with the same "only when set" filter style the `email-password` entry uses. Add a `precondition` on the helm release: `var.oidc == null || var.oidc_client_secret != null`, and one refusing `password_login = false` while `oidc == null` ("no way to log in").
- [ ] **Step 3: Wire the root**: `keycloak_login` and `password_login` variables; the remote state data source with `count = var.keycloak_login ? 1 : 0`; pass `oidc = { issuer = …outputs.issuer, client_id = …outputs.zulip_client_id }` and `oidc_client_secret`. Document both variables in `terraform.tfvars.example`.
- [ ] **Step 4: Validate and plan.**
  Run: `tofu -chdir=deploy/envs/orion/zulip validate && just orion-plan zulip`
  Expected: valid; `helm_release.zulip` and `kubernetes_secret_v1.app` change in place, nothing is destroyed. The values diff shows the three settings above and no secret text.
- [ ] **Step 5: Check reachability BEFORE applying.**
  Run: `just orion-kube -n zulip exec zulip-0 -c zulip -- curl -s -o /dev/null -w '%{http_code}\n' https://auth.factoriosurvivalworld.com/realms/players/.well-known/openid-configuration`
  Expected: `200`. If it times out, the pod cannot reach the node's own public IP (k3s hairpin). Do not work around it (no CoreDNS rewrite, no in-cluster issuer URL: the issuer Zulip sees must equal the one browsers see). Stop and report to the user; it needs their decision.
- [ ] **Step 6: Stop. The user runs `just orion-apply zulip`** (password login still on). Wait for `zulip-0` to be ready.
- [ ] **Step 7: Verify login through Keycloak.** Manual: the Zulip login page shows a "Log in with Game account" button; a Keycloak user with a verified email that has no Zulip account lands in Zulip with full name = their username, without typing a Zulip password; the existing owner (same email in Keycloak) lands in their existing owner account.
- [ ] **Step 8: Turn password login off.** Set `password_login = false` in `deploy/envs/orion/zulip/terraform.tfvars`; `just orion-plan zulip` shows only `ZULIP_AUTH_BACKENDS` changing; the user applies; the Zulip login page shows only the Keycloak button and `curl -s https://chat.factoriosurvivalworld.com/api/v1/server_settings` reports `"password": false`.
- [ ] **Step 9: Commit.** `feat(deploy): Zulip signs in through Keycloak (OpenID Connect, accounts made on first login)`.

### Task 5: Grafana logs in through Keycloak (added 2026-11-12, user request)

**Files:**
- Modify: `deploy/modules/keycloak-realm/{clients,variables,outputs}.tf`, `deploy/envs/orion/keycloak-realm/main.tf`
- Modify: `deploy/modules/monitoring/{main,variables}.tf`, `deploy/modules/monitoring/values/kube-prometheus-stack.yaml.tftpl`, `deploy/envs/orion/monitoring/main.tf`

**Interfaces:**
- Produces (realm root outputs): `grafana_client_id` = `"grafana"`, `grafana_client_secret` (sensitive). Client roles `admin`, `editor`, `viewer` on the `grafana` client; claim `grafana_roles` (list) in ID token, access token and userinfo.
- Produces (monitoring module): `grafana_oidc` (object `{ issuer, client_id, display_name }`, default `null`), `grafana_oidc_client_secret`; root variable `keycloak_login` (bool, default `false`) reading the realm root's state.

Decision (user, 2026-11-12, replacing the first version that reused `players`): Grafana logs in against a separate **`staff` realm** (`deploy/modules/keycloak-realm/staff.tf`: no self-registration, accounts made by an administrator, two-factor required at first login; output `staff_issuer`). The user accepts having a second account. Grafana's role mapping is still **strict** (`role_attribute_strict: true`): a person with no grafana client role is refused, never made a Viewer. The local `admin` login stays as the way in when Keycloak is down.

- [ ] **Step 1:** The realm client, roles and mapper are applied with Task 3's apply.
- [ ] **Step 2:** In the admin console (`just orion-keycloak-admin`), switch to realm `staff`, create your staff user (Users → Add user, then Credentials → set a temporary password), and give it the `grafana` client role `admin`: Role mapping → Assign role → client roles.
- [ ] **Step 3:** Set `keycloak_login = true` in `deploy/envs/orion/monitoring/terraform.tfvars`; `just orion-plan monitoring` shows the `auth.generic_oauth` block and the `grafana-oidc` Secret; the user applies.
- [ ] **Step 4: Verify.** The Grafana login page shows "Sign in with Game account"; your user lands as Admin; a Keycloak user with no grafana role is refused with a role error; `just orion-grafana-password` + user `admin` still logs in.

## Self-review notes

- Every decision 1–8 has a task: 1–3, 6 → Task 1 / 3; 4, 5 → Task 3; 7, 8 → Task 4. Decision 9 has none on purpose.
- Names used across tasks: outputs `admin_url` / `admin_username` / `admin_password` (Task 1 → 3), `issuer` / `zulip_client_id` / `zulip_client_secret` / `game_client_id` / `login_audience` (Task 3 → 4 and plan 2), module variables `oidc` / `oidc_client_secret` / `password_login` (Task 4).
- Not verified while planning, to confirm in the task that meets it: the exact attribute names of `keycloak_realm_user_profile` and `oauth2_device_authorization_grant_enabled` in provider 5.10 (Task 3 Step 4's `validate` settles both); whether a pod on orion can reach the node's public IP (Task 4 Step 5).
- Known limit, accepted: disabling a user in Keycloak does not log them out of Zulip sessions that already exist; deactivate them in Zulip as well.
