# Handoff: observability stack on the `orion` k3s server (OpenTofu — user-confirmed — with S3 state)

**Date:** 2026-11-06
**Status:** IMPLEMENTED and applied 2026-11-06 (develop 43039c6 + b9a675a): roots
`deploy/envs/orion/{bootstrap,platform,monitoring}`, modules `cert-manager` + `monitoring`, state
in `s3://mmo-orion-tfstate-029905505680/orion/*.tfstate`. Verified: items 1–4, 6–10a. Still open:
item 5's time-dependent half (Thanos block upload after ≥ 2 h, compactor run, a query older than
Prometheus' 2d retention) and the optional live Pyroscope push. Deviations: the Grafana DNS record
already exists in survival-world's infra, so `manage_dns_record` is off; cert-manager is its own
root `platform`; Thanos is a local chart (`modules/monitoring/charts/thanos`); Loki/Tempo come from
`grafana-community` (the grafana/ charts were deprecated / GEL-only); Grafana needs a 1 GiB limit.
**For:** a fresh session that implements it end to end.
**Parent design:** `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md`
§0.9(g) / §0.10. The whole game stack (CNPG, login, panel, worlds) will later run on this same
server; this handoff is only the observability layer, which goes first.

## Goal

Deploy onto the single-node k3s server, as code with remote state in S3:

| Signal | Store | Long-term storage |
|---|---|---|
| Metrics | Prometheus (kube-prometheus-stack) | **Thanos → S3** (sidecar upload, store gateway, compactor, query) |
| Logs | Loki | S3 |
| Traces | Tempo | S3 |
| Profiles | Pyroscope | S3 |
| UI | Grafana, datasources for Thanos Query, Loki, Tempo, Pyroscope | (Grafana's own SQLite on a PV) |

The user asked for Thanos → S3. Loki, Tempo and Pyroscope also go to S3 in this design (each
supports it natively), so the node's local disk holds only short-lived data and the stack can be
rebuilt without loss.

## Verified facts (2026-11-06, read-only checks)

**Cluster** (kube context **`orion`** in `~/.kube/config`):
- **k3s v1.37.2+k3s1**, one node `hetzner-ryzen5-3600-1`, internal = external IP **5.9.7.138**.
- Ubuntu 26.04.1, kernel 7.0, containerd 2.3.7.
- Capacity: **12 CPU, ~64 GiB RAM, ~1.9 TB ephemeral disk**, 110 pods.
- Installed by k3s:
  - **Traefik** (default IngressClass `traefik`), exposed through k3s ServiceLB as `LoadBalancer`
    **5.9.7.138:80/443**;
  - `local-path` (the default StorageClass; `WaitForFirstConsumer`, **no volume expansion**);
  - CoreDNS and metrics-server.
- Traefik Hub and Gateway API CRDs are present (they come with k3s's Traefik).
- Namespaces: only the defaults. No cert-manager, no Prometheus CRDs.
- **Caddy is off and stays off** (user, 2026-11-06). Traefik is the only HTTP(S) entry, so TLS
  must come from inside the cluster (below). Side effect outside this handoff: the game server's
  WebTransport certificate used to be read from the survival-world Caddy's Let's Encrypt
  directory (`VOXEL_WT_CERT` / `VOXEL_WT_KEY`, CLAUDE.md "Web deployment"). That source is gone;
  the game's deployment will need a cert-manager certificate instead. Note it; don't fix it here.

**Workstation:**
- **OpenTofu v1.13.9** (`/usr/bin/tofu`); no `terraform` binary.
- `helm` is installed.
- `kubectl` client **1.30** against a 1.37 server is outside the supported skew (it warns).
  Upgrade kubectl before relying on it.

**Repo:**
- `deploy/` already uses OpenTofu: `modules/{platform,world}` and `envs/local/{platform,world}`
  (k0s-in-docker, **local state**).
  - Providers: `hashicorp/helm ~> 3.3` (v3 syntax: `kubernetes = { … }`) and
    `hashicorp/kubernetes ~> 3.2`; `required_version >= 1.8`.
  - `.gitignore` already excludes `deploy/**/.terraform/` and `deploy/**/*.tfstate*`;
    `.terraform.lock.hcl` files are committed.
- `modules/platform` installs openebs localpv, CNPG and **Agones**. The design dropped Agones
  (§0.7b′), but leave that module alone in this task.
- **Do not touch `deploy/web/compose.yml`, the Caddy setup or the survival-world repo.**
- The S3 backend pattern already in use (survival-world `infra/main.tf`):
  `backend "s3" { bucket = "survival-world-tfstate-029905505680", key = "...", region =
  "eu-west-1", encrypt = true, use_lockfile = true }`. AWS account 029905505680. OpenTofu
  supports `use_lockfile` (native S3 locking, no DynamoDB) from 1.10.
- The local profiling stack (`just profile-up`, root `docker-compose.yml`) runs Pyroscope 4040 +
  Grafana 3000 with provisioning in `scripts/provisioning/` (a Pyroscope datasource and the
  explore-profiles plugin). Reuse its datasource / plugin settings in the cluster Grafana.
- The game has a `pyroscope` cargo feature (client and server) that pushes to `:4040`. It has
  **no `/metrics` endpoint and no OTLP tracing yet**: game integration is a follow-up (end of
  this doc).

## Decisions (user, 2026-11-06) — all resolved

1. **OpenTofu** (`tofu`), not Terraform.
2. **A new, dedicated AWS S3 bucket holds the OpenTofu state.** Do not reuse the survival-world
   bucket.
   - Created by a **bootstrap root** `deploy/envs/orion/bootstrap`:
     - it starts with local state and creates the bucket (versioning on, all public access
       blocked, SSE-S3 default encryption, `prevent_destroy`);
     - then its own state is migrated into that bucket (`tofu init -migrate-state` with a
       `backend "s3"` block added), so no local state remains.
   - Name: `mmo-orion-tfstate-<account id>` (globally unique; read the id with
     `data "aws_caller_identity"`).
   - Keys: `orion/bootstrap.tfstate`, `orion/monitoring.tfstate`.
   - `use_lockfile = true`, `encrypt = true`.
3. **The telemetry data goes to AWS S3**, in buckets this design creates (see Object storage).
   - The account is the user's AWS account (survival-world uses 029905505680 — **confirm it
     is the same account** before creating anything).
   - Region: **eu-central-1 (Frankfurt) by default**. The server is a Hetzner box at 5.9.7.138,
     in Hetzner's German network, so Frankfurt is the nearest AWS region for Thanos / Loki
     reads. It is one variable; eu-west-1 (where the survival-world state lives) also works.
4. **Grafana's hostname is `grafana.factoriosurvivalworld.com`.**
   - The user typed "factoriosurviavlworld"; this assumes a typo for the existing domain
     `factoriosurvivalworld.com` (the one `orion.factoriosurvivalworld.com` lives in). **Confirm
     before creating DNS.**
   - The zone is in Route53, managed by the survival-world repo's `infra/`.
   - **Default: this repo's monitoring root creates only the A record** (`aws_route53_record`
     → 5.9.7.138, TTL 300), finding the zone with `data "aws_route53_zone"`. The zone stays
     owned by survival-world; the record is owned here.
   - The certificate is Let's Encrypt via cert-manager, HTTP-01 through Traefik. The record must
     resolve before the first issuance.
   - Everything other than Grafana stays internal (port-forward only).

Also confirm with the user: the retention defaults (below), and that the Grafana admin password
comes from a generated secret / tofu variable (never committed).

## Architecture

**Namespace `monitoring`.** Every component's chart version is pinned in variables, like
`modules/platform` does.

**1. cert-manager** (jetstack chart, namespace `cert-manager`): a `ClusterIssuer` for Let's
Encrypt, HTTP-01 through the `traefik` IngressClass. Grafana's Ingress gets its certificate
from it. It is shared platform: the game's WebTransport certificates will use it later, so it
may belong in a separate `platform` root (see Layout).

**2. kube-prometheus-stack** (prometheus-community): Prometheus Operator, Prometheus,
Alertmanager, node-exporter, kube-state-metrics, Grafana.
- Prometheus:
  - `retention` short (default **2d** / `retentionSize` 20GB on local-path), because Thanos
    holds history;
  - the Thanos sidecar enabled with an object-store secret;
  - `externalLabels: { cluster: orion }`;
  - `serviceMonitorSelectorNilUsesHelmValues: false` and the same for PodMonitors / rules, so
    later game ServiceMonitors are picked up without chart labels.
- **k3s quirk:** the controller-manager, scheduler, proxy and etcd are embedded in the k3s
  process and not scrapeable at the chart's default endpoints, so the default scrapes show red.
  Disable `kubeControllerManager`, `kubeScheduler`, `kubeProxy` and `kubeEtcd` in the values (or
  expose them with k3s flags and point the scrapes at the node IP — not needed now).
- Grafana:
  - a PV on local-path;
  - anonymous access **off**, admin from a secret;
  - plugins: `grafana-pyroscope-app` (as in `docker-compose.yml`);
  - datasources provisioned in the values:
    - **Thanos Query as the default Prometheus datasource** (not Prometheus directly);
    - Loki, Tempo and Pyroscope;
    - Tempo ↔ Loki ↔ Pyroscope correlations (trace-to-logs, trace-to-profiles), configured but
      harmless while no app emits traces.
  - An Ingress on `traefik` with TLS from cert-manager.

**3. Thanos.**
- The sidecar (from kube-prometheus-stack) uploads 2 h blocks.
- Add **Query** (+ optionally Query Frontend), **Store Gateway** and **Compactor**.
- **Chart choice is an open research item** — verify, don't assume:
  - there is no official Thanos Helm chart;
  - Bitnami's chart / images moved behind a paid catalogue in 2025 (check the current state,
    don't depend on `bitnami/*` images without confirming);
  - candidates: a maintained community chart, or plain manifests in a small tofu module
    (`kubernetes_manifest` / `kubernetes_deployment_v1`). Three Deployments / StatefulSets are
    small enough to own.
- The Compactor must be a **singleton** (one replica), with downsampling and retention
  (default raw **30d**, 5m **90d**, 1h **1y**).
- The Store Gateway and Compactor get local-path PVs for cache / scratch. The compactor needs
  real disk: size it at 50–100 GiB, and remember local-path cannot expand, so pick the size
  up front.

**4. Loki** (grafana/loki chart, **monolithic / SingleBinary mode**, `replicas: 1`):
- S3 storage;
- the TSDB index (`schema v13`);
- retention via the compactor (default **30d**);
- caches off or small.

**5. Tempo** (grafana/tempo chart, monolithic):
- S3 backend;
- receivers: OTLP gRPC 4317 / HTTP 4318;
- retention default **14d**;
- metrics-generator optional (off).

**6. Pyroscope** (grafana/pyroscope chart, single binary):
- S3 storage;
- port 4040 internal;
- the game's `pyroscope` feature will push here later.

**7. Grafana Alloy** (grafana/alloy chart, a DaemonSet):
- collects pod logs (Kubernetes API or files) → Loki;
- an **OTLP receiver** → Tempo, the single in-cluster ingest point for future game traces;
- optionally kubelet / cAdvisor scraping, but kube-prometheus-stack already does that, so
  don't duplicate it;
- eBPF profiling for Pyroscope is optional and needs a privileged DaemonSet: leave it off unless
  the user asks.
- **Do not use Promtail or Grafana Agent** — both are deprecated in favour of Alloy (verify
  status).

**Object storage (AWS S3, in the monitoring root via the `aws` provider):**
- **Four buckets**, one per component, so each has its own lifecycle rule:
  `mmo-orion-thanos-<acct>`, `mmo-orion-loki-<acct>`, `mmo-orion-tempo-<acct>`,
  `mmo-orion-pyroscope-<acct>`. Names come from a variable prefix.
- Each bucket: public access blocked, SSE-S3, **no versioning** (telemetry is disposable), and
  a lifecycle expiry a little past the component's retention as a backstop. Thanos needs ≥
  its longest downsampled retention (1y + margin); the component's own compactor does the real
  deletion.
- **Credentials:** a dedicated IAM user `mmo-orion-telemetry` whose policy allows only
  `s3:ListBucket` / `GetObject` / `PutObject` / `DeleteObject` (plus the multipart actions)
  on those four buckets.
  - Its access key is created by tofu and written into a Kubernetes Secret in `monitoring`.
  - The key therefore lives in the (encrypted, private) state bucket — acceptable; there is
    no workload identity off-AWS.
  - Rotation: `tofu apply -replace` on the access key.
- The component configs use the plain AWS S3 endpoint (`s3.<region>.amazonaws.com`) with
  virtual-host-style addressing — no custom endpoint or path-style needed.
- **Cost note:** Grafana reads of old data are AWS egress to Hetzner (internet egress pricing).
  At one server's telemetry this is small, but query-frontend caching and the store gateway's
  index cache keep it down.

**Resource budget** (single 64 GiB box shared later with the game): set requests / limits so the
whole stack stays within **~8 GiB RAM / ~3 CPU** at rest. Rough starting points:
- Prometheus: 2 GiB;
- Thanos Store + Query + Compactor: 1.5 GiB together;
- Loki: 1 GiB;
- Tempo: 1 GiB;
- Pyroscope: 1 GiB;
- Grafana: 256 MiB;
- Alloy: 256 MiB.

Tune after a day of data.

## Repo layout

```
deploy/
  modules/
    monitoring/          # kube-prometheus-stack + thanos + loki + tempo + pyroscope + alloy
      main.tf variables.tf outputs.tf versions.tf
      values/*.yaml.tftpl   # chart values as templates (S3 config, retention, hostnames)
    cert-manager/        # or fold into a new "orion platform" module — implementer's call, say which
  envs/
    orion/
      bootstrap/         # aws provider only: the state bucket; local state, then migrated into the bucket
      monitoring/
        backend.tf       # backend "s3" { bucket, key = "mmo/orion/monitoring.tfstate", region, encrypt, use_lockfile }
        providers.tf     # kubernetes + helm (config_path = var.kubeconfig, config_context = "orion") + aws (region var, profile var)
        main.tf          # module "monitoring" { … }
        versions.tf
        terraform.tfvars.example   # committed; the real *.tfvars is git-ignored (add a rule)
```

- One root per concern on this server; later roots (`envs/orion/platform` for CNPG etc.,
  `envs/orion/worlds`) get their own state keys. CRD-before-CR ordering is the same reason
  `local` is split into `platform` and `world`.
- Within monitoring, ServiceMonitors / PrometheusRules created by *this* root must wait for the
  kube-prometheus-stack release (`depends_on`). If `tofu plan` fails because the CRDs are
  missing at plan time, split them into a second root, exactly like `local/world`.
- **Secrets:**
  - S3 keys and the Grafana admin password come in via `TF_VAR_…` or a git-ignored
    `terraform.tfvars`;
  - add `deploy/**/*.tfvars` (except `*.tfvars.example`) to `.gitignore`;
  - they land in the S3 state, so the state bucket must stay private and encrypted (it is,
    with `encrypt = true`).
- **AWS credentials** (backend and the `aws` provider): an AWS CLI profile on the workstation,
  passed as a variable / `AWS_PROFILE`. Ask the user which profile — likely the one the
  survival-world infra uses. Never print keys; never commit `~/.aws` or `.env` contents.
  The backend block cannot take variables, so pass the profile and region with
  `tofu init -backend-config=...` from the justfile recipe.

**justfile** (CLAUDE.md requires keeping it in sync): add recipes such as
- `just orion-monitoring-init` (`tofu init` with the backend);
- `just orion-monitoring-plan` / `orion-monitoring-apply`;
- `just orion-grafana` (port-forward fallback);
- `just orion-kube <args>` (kubectl `--context orion`);

and pin them to the `orion` context explicitly — never the current context. Update CLAUDE.md's
deployment paragraph in the same change.

## Verification (all must pass before calling it done)

1. The bootstrap root created the state bucket, and its own state was migrated into it (no
   local `terraform.tfstate` left). `tofu init` in monitoring uses the S3 backend; the state
   object exists at its key; a lock file appears during apply.
2. `tofu apply` succeeds; **a second `tofu plan` shows no changes.**
3. `kubectl --context orion -n monitoring get pods`: all Ready, no restarts after 10 minutes.
4. Prometheus targets: node-exporter, kube-state-metrics, kubelet / cAdvisor, Alloy and the
   stack's own components are up; the disabled k3s control-plane scrapes are absent, not red.
5. Thanos:
   - Query lists the sidecar and the Store Gateway as stores;
   - **after ≥ 2 h**, block objects exist in the Thanos bucket and the compactor log shows it
     ran;
   - a Grafana query over a range older than Prometheus' retention returns data (re-check the
     next day if needed).
6. Loki: in Grafana Explore, `{namespace="monitoring"}` returns recent pod logs; objects appear
   in the Loki bucket.
7. Tempo:
   - send one test trace to Alloy's OTLP endpoint (e.g. `telemetrygen traces` in a temporary
     pod, deleted afterwards);
   - it shows in Grafana Explore → Tempo; objects appear in the bucket after the flush.
8. Pyroscope:
   - the datasource "Save & test" is OK;
   - optional live check: port-forward 4040 and run `just profile-server` against it (the
     `pyroscope` feature pushes to `:4040`);
   - **ask the user before any live game run** (CLAUDE.md / memory: the user runs their own
     benches on this box).
9. `dig +short grafana.factoriosurvivalworld.com` returns 5.9.7.138. Grafana is reachable at
   `https://grafana.factoriosurvivalworld.com` with a valid Let's Encrypt certificate;
   anonymous access is refused.
10a. The four data buckets: public access blocked, encrypted, lifecycle rules present. The IAM
    user can read and write them but nothing else (e.g. `aws s3 ls` of another bucket with its
    key is denied).
10. Restart test: delete the Prometheus, Loki and Grafana pods; they come back with their data
    (PVs) and nothing in S3 is lost.

## Out of scope (follow-ups, note them in the final report)

- **Game integration:**
  - the server exports `/metrics` (`metrics` + `metrics-exporter-prometheus`) with today's
    `[server]` / `[region …]` counters, plus ServiceMonitors;
  - OTLP traces via Alloy;
  - the `pyroscope` feature pointed at the in-cluster Pyroscope.
- **Dashboards as code:** in git, provisioned via ConfigMaps with the Grafana sidecar label.
- **Alertmanager routes** (email / notification channel) — no receiver configured yet.
- **The game's WebTransport certificate from cert-manager** (Caddy is gone; above).
- **CNPG, login, panel, worlds on this server**: the design doc's R0–R5.
- **Backups:** none by decision (§0.10). The S3 buckets are the only durability for telemetry.

## Rules for the implementing session

- Work on `develop` in the main checkout. **No git worktrees** (CLAUDE.md).
- The user has uncommitted work: `git add` only the files you created or changed, and run
  `git status` before every commit.
- Commit messages end with the session's attribution line.
- Every `kubectl` / `tofu` command targets the **`orion` context explicitly**. Applying to the
  server is outward-facing: **show the first `tofu plan` to the user and get an OK before the
  first `tofu apply`.**
- Never print or commit credentials (S3 keys, AWS profile secrets, the Grafana password,
  `.env`).
- Verify chart names / versions / values keys against the charts' current docs at
  implementation time; this handoff names components, not exact values keys.
