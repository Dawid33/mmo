# Login service on the orion k3s cluster — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The game's login service (`crates/login`) runs on the orion k3s cluster at `https://login.factoriosurvivalworld.com`, signs players in through the Keycloak already there, and the deployed game can be switched to it.

**Architecture:** One new OpenTofu module and root (`login`), shaped like `keycloak`: a namespace, the login service's own PostgreSQL (StatefulSet + volume), two `login serve` pods behind one Service (rolling updates, a disruption budget), Prometheus metrics scraped through a ServiceMonitor, a Traefik Ingress with a cert-manager certificate, a Route53 record. No image is built: the pod is stock Debian running the release `login` binary from a read-only host path on the node, the way the deployed game server runs today. The game server (compose, same machine) reaches the service over its public URL for keys; the service reaches the game server's login link over UDP with a shared service token.

**Tech Stack:** Rust (`crates/login`: axum 0.8, the `prometheus` crate 0.14 without its default features); OpenTofu ≥ 1.10; providers `hashicorp/kubernetes ~> 3.2`, `hashicorp/helm ~> 3.3`, `hashicorp/aws ~> 6.0`, `hashicorp/random ~> 3.7`, `hashicorp/tls ~> 4.1`; images `debian:trixie-slim`, `postgres:17-alpine`; the release `login` binary of this repo; `just`, `rsync`, `kubectl`.

**Spec:** none written. The decisions below are the spec; 1–3 were confirmed by the user on 2026-11-14. Context: `docs/superpowers/plans/2026-11-12-keycloak-2-game-login.md` ("As built") — what the service does and its settings.

## Verified against the code on 2026-11-14 (`bc385535`) — read before starting

- **Nothing deploys the login service today.** `deploy/` has no module, image or compose service for it. The deployed game (`deploy/web/compose.yml`, `just deploy`, on `root@5.9.7.138`, the same machine as the k3s node) runs with the default local auth, and the web bundle is built without `VOXEL_LOGIN_URL`.
- **The repo builds no container images and has no registry.** The game server is a stock Debian container with the binary bind-mounted from `/srv/mmo/bin`; the website is stock nginx with a ConfigMap.
- **Login service settings** (`crates/login/src/config.rs`): `VOXEL_LOGIN_BIND` (default `127.0.0.1:8790`), `VOXEL_LOGIN_DB` (SQLite path or `postgres://` URL; it migrates at start), `VOXEL_LOGIN_KEYS_DIR` (EdDSA keys; `login keygen` makes one), `VOXEL_LOGIN_CORS_ORIGINS` (comma list), `VOXEL_OIDC_ISSUER` / `_AUDIENCE` (`mmo-login`) / `_CLIENT_ID` (`game`), `VOXEL_SERVICE_TOKEN[_FILE]` (to dial a host's login link). With an issuer set, start-up fetches Keycloak's discovery document and fails if it cannot.
- **Routes** (`crates/login/src/api.rs`): `/v1/login/oidc`, `/v1/config`, `/v1/refresh`, `/v1/logout`, `/v1/worlds`, `/v1/join`, `/.well-known/jwks.json`, `/healthz`.
- **TLS roots:** the binary carries `webpki-roots` (no CA package needed in the container to reach Keycloak over HTTPS).
- **Operator commands** (`login world add|list|remove|open|close`, `login account issue-refresh|revoke`) open the database themselves: in the cluster they run inside the pod, where the environment names it.
- **Game server side** (`crates/server/src/login_link.rs`, `auth_edge.rs`): `VOXEL_AUTH=jwt` needs `VOXEL_LOGIN_URL` (it fetches `/.well-known/jwks.json`), `VOXEL_WORLD_ID`, and a service token (`VOXEL_SERVICE_TOKEN_FILE`) — without one it refuses to start (`VOXEL_JWT_ADMISSION=newest` is dev only). With a token it listens for the login service on `VOXEL_HOST_LINK_BIND` (default `127.0.0.1:6490`, UDP/QUIC) and writes its certificate pin to `VOXEL_HOST_LINK_CERT_HASH_FILE`; the world's registry row must carry `--link <ip:port> --link-cert <sha256>`.
- **Keycloak is live** at `https://auth.factoriosurvivalworld.com/realms/players`; its `game` client's web origins default to `https://orion.factoriosurvivalworld.com`, where the game page is served.
- **Two replicas are safe as the code stands** (checked 2026-11-14): refresh rotation is one conditional `UPDATE … WHERE revoked_at IS NULL … RETURNING` (exactly one of two concurrent rotations wins); session epochs are bumped by an upsert; migrations run under sqlx's Postgres advisory lock; the game server keeps a SET of login peers (`crates/server/src/login_link.rs`, `LoginPeers.peers`), acks a spawn order to the peer that sent it and tells every login peer when a session ends; a kick sent by two replicas is applied once ("a resend is harmless", `crates/login/src/host_link.rs`). What does NOT survive two replicas is per-pod key generation: keys are read once at start (`auth::Signer::load_dir`), so every replica must start with the same key set — hence Decision 4.
- **Signing key format:** PKCS#8 PEM, Ed25519 (`ed25519_dalek::SigningKey::from_pkcs8_pem`); every `*.pem` in the directory is published, the last by file name signs.
- **No Rust service here exposes metrics yet** (no `prometheus` / `metrics` dependency in the workspace). The cluster's Prometheus picks up every ServiceMonitor in every namespace (`serviceMonitorSelectorNilUsesHelmValues: false`); Keycloak's is a tiny local chart (`deploy/modules/keycloak/charts/keycloak-monitor`).
- **Orion root conventions** (`deploy/envs/orion/keycloak/`): `backend.tf` (partial S3 backend, key `orion/<root>.tfstate`), `versions.tf`, `providers.tf` (`kubeconfig`, `kube_context` = `orion`, AWS for Route53), `main.tf`; recipes `just orion-init|orion-plan|orion-apply <root>`, `just orion-kube …`. `local-path` volumes cannot grow and have one copy, no off-box backup.

## Decisions settled in planning

1. **Public address:** `https://login.factoriosurvivalworld.com`. (confirmed by the user, 2026-11-14)
2. **(Superseded on 2026-11-14 by Task 5: the image comes from AWS ECR.)** Originally: no image, the binary comes from the node. Three options. A registry (ghcr or one on the box): the standard answer, but a new moving part and credentials for one small binary. Building an image locally and importing it into k3s's containerd over ssh: no registry, but an image pipeline nothing else here has. **Chosen:** `debian:trixie-slim` with `/srv/mmo/bin` mounted read-only from the host, binary shipped by rsync — exactly how the game server is deployed now, on the same machine. Cost: it only works while the cluster is this one node (R5's StatefulSet work needs a real image; this plan's module takes `image` and `command` as variables so that switch is two values).
3. **Its own PostgreSQL** (confirmed by the user, 2026-11-14: all the cluster's PostgreSQL instances will be consolidated later, so keep this one plain and easy to move — one database, one role, no extensions), a StatefulSet in the same module like Keycloak's. SQLite on a volume is simpler, but `world_players` (which account owns which character) is the one table here that cannot be rebuilt, and there is no SQLite → Postgres migration tool; starting on Postgres avoids that move later and gives `pg_dump`.
4. **The signing key is a Secret made by OpenTofu** (`tls_private_key`, ED25519, mounted as `0001.pem`), not a volume filled by each pod: with two replicas every pod must publish and trust the same keys from its first request, and a Secret also works on more than one node. Cost: the private key is in the state (as the database password and service token are). Rotation, when wanted: add a second `tls_private_key` as `0002.pem` and apply (every pod then publishes both keys and signs with the new one); remove `0001.pem` in a later apply, never in the same one — during a rolling restart a ticket signed by a new pod may be checked against keys an old pod served.
5. **Only the API is public:** the Ingress routes `/v1/` and `/.well-known/jwks.json`. `/healthz` is for the probes only.
6. **CORS:** `VOXEL_LOGIN_CORS_ORIGINS` = the game page's origin(s), default `https://orion.factoriosurvivalworld.com`.
7. **One service token**, generated here (`random_password`), mounted into the pod as a file, and copied to the game box by a recipe that never prints it.
8. **Rollout is two-step.** Tasks 0–3 put the service up and prove it; players notice nothing. Task 4 switches the deployed game to it and is gated on the user: it re-keys players (`local:<name>` → account), so characters on the deployed world start fresh.
9. **Two replicas** (`replicas` variable, default 2), `RollingUpdate` with `max_unavailable = 0`, `max_surge = 1`, and a PodDisruptionBudget of `min_available = 1`. What this buys on orion: a deploy or a crashed pod no longer interrupts logins. What it does not: the node and the single PostgreSQL remain single points — say so in the module's header.
10. **Metrics:** the service gets `GET /metrics` (Prometheus text format) on its HTTP port; the Ingress does not route it; a ServiceMonitor scrapes every pod every 30 s. Three options for the library: `metrics` + `metrics-exporter-prometheus` (a global recorder, pulls its own HTTP stack unless trimmed); OpenTelemetry (far more than a handful of counters needs); **chosen:** the `prometheus` crate with `default-features = false` (no protobuf), an explicit registry held in `AppState` — nothing global, so tests read their own registry. Labels are from fixed sets only: never an email, account id, token or raw path.
11. **Not in this plan:** backups of the login database (nothing on orion has off-box backup yet), a Grafana dashboard and alert rules for the new metrics (follow-up in `deploy/modules/monitoring/` once real numbers exist), a highly available PostgreSQL, rate limiting (deferred since R2a), moving the game server itself into k3s (R5).

## Global Constraints

- Work on `develop` in the main checkout. Never create a git worktree. The tree holds the user's uncommitted work and another session commits to it: `git add` only a task's own files; for `justfile` / `CLAUDE.md` build the commit from the HEAD copy and verify the committed file (`git show HEAD:<file>`, `git diff HEAD -- <file>` shows only foreign hunks).
- OpenTofu, not Terraform. Every cluster command goes through the `just orion-*` recipes, which pin the `orion` kube context.
- **Plan before every apply, and the user runs every `just orion-apply …`, every `just orion-login-push`, and everything in Task 4.** An agent stops at each such step, shows the plan summary or the command, and waits.
- Never print or commit secrets. `deploy/**/*.tfvars` is git-ignored; secrets live in state and Kubernetes Secrets. The service token and the database password never appear in a log, a report or a command line.
- Follow `deploy/modules/keycloak` and `deploy/modules/picoshare`: `versions.tf`, `variables.tf` with a description on every non-obvious variable, `main.tf`, `outputs.tf`; pods set `automount_service_account_token = false` and `enable_service_links = false`; PostgreSQL is one replica; the login Deployment follows Decision 9.
- `local-path` volumes cannot grow: sizes are final. IAM tag values may only contain letters, digits, spaces and `_ . : / = + - @`.
- Keep the `justfile` and the Deployment paragraph of `CLAUDE.md` in sync in the task that adds a root or a recipe. Run `tofu fmt -recursive` on touched directories before each commit.
- `login-api` stays serde-only. The workspace's only rustls crypto provider is ring (no dependency or feature that pulls `aws-lc-rs`). Tests: `just test-one login "<args>"` for the touched tests, the whole login `it` binary once after a wide change, `just smoke`; tag tests slower than 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`.
- Metric names and labels, exact: `login_http_requests_total{route,status}`, `login_http_request_duration_seconds{route}` (histogram, buckets 0.005, 0.025, 0.1, 0.5, 2, 10), `login_oidc_logins_total{outcome}` (`created|existing|adopted|email_taken|refused|provider_unavailable`), `login_refresh_total{outcome}` (`ok|refused`), `login_joins_total{outcome}` (`ok|refused|world_unavailable`), `login_host_link_up{world}` (0 or 1), `login_pending_kicks`, `login_jwks_refetch_total{outcome}` (`ok|failed`), `login_build_info{version}` (always 1). `route` is the matched route template (`/v1/join`), or `other` for anything unmatched; `status` is the numeric code.
- Exact values: namespace `login`; replicas `2`; host `login.factoriosurvivalworld.com`; issuer `https://auth.factoriosurvivalworld.com/realms/players`; container port `8790`; binary path in the pod `/srv/mmo/bin/login`; node IP `5.9.7.138`; world id `orion`; host-link port `6490/udp`.

## Review Focus

1. **Keycloak is unreachable when the pod starts** (both sit on one box; a reboot starts them together). Expected: the pod fails its start, Kubernetes restarts it, and it comes up once Keycloak answers — no manual step. Pinned by Task 2 Step 6.
2. **The binary is missing or replaced while the pod runs** (first apply before the first push; a push mid-flight). Expected: a missing binary is a clear `CrashLoopBackOff` with the path in the event, never a half-started service; a push replaces the file atomically and restarts the pod. Pinned by Task 1 Step 4 and Task 3 Step 3.
3. **The pod cannot reach `https://auth.factoriosurvivalworld.com`** (a pod calling the node's own public IP — the Keycloak 1 plan hit this class for Zulip). Expected: discovery works from inside the pod. Pinned by Task 2 Step 5, with the fallback named there.
4. **The database password changes in state but not in the volume** (`random_password` regenerated). Expected: documented as in `keycloak/postgres.tf` ("changing this does NOT change the password inside an existing volume"), and the pod's failure names authentication, not a hang.
5. **`/healthz`, `/metrics` or the database is reachable from the internet.** Expected: `https://login.factoriosurvivalworld.com/healthz` and `/metrics` answer 404; Postgres has no Ingress and a headless Service. Pinned by Task 2 Step 7.
6. **A request for an arbitrary path** (a scanner walking `/wp-admin/…`) **mints a new metric series each time.** Expected: every unmatched path counts under `route="other"`; the series count stays bounded. Pinned by Task 0's `unmatched_paths_share_one_route_label`.
7. **Two pods start at the same moment on an empty database** (the first apply; a node reboot). Expected: both become Ready, the schema is migrated once, and both publish the same key id. Pinned by Task 2 Step 6.

## File structure

```
crates/login/src/metrics.rs           new: the registry, the HTTP layer, GET /metrics
crates/login/src/{api,lib,host_link,oidc}.rs   count logins, refreshes, joins, link state, key refetches
crates/login/tests/metrics.rs         new (module of the `it` binary)
Cargo.toml, crates/login/Cargo.toml   + prometheus (default-features = false)
deploy/modules/login/versions.tf      providers
deploy/modules/login/variables.tf     every dial, described
deploy/modules/login/postgres.tf      the service's own PostgreSQL (StatefulSet, headless Service, Secret)
deploy/modules/login/main.tf          namespace, secrets (database, service token, signing key), Deployment, PodDisruptionBudget, Service, Ingress, DNS
deploy/modules/login/charts/login-monitor/   Chart.yaml + templates/servicemonitor.yaml (as keycloak-monitor)
deploy/modules/login/outputs.tf       url, namespace, service_token (sensitive)
deploy/envs/orion/login/{backend,versions,providers,main}.tf   the root (state key orion/login.tfstate)
scripts/orion_login_push.sh           build artefact → node, restart the pod
justfile                              orion-login-build / -push / orion-login / -token-push / -logs
deploy/web/compose.yml, scripts/web_release.sh   Task 4 only
CLAUDE.md                             Deployment paragraph
```

---

### Task 0: Metrics in the login service

**Files:**
- Create: `crates/login/src/metrics.rs`, `crates/login/tests/metrics.rs`
- Modify: `Cargo.toml` (`[workspace.dependencies]`: `prometheus = { version = "0.14", default-features = false }`), `crates/login/Cargo.toml`, `crates/login/src/lib.rs` (`pub mod metrics;`, build info at start), `crates/login/src/api.rs` (`AppState.metrics`, the layer, the route, the outcome counters), `crates/login/src/host_link.rs` (link gauge, pending kicks), `crates/login/src/oidc.rs` (refetch counter), `crates/login/tests/it.rs` (`mod metrics;`), `crates/login/tests/common` (the test app exposes its `Metrics`)

**Interfaces:**
- Produces:
  - `pub struct Metrics` (cheap to clone: an `Arc` inside) with `pub fn new() -> Metrics` (its own `prometheus::Registry`, every metric of Global Constraints registered, `login_build_info{version = env!("CARGO_PKG_VERSION")}` set to 1) and `pub fn render(&self) -> String` (text exposition format).
  - Methods the rest of the crate calls, one per event, so no caller builds a label by hand: `oidc_login(OidcOutcome)`, `refresh(ok: bool)`, `join(JoinOutcome)`, `host_link(world: &str, up: bool)`, `pending_kicks(n: u64)`, `jwks_refetch(ok: bool)`; `pub enum OidcOutcome { Created, Existing, Adopted, EmailTaken, Refused, ProviderUnavailable }`, `pub enum JoinOutcome { Ok, Refused, WorldUnavailable }`.
  - `AppState` gains `pub metrics: Metrics` (set in its constructor; `HostLinks` and `OidcVerifier` get a clone where they count).
  - The HTTP layer: `axum::middleware::from_fn_with_state` around the whole router, reading `axum::extract::MatchedPath` from the request extensions (`other` when absent), timing the handler, then counting `login_http_requests_total{route,status}` and observing the histogram. `/metrics` and `/healthz` are themselves NOT counted (a scrape every 30 s and probes every 10 s would drown the rest).
  - Route `GET /metrics` → `200`, `content-type: text/plain; version=0.0.4`, body `metrics.render()`.
- `login_pending_kicks` is set by the kick sweeper each pass (`HostLinks`, where `pending_kicks()` is already read); `login_host_link_up` by the link pump on `Up` / `Down`.

- [ ] **Step 1: Write the failing tests** in `crates/login/tests/metrics.rs` (through the test app, reading `GET /metrics`):
  - `metrics_endpoint_serves_text_format`: status 200, the content type above, the body contains `login_build_info{version="` and a `# TYPE login_http_requests_total counter` line.
  - `an_oidc_login_is_counted_by_outcome`: one good OIDC login then one with a foreign audience → the body contains `login_oidc_logins_total{outcome="created"} 1` and `login_oidc_logins_total{outcome="refused"} 1`.
  - `requests_are_counted_by_route_template_and_status`: `GET /v1/worlds` twice → `login_http_requests_total{route="/v1/worlds",status="200"} 2`; a `login_http_request_duration_seconds_count{route="/v1/worlds"} 2` line exists.
  - `unmatched_paths_share_one_route_label` (Review Focus 6): `GET /a`, `GET /b/c`, `GET /wp-admin/x.php` → exactly one series with `route="other"` and value 3; none of the three paths appears in the body.
  - `scrapes_and_probes_are_not_counted`: `GET /metrics` three times and `GET /healthz` once → no `route="/metrics"` and no `route="/healthz"` series.
  - `metrics_never_carry_identities`: after an OIDC login (email `ann@x.io`, subject `u-1`) and a refresh, the body contains none of `ann`, `x.io`, `u-1`, nor the refresh token returned by the login.
  - `a_refresh_and_a_join_are_counted`: a good refresh, a refresh with garbage, a join to a closed world → `login_refresh_total{outcome="ok"} 1`, `…{outcome="refused"} 1`, `login_joins_total{outcome="refused"} 1` (use whichever outcome the existing closed-world join test asserts, and say so).
- [ ] **Step 2: Run** `just test-one login "--test it metrics::"` — expected: compile failure (`metrics` module missing).
- [ ] **Step 3: Implement.** Counters are incremented where the outcome is decided (the `match` on `OidcUpsert` / `OidcError` in `login_oidc`; the result of `rotate_refresh`; the end of `join`), never by parsing a response.
- [ ] **Step 4: Run** `just test-one login "--test it metrics::"`, then the whole binary once (`just test-one login "--test it -- --include-ignored"`), `just smoke`, and `nice -n 10 cargo tree -p login -i aws-lc-rs` (must not find the package) and `nice -n 10 cargo tree -p login | grep -c protobuf` (prints `0`).
- [ ] **Step 5: Commit** `feat(login): Prometheus metrics at /metrics (requests, logins, refreshes, joins, host links)`.

### Task 1: The `login` module

**Files:**
- Create: `deploy/modules/login/versions.tf`, `variables.tf`, `postgres.tf`, `main.tf`, `outputs.tf`

**Interfaces:**
- Produces variables (with defaults): `namespace` (`login`), `host` (`login.factoriosurvivalworld.com`), `image` (`debian:trixie-slim`), `command` (`["/srv/mmo/bin/login"]`), `bin_host_path` (`/srv/mmo/bin`), `issuer` (no default — the root passes it), `oidc_audience` (`mmo-login`), `oidc_client_id` (`game`), `cors_origins` (`["https://orion.factoriosurvivalworld.com"]`), `postgres_image` (`postgres:17-alpine`), `storage_class` (`local-path`), `postgres_volume_size` (`2Gi`), `replicas` (`2`), `metrics` (`true`), `memory_limits` (`{ login = "128Mi", postgres = "256Mi" }`), `cluster_issuer` (`letsencrypt`), `ingress_class` (`traefik`), `manage_dns_record` (`true`), `dns_zone` (`factoriosurvivalworld.com`), `node_ip` (`5.9.7.138`).
- Produces outputs: `url` (`https://<host>`), `namespace`, `service_token` (sensitive).
- Resources, by name: namespace; `random_password.db` + Secret `login-db` (key `password`); `random_password.service_token` (48 chars, no specials) + Secret `login-service` (key `service-token`); `tls_private_key.signing` (`algorithm = "ED25519"`) + Secret `login-keys` (key `0001.pem` = its `private_key_pem`); headless Service + StatefulSet `login-postgres` (database and user `login`, `PGDATA` in a subdirectory, `pg_isready` readiness, a `data` volume claim template — copy `deploy/modules/keycloak/postgres.tf` and rename; add its comment that the password in an existing volume does not follow the state); Deployment `login` (`replicas = var.replicas`, `RollingUpdate` with `max_unavailable = 0`, `max_surge = 1`); `kubernetes_pod_disruption_budget_v1` `login` (`min_available = 1`, only when `var.replicas > 1`); Service `login` (port 80 → `http`, labelled so the ServiceMonitor selects it); `helm_release.monitor` (`count = var.metrics ? 1 : 0`, chart `${path.module}/charts/login-monitor`, values `selectorLabels`) — copy `deploy/modules/keycloak/charts/keycloak-monitor` and its `helm_release`, with endpoint `port: http`, `path: /metrics`, `interval: 30s`; Ingress `login`; the Route53 A record under `manage_dns_record`.
- The Deployment's pod:
  - volumes: `bin` = `host_path { path = var.bin_host_path, type = "Directory" }`, `keys` = the `login-keys` Secret (mode `0400`), `service` = the `login-service` Secret (mode `0400`). No init container and no key volume (Decision 4).
  - container `login`: `command = concat(var.command, ["serve"])`; env `VOXEL_LOGIN_BIND=0.0.0.0:8790`, `VOXEL_LOGIN_KEYS_DIR=/keys`, `VOXEL_LOGIN_CORS_ORIGINS=join(",", var.cors_origins)`, `VOXEL_OIDC_ISSUER`, `VOXEL_OIDC_AUDIENCE`, `VOXEL_OIDC_CLIENT_ID`, `VOXEL_SERVICE_TOKEN_FILE=/run/login/service-token`, `PGPASSWORD` from the `login-db` Secret, and `VOXEL_LOGIN_DB=postgres://login:$(PGPASSWORD)@login-postgres:5432/login` (Kubernetes expands `$(PGPASSWORD)` when that variable is declared first; the password has no special characters, so no URL-encoding is needed); `bin` mounted read-only at `/srv/mmo/bin`; port `http` 8790; readiness `http_get /healthz` every 10 s; liveness `tcp_socket` every 30 s after 10 s; requests `cpu = "10m", memory = "32Mi"`, limit `var.memory_limits["login"]`.
  - annotations `checksum/secrets` = sha256 over the database password, the service token and the signing key PEM, so a changed secret rolls the pods.
  - `termination_grace_period_seconds = 30`; with `max_unavailable = 0` a new pod must be Ready (its `/healthz` answering, which needs the database and Keycloak's discovery) before an old one is stopped.
- The Ingress: host `var.host`, TLS secret `login-tls`, the `cert-manager.io/cluster-issuer` annotation, exactly two `Prefix` paths → Service `login` port `http`: `/v1/` and `/.well-known/jwks.json`; `wait_for_load_balancer = false`; `depends_on` the DNS record (as Keycloak's).

- [ ] **Step 1: Write the five files and the `login-monitor` chart** from the Interfaces block (`versions.tf` requires `kubernetes`, `helm`, `aws`, `random` and `tls`). Header comment of `main.tf`: what the service is, that the binary comes from the node (`just orion-login-push`), what two replicas do and do not protect against (Decision 9), and that the first apply before the first push crash-loops by design.
- [ ] **Step 2: Format and validate without a backend:**
  Run: `tofu -chdir=deploy/modules/login fmt -check` and, in a scratch root under the job's temp directory holding `module "login" { source = "<repo>/deploy/modules/login"  issuer = "https://example.invalid/realms/x" }` plus the three providers with dummy config, `tofu init -backend=false && tofu validate`.
  Expected: both succeed ("The configuration is valid").
- [ ] **Step 3: Check the two things `validate` cannot,** on the source (`tofu plan` needs the cluster): `grep -c '"/healthz"' deploy/modules/login/main.tf` prints `1` (the readiness probe only, never an Ingress path); the Ingress block names exactly `/v1/` and `/.well-known/jwks.json`.
- [ ] **Step 4: Missing-binary behaviour is by construction:** confirm the `bin` volume uses `type = "Directory"` (the pod waits with a clear event if `/srv/mmo/bin` is absent) and that the container's `command` is the binary path itself, not a shell (a missing binary is a `StartError` naming that path). Review Focus 2.
- [ ] **Step 4b: Prove the key format before anything is applied.** OpenTofu's ED25519 `private_key_pem` must be what `auth::SigningKey::from_pkcs8_pem` reads. In the scratch root of Step 2 add `resource "tls_private_key" "k" { algorithm = "ED25519" }` with `output "pem" { value = tls_private_key.k.private_key_pem, sensitive = true }`, `tofu apply -auto-approve` there (local state, no cluster, nothing but that resource), write `tofu output -raw pem` to `<tmp>/keys/0001.pem` (mode 600) and run `VOXEL_LOGIN_KEYS_DIR=<tmp>/keys VOXEL_LOGIN_DB=sqlite::memory: VOXEL_LOGIN_BIND=127.0.0.1:0 timeout 3 ./target/debug/login serve`.
  Expected: the log line `[login] listening on … (signing kid …, 1 key(s) published)` before the timeout ends it. If the key is refused: stop and report — the fallback is a one-off Kubernetes Job that runs `login keygen` into a Secret, which changes Decision 4. Delete the scratch key afterwards; never print the PEM.
- [ ] **Step 5: Commit** `feat(deploy): login module — the game's login service with its own PostgreSQL`.

### Task 2: The orion root, apply, and exposure checks

**Files:**
- Create: `deploy/envs/orion/login/backend.tf`, `versions.tf`, `providers.tf`, `main.tf`
- Modify: `justfile` (the orion comment block's root list, the `orion-init` comment, the apply-order comment: `keycloak` → `keycloak-realm` → `login`), `CLAUDE.md` (Deployment paragraph: `login` = https://login.factoriosurvivalworld.com, the game's login service)

**Interfaces:**
- Consumes: Task 1's module and outputs.
- Produces: root `login`, state key `orion/login.tfstate`. `main.tf` reads the issuer from the `keycloak-realm` root's remote state (`data "terraform_remote_state"`, the way `deploy/envs/orion/zulip/main.tf` reads it) and passes `issuer = …outputs.issuer`; it re-exports `url` and `service_token` (sensitive). The remote state needs the root variables `state_bucket` and `aws_region` exactly as the zulip root declares them (copy their declarations and defaults). Providers: copy `deploy/envs/orion/keycloak/providers.tf` (kubernetes, helm, aws) and add `tls` to `versions.tf`; stack tag `orion-login`. Needs `../monitoring` applied for the ServiceMonitor kind (as the keycloak root does). Header comment: needs `../platform`, `../keycloak` and `../keycloak-realm` applied first, and `just orion-login-push` (Task 3) before the pod can start.

- [ ] **Step 1: Write the root** and the justfile / `CLAUDE.md` lines.
- [ ] **Step 2: Init and plan.**
  Run: `just orion-init login && just orion-plan login`
  Expected: the plan adds only resources in namespace `login` plus one Route53 record; nothing is changed or destroyed; no secret value is printed (they show as `(sensitive value)`).
- [ ] **Step 3: Ship the binary first** — Task 3 Steps 1–4 (the push recipe) come before the apply, so the pod has something to run. Do Task 3 up to its Step 4, then return here.
- [ ] **Step 4: Stop. The user runs `just orion-apply login`.** Then: `just orion-kube -n login rollout status statefulset/login-postgres --timeout=5m` and `just orion-kube -n login rollout status deploy/login --timeout=5m`; `just orion-kube -n login get pods -l app.kubernetes.io/name=login` shows two pods Ready.
- [ ] **Step 5: Verify the pod reached Keycloak and the database** (Review Focus 3).
  Run: `just orion-kube -n login logs deploy/login --tail 50`
  Expected: `[login] listening on http://0.0.0.0:8790 …` and `[login] identity-provider login on: issuer https://auth.factoriosurvivalworld.com/realms/players, audience mmo-login`; no line `[login] no service token`; no database error. If discovery fails with a connection error to `auth.factoriosurvivalworld.com`: the fallback is a `host_aliases` entry on the pod mapping that name to the Traefik Service's cluster IP (add variable `issuer_host_alias_ip`, default null) — the same hairpin fix the Keycloak 1 plan names for Zulip; record which was needed.
- [ ] **Step 6: Verify the restart path and that both pods agree** (Review Focus 1 and 7).
  Run: `just orion-kube -n login delete pod -l app.kubernetes.io/name=login` (both at once — the cold start), wait for the rollout; then `for i in 1 2 3 4 5 6; do curl -s https://login.factoriosurvivalworld.com/.well-known/jwks.json | sha256sum; done`.
  Expected: two pods Ready again with no manual step; all six sums are equal (both pods publish the same single key); `just orion-kube -n login logs -l app.kubernetes.io/name=login --prefix --tail 20` shows the same `signing kid` in both pods and no migration error.
- [ ] **Step 6b: A rolling restart drops no request.**
  Run in one terminal: `while true; do curl -s -o /dev/null -w "%{http_code}\n" https://login.factoriosurvivalworld.com/v1/config; sleep 0.2; done | uniq -c`; in another: `just orion-kube -n login rollout restart deploy/login` and wait for it.
  Expected: only `200` lines for the whole restart.
- [ ] **Step 7: Verify exposure** (Review Focus 5).
  Run: `for p in /healthz /metrics / /v1/config /.well-known/jwks.json /v1/worlds; do curl -s -o /dev/null -w "%{http_code} $p\n" https://login.factoriosurvivalworld.com$p; done`
  Expected: `404 /healthz`, `404 /metrics`, `404 /`, `200 /v1/config`, `200 /.well-known/jwks.json`, `200 /v1/worlds`. And `curl -s https://login.factoriosurvivalworld.com/v1/config` names the issuer `https://auth.factoriosurvivalworld.com/realms/players` and client `game`.
- [ ] **Step 8: Verify CORS** from the game page's origin.
  Run: `curl -s -o /dev/null -D - -H 'Origin: https://orion.factoriosurvivalworld.com' https://login.factoriosurvivalworld.com/v1/config | grep -i access-control-allow-origin`
  Expected: the header echoes that origin; with `Origin: https://example.com` there is no such header.
- [ ] **Step 8b: Prometheus scrapes both pods.** Through Grafana's API (read-only; token from `.env`, loaded inside the one command, never echoed): query the Prometheus data source for `count(login_build_info)` and `sum(up{namespace="login"})`.
  Expected: both `2` within two scrape intervals of the rollout. If the target is missing: check the ServiceMonitor's selector labels against the Service's labels (`just orion-kube -n login get servicemonitor,svc --show-labels`).
- [ ] **Step 9: Commit** `feat(deploy): the login service on orion (https://login.factoriosurvivalworld.com)`.

### Task 3: Shipping the binary and operating the service

**Files:**
- Create: `scripts/orion_login_push.sh`
- Modify: `justfile` (new recipes), `CLAUDE.md` (the same Deployment paragraph)

**Interfaces:**
- Produces recipes:
  - `orion-login-build`: `{{cap}} {{cargo}} build --release -p login --bin login`.
  - `orion-login-push: orion-login-build`: `DEPLOY_HOST={{deploy_host}} KUBE_CONTEXT={{orion_context}} scripts/orion_login_push.sh`.
  - `orion-login *args`: `kubectl --context {{orion_context}} -n login exec deploy/login -- /srv/mmo/bin/login "$@"` — the operator commands, run where the database is (`just orion-login world list`).
  - `orion-login-logs`: `kubectl --context {{orion_context}} -n login logs -f deploy/login --tail 200`.
  - `orion-login-token-push`: `{{tofu}} -chdir=deploy/envs/orion/login output -raw service_token | ssh -o BatchMode=yes {{deploy_host}} 'umask 077; mkdir -p /srv/mmo/secrets; cat > /srv/mmo/secrets/service-token.new; mv -f /srv/mmo/secrets/service-token.new /srv/mmo/secrets/service-token'` (the token goes through a pipe, never an argument or the terminal).
- `scripts/orion_login_push.sh` (env `DEPLOY_HOST` default `root@5.9.7.138`, `DEPLOY_DIR` default `/srv/mmo`, `LOGIN_BIN` default `target/release/login`, `KUBE_CONTEXT` default `orion`), following `scripts/deploy_push.sh`: refuse if the binary is missing; `ssh mkdir -p $dir/bin`; rsync to `$dir/bin/login.new`; `ssh mv -f $dir/bin/login.new $dir/bin/login` (atomic); then, only if `kubectl --context … -n login get deploy/login` succeeds, `rollout restart deploy/login` and `rollout status --timeout=5m` (a rolling restart: with two replicas logins continue through it); if the Deployment does not exist yet, say "binary shipped; apply the login root" and exit 0.

- [ ] **Step 1: Write the script and the recipes.**
- [ ] **Step 2: Static checks.**
  Run: `bash -n scripts/orion_login_push.sh` and, on the committed justfile copy, `just --dry-run orion-login-push`, `just --dry-run orion-login world list`, `just --dry-run orion-login-token-push`.
  Expected: all render; the token recipe's rendering contains no token (only the pipeline).
- [ ] **Step 3: Check the binary runs on the container's base** before shipping (Review Focus 2).
  Run: `just orion-login-build && docker run --rm -v "$PWD/target/release:/b:ro" debian:trixie-slim /b/login --db sqlite::memory: world list`
  Expected: exit 0 with an empty list (no missing shared library or glibc-version error). A failure here means the build host is newer than trixie: stop and report — the fix (build in a `rust:trixie` container) changes Decision 2's cost.
- [ ] **Step 4: Stop. The user runs `just orion-login-push`** (first time: "binary shipped; apply the login root"). Return to Task 2 Step 4.
- [ ] **Step 5 (after Task 2's apply): Register the world, closed for now.**
  The user runs: `just orion-login world add orion --name "Orion" --quic 5.9.7.138:6466 --wt https://orion.factoriosurvivalworld.com:6467/ --closed`
  then `just orion-login world list`.
  Expected: one row `orion`, status closed, no link. (Closed: the game server does not take tickets yet, so nobody should be offered a Join.)
- [ ] **Step 6: Sign in for real, without the game server.** On the developer machine: `VOXEL_LOGIN_URL=https://login.factoriosurvivalworld.com just play` → Play online → Sign in → the browser opens the production Keycloak → sign in with a real account.
  Expected: the world list shows "Orion" (closed) and "Signed in as <username>". `just orion-login-logs` shows the login and no token. **Ask the user first — this launches the client.**
- [ ] **Step 7: A second push restarts cleanly.** The user runs `just orion-login-push` again.
  Expected: the rollout finishes with no failed request (repeat Task 2 Step 6b's loop during it); the client from Step 6, restarted, goes straight to the world list (the refresh token lives in PostgreSQL, and either pod accepts it).
- [ ] **Step 8: Commit** `feat(deploy): ship and operate the login service on orion (orion-login-push, orion-login)`.

### Task 4: The deployed game uses it — gated on the user

**Before this task (found by the final review of Tasks 0–3, 2026-11-14):** host links are dialed lazily (`HostLinks::link` is called only from a join or the kick sweeper), so a pod has no `login_host_link_up` series until it serves a join or has a pending kick, and a `SessionEnded` reaches no pod after a push until someone joins — make the service dial at start for every world that has a link, then Step 3's `sum(login_host_link_up{world="orion"}) = 2` holds. Read `login_pending_kicks` with `max`, never `sum` (every pod reports the same database count). Also fix then: the gauge write order in `link()`, a test for "current link goes Down → 0".

**Deferred by the user on 2026-11-14: the deployed game is switched only after R4 / R5 are done. This run stops after Task 3.** **Do not start this task without the user's explicit go-ahead.** It changes what players of the deployed world experience: the server stops taking names and takes login tickets, and a player's character is keyed by their account from then on, so existing characters on the deployed world do not carry over.

**Files:**
- Modify: `deploy/web/compose.yml` (the `server` service), `scripts/web_release.sh` (pass `VOXEL_LOGIN_URL` through to the client build), `justfile` (`web-release`: new variable `web_login_url := "https://login.factoriosurvivalworld.com"` exported as `VOXEL_LOGIN_URL`; `deploy` comment), `CLAUDE.md` (Deployment paragraph)

**Interfaces:**
- Consumes: Task 3's `orion-login-token-push` and `orion-login`.
- `deploy/web/compose.yml`, service `server`, added environment: `VOXEL_AUTH: jwt`, `VOXEL_WORLD_ID: orion`, `VOXEL_LOGIN_URL: https://login.factoriosurvivalworld.com`, `VOXEL_SERVICE_TOKEN_FILE: /srv/mmo/secrets/service-token`, `VOXEL_HOST_LINK_BIND: 0.0.0.0:6490`, `VOXEL_HOST_LINK_CERT_HASH_FILE: /srv/mmo/secrets/host-link.cert-sha256`; added port `"6490:6490/udp"`; added volume `/srv/mmo/secrets:/srv/mmo/secrets` (read-write: the server writes its link certificate, key and pin beside the hash file). The header comment gains the login link and says the host firewall keeps 6490 closed to the internet (the firewall lives in `~/Projects/mod/mods/survival-world`).
- The web bundle is built with `VOXEL_LOGIN_URL` set, so the page offers Play online (`option_env!("VOXEL_LOGIN_URL")` in `crates/client/src/login/api.rs`; `?login=` is ignored off localhost).

- [ ] **Step 1: Write the compose, script and justfile changes.**
  Run: `docker compose -f deploy/web/compose.yml config -q` (syntax) and `just --dry-run web-release | grep -c VOXEL_LOGIN_URL`.
  Expected: no error; `1`.
- [ ] **Step 2: Stop. The user runs `just orion-login-token-push`,** then `just deploy` (builds the bundle with the login URL and the release server, ships, restarts).
  Expected in `just deploy-logs`: the server reports `auth: jwt` with the login link listening on `0.0.0.0:6490`, and that it fetched the login service's keys. If the key fetch fails with a connection error to `login.factoriosurvivalworld.com` from inside the container (the node calling its own public address): the fallback is `extra_hosts` on the `server` service mapping that name to the host gateway; record which was needed.
- [ ] **Step 3: Register the link and open the world.** The user runs:
  `just orion-login world add orion --name "Orion" --quic 5.9.7.138:6466 --wt https://orion.factoriosurvivalworld.com:6467/ --link 5.9.7.138:6490 --link-cert "$(ssh root@5.9.7.138 cat /srv/mmo/secrets/host-link.cert-sha256)"`
  Expected: `just orion-login world list` shows `orion` open with `link=5.9.7.138:6490`, and within a few seconds the link is up from BOTH pods: the Grafana query `sum(login_host_link_up{world="orion"})` returns `2` (each replica dials the game server itself; the server keeps a set of login peers). If the link never comes up (a pod reaching the node's public IP over UDP is the unproven part): the fallback is to register `--link <the node's address on the cluster network>:6490` (`just orion-kube get node -o wide`, INTERNAL-IP) and, if the host firewall drops it, to allow UDP 6490 from the pod network (`10.42.0.0/16`) in the survival-world firewall — that file is in the user's other repository.
- [ ] **Step 4: Confirm 6490 is closed from outside.**
  Run from the developer machine: `nc -u -z -w 3 5.9.7.138 6490; echo $?` and check the firewall rules the user keeps.
  Expected: not reachable from the internet (a UDP probe cannot prove closed on its own; the firewall rule list is the evidence — say which you saw).
- [ ] **Step 5: End to end.** Native: `VOXEL_LOGIN_URL=https://login.factoriosurvivalworld.com just play` → Play online → Sign in → Join "Orion" → in the world. A second login of the same account elsewhere takes the first over ("logged in elsewhere"). Browser: `https://orion.factoriosurvivalworld.com/` → Play online → Sign in → Open browser opens a tab → Join → in the world over WebTransport. `just orion-login account revoke <email>` ends that player's session within a few seconds (the kick is sent by both pods and applied once). Join five times in a row: every join succeeds whichever pod serves it. **Ask the user first — client launches.**
- [ ] **Step 6: Commit** `feat(deploy): the deployed game takes logins from the login service on orion`.

### Task 5: The image comes from AWS ECR (supersedes Decision 2) — added 2026-11-14

The user asked for it on 2026-11-14 ("you can push to aws ecr repo and pull from it in the cluster if possible"). It replaces the host-path binary before anything was applied. Verified the same day: the credentials here are an administrator of account `029905505680`; ECR in `eu-central-1` has no repository; Docker 29 with buildx is installed; the cluster is k3s v1.37 on a Hetzner machine — **no AWS instance role, and an ECR pull token lives 12 hours**, so the cluster must refresh its own pull secret.

**Decisions**
- **A `registry` root, applied before `login`:** ECR repository `mmo/login` (`eu-central-1`, scan on push, tags mutable, a lifecycle rule keeping the last 20 images) and an IAM user `orion-ecr-pull` that may only pull from it (the shape of `deploy/modules/ses-smtp-user`). It is its own root because the image must exist before the `login` root can start a pod, and because the game server's image (R5) will live beside it.
- **Private, not ECR Public:** nothing in the binary is secret, but a private repository costs only the refresher below and keeps the choice open.
- **Pull secret:** the `login` module creates the `kubernetes.io/dockerconfigjson` Secret `login-ecr` at apply time from `data "aws_ecr_authorization_token"` (so the very first pod can pull) and then ignores its data; a CronJob `ecr-refresh` (every 6 hours, plus the module's apply-time token) rewrites it with a fresh token using the pull user's key. On this single node a cached image also keeps pods restartable if a refresh is missed.
- **Releases are `kubectl set image`, not an apply:** `just orion-login-push` builds the release binary, builds the image (`debian:trixie-slim` + the binary at `/usr/local/bin/login`), pushes it as `mmo/login:<git short sha>` and sets it on the Deployment, then waits for the rollout; a failed rollout prints `kubectl rollout undo`. The Deployment ignores changes to its image, so a later apply does not roll a release back. The first push (no Deployment yet) also pushes the tag `bootstrap`, which is what the module starts from — so the first `just orion-apply login` needs no argument.
- **Gone:** the host path, `/srv/mmo/bin/login`, `login.prev`, rsync and ssh in the push (the box is no longer touched by a login release).

**Order for the owner now:** `just orion-apply registry` → `just orion-login-push` (first push: says the image is pushed and to apply the login root) → `just orion-apply login` → the checks of Task 2 → Task 3 Steps 5–7.

**Files:**
- Create: `deploy/modules/registry/{versions,variables,main,outputs}.tf`, `deploy/envs/orion/registry/{backend,versions,providers,main}.tf`, `deploy/images/login/Dockerfile`
- Modify: `deploy/modules/login/{variables,main,outputs}.tf` (image from the registry, pull secret, refresher, no host path), `deploy/envs/orion/login/main.tf` (reads the `registry` root's state), `scripts/orion_login_push.sh`, `justfile` (`orion-login-push`, `orion-login`, root lists and apply order), `CLAUDE.md`

**Interfaces:**
- `registry` module outputs: `repository_url` (`<account>.dkr.ecr.eu-central-1.amazonaws.com/mmo/login`), `registry_host`, `region`, `pull_access_key_id`, `pull_secret_access_key` (sensitive).
- `login` module: variables `image_repository` (no default), `image_tag` (default `bootstrap`), `ecr_region`, `ecr_pull_access_key_id`, `ecr_pull_secret_access_key` (sensitive), `refresh_image` (an image with the AWS CLI and kubectl, pinned); `image`, `command` and `bin_host_path` are removed; the container runs `login serve` from the image; `image_pull_secrets` = `login-ecr`; `image_pull_policy` = `IfNotPresent`; Deployment `lifecycle { ignore_changes = [the container's image] }`. New resources: Secret `login-ecr` (ignore_changes on data), Secret `ecr-pull-aws` (the pull key), ServiceAccount + Role + RoleBinding `ecr-refresh` (get / patch / update on Secret `login-ecr` only), CronJob `ecr-refresh`.
- Operator commands in the pod: `just orion-login …` runs `login …` (on `PATH` in the image).

- [ ] **Step 1:** write the `registry` module and root; `tofu fmt`, `validate`, `just orion-plan registry` (read-only) — expected: only additions (the repository, its lifecycle policy, the IAM user, its policy, its access key).
- [ ] **Step 2:** write the Dockerfile; build the image locally from the release binary and run `docker run --rm <image> --db sqlite::memory: world list` — expected: exit 0, an empty list; the image has no shell-less surprises for `kubectl exec` (it keeps Debian's userland) and `sleep` exists (the pre-stop hook).
- [ ] **Step 3:** change the `login` module and root; `tofu fmt`, `validate`. `just orion-plan login` cannot run until the `registry` root is applied (its state has no outputs yet): say so and leave it for the owner.
- [ ] **Step 4:** rewrite `scripts/orion_login_push.sh` and the recipes; prove them against stubs (`docker`, `aws`, `kubectl` stubbed on `PATH`): first push with no Deployment → pushes `<sha>` and `bootstrap`, exit 0 with the "apply the login root" line; a release → `set image` to the `<sha>` tag with the pinned context, waits; a failed rollout → non-zero with the `rollout undo` line; a cluster error → exit 2 (not "first push"); a dirty tree → the tag carries `-dirty` and the script says so.
- [ ] **Step 5:** the refresher's script, run locally against stubs: a token never reaches a log line; a failed `aws` call leaves the Secret untouched and fails the job.
- [ ] **Step 6:** docs (`CLAUDE.md`, justfile comments, the module and root headers) say the new order; commit.
- **Owner's steps after this task:** as "Order for the owner now" above; plus, once pods run, `just orion-kube -n login create job --from=cronjob/ecr-refresh ecr-refresh-check` and its log — expected: the Secret was rewritten, no token printed.

## Rollback

- Task 4: remove the added environment, port and volume from `deploy/web/compose.yml`, unset `web_login_url`, `just deploy`; the server is back on names (characters made under accounts stay in the world store, unreachable by name). `just orion-login world close orion`.
- Task 0 stays (the metrics are harmless without a scraper). Tasks 1–3: `just orion-plan login -destroy`, then the user applies it. The PostgreSQL volume is deleted with the StatefulSet's claim only if the claim is removed too; say so before destroying — it holds the account → character mapping.

## Self-review notes

- Decisions → tasks: 1, 5, 6 → Task 1 (Ingress, CORS env) and Task 2 Steps 7–8; 2 → Task 1 (`bin` host path, `image` / `command` variables), Task 3; 3 → Task 1 `postgres.tf`; 4 → Task 1 (`tls_private_key`, Step 4b), Task 2 Step 6; 7 → Task 1 secret + output, Task 3 `orion-login-token-push`; 8 → Tasks 0–3 vs Task 4; 9 → Task 1 (replicas, rolling update, budget), Task 2 Steps 6–6b, Task 4 Steps 3 and 5; 10 → Task 0, Task 1 (chart), Task 2 Steps 7 and 8b; 11 → not planned.
- Review Focus → steps: 6 → Task 0 Step 1; 7 → Task 2 Step 6; 1 → Task 2 Step 6; 2 → Task 1 Step 4, Task 3 Steps 3 and 7; 3 → Task 2 Step 5; 4 → Task 1 `postgres.tf` comment (no test: reproducing it means breaking the live database); 5 → Task 2 Step 7.
- Names checked across tasks: namespace `login`, Deployment `login`, `/srv/mmo/bin/login`, output `service_token`, recipes `orion-login-build|push|logs|token-push` and `orion-login`, world id `orion`, `/srv/mmo/secrets/{service-token,host-link.cert-sha256}`.
- Task 0 has no dependency on the cluster and can land first on its own. Task order is otherwise not the file order: Task 3 Steps 1–4 run before Task 2 Step 4 (the binary must be on the node before the first apply). Both tasks say so.
- **Not verified while planning** (each has a step that settles it and a named fallback): that a pod reaches `auth.factoriosurvivalworld.com` on the node's own public IP (Task 2 Step 5); that the release binary built on this machine runs on `debian:trixie-slim` (Task 3 Step 3); that OpenTofu's ED25519 PEM is the PKCS#8 form the service reads (Task 1 Step 4b); the `prometheus` crate's current 0.14 line and that `default-features = false` drops protobuf (Task 0 Step 4 — if the version differs, use the newest that does and say so); that the compose container reaches `login.factoriosurvivalworld.com` (Task 4 Step 2); that a pod reaches the game server's UDP 6490 and that the host firewall keeps it closed outside (Task 4 Steps 3–4); the exact log lines of the game server at start (Task 4 names what must be true, not a string to grep; the login service's lines in Task 2 Step 5 are from `crates/login/src/lib.rs`); whether `--quic` accepts a hostname (the plan uses the IP, which is what the dev recipes use).
- The deployed server's other settings (`VOXEL_WORLD_FILE`, single-process role `all`, SQLite world store) are untouched; a single host with role `all` and a service token is the R2a/R2b shape `just dev-login` and `dev-r2` exercise.

## As built (2026-11-14, commits `80bd7205`, `5d9fac55`, `6d580bc9`, `60022e97`, `494523bf`, `e5f21120`, `9f8c8573`, `852b0a55`) — Tasks 0–3, code only; nothing applied

- **Done without the cluster:** the metrics, the module, the root, the script and recipes; `just orion-plan login` (read-only) = 15 to add, 0 to change, 0 to destroy; OpenTofu's ED25519 key is accepted by the service; the release binary runs on `debian:trixie-slim`.
- **Differs from the text above:** a stopping pod sleeps 5 s before its stop signal (`pre_stop`), so a rolling restart drops no request; the hairpin fallback exists as `issuer_host_alias_ip` (default null); `orion-login-token-push` is a bash recipe that reads the token locally and installs it on the box only if the byte count matches; `orion-login-push` treats only NotFound as "not applied yet" (any other cluster error exits 2), keeps the previous binary as `login.prev` (overwritten at every push) and prints the way back on a failed rollout; the first apply must come AFTER the first push (an apply without the binary blocks ~10 min, fails and taints the Deployment); `login_host_link_up` is written only by a world's current link.
- **Task 2 Step 5's log check:** require `[login] identity-provider login on: …` — `listening on` is logged before Keycloak discovery.
- **Owner's steps, in order:** `just orion-login-push` → `just orion-plan login` → `just orion-apply login` → Task 2 Steps 4–8b (wait for the certificate before the curl checks) → Task 3 Steps 5–7. `orion-login-token-push` belongs to Task 4.
- **Open minors, rulings and reports:** `.superpowers/sdd/2026-11-14-login-service-on-orion/`.

## As built, Task 5 (2026-11-14, commits `12e4d4a0`, `b1e68306`, `1779fd28`, `46de4e66`, `8ea33338`) — replaces the host-path parts of the section above

- **Image, not a binary on the node:** private ECR repository `mmo/login` (`eu-central-1`) and pull-only user `orion-ecr-pull` in the new `registry` root; the login module pulls with Secret `login-ecr`, seeded at apply with the pull user's own token and rewritten every 6 hours by CronJob `ecr-refresh`.
- **One tag, `latest`** (the user: "I only care about latest"); the commit is an image label. **Releases set the digest** (`kubectl set image … login=<repo>@sha256:…`), so every push rolls out; untagged images expire a day after their push. `kubectl rollout undo` works while the previous image is still in ECR (about a day) or in the node's cache.
- **Gone:** the host path, `/srv/mmo/bin/login`, rsync / ssh in the push, `login.prev`, the `bootstrap` and sha tags, keep-20.
- **Owner's order:** `just orion-apply registry` (now: replaces the lifecycle policy only) → `just orion-login-push` (first push: says to apply the login root) → `just orion-init login` / `orion-plan login` / `orion-apply login` → the checks of Task 2 → `just orion-kube -n login create job --from=cronjob/ecr-refresh ecr-refresh-check` and its log → `just orion-login-push` again (sets the digest; pods restart once) → Task 3 Steps 5–6.
