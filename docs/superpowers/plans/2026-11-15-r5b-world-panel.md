# R5b — The World Panel (first slice) Implementation Plan

**Goal:** one operator page that shows every game world on a cluster and runs what the `world-*` recipes
run today — start, stop, forge — without a terminal. It is R5b's first slice: the spec's management panel
(`specs/2026-09-20-multi-instance-k8s-postgres-research.md` §0.9 (e), §0.10 item 5) minus creating and
deleting worlds.

**Architecture:** a new crate `crates/panel`, binary `panel`: axum + askama templates + htmx (vendored,
2.0.4), served on a ClusterIP Service only — the operator reaches it with `just orion-panel` (a
port-forward, as Grafana's admin path). It talks to the Kubernetes API with `reqwest` (rustls, ring —
the crate login already uses) using its ServiceAccount token in the cluster, or `kubectl proxy` outside
it. A world is a namespace labelled `app.kubernetes.io/name=world` (`deploy/modules/world`); the panel
reads its Deployments, StatefulSet, pods, ConfigMaps and each pod's `/readyz` and `/metrics` through the
API server's pod proxy (the `world-status` recipe's path).

## Decisions (made for Dawid, 2026-11-15 — revisit any of them)

1. **Scope of this slice: operate existing worlds.** List, status, start, stop, forge, the release
   history, forge Job logs. Creating, deleting and re-sizing a world stay in OpenTofu (`world` root):
   the host count is baked into `VOXEL_HOSTS`, the NodePort list and the host-endpoints ConfigMap, the
   database comes from the `postgres` root, the link identities are `tls_*` resources. A second slice
   (R5b-2) moves those to the panel or keeps them in OpenTofu — that is the open question below.
2. **No kube-rs.** `reqwest` + `serde_json` against the handful of endpoints the panel needs keeps
   `aws-lc-rs` out (CLAUDE.md: ring only) and keeps `k8s-openapi` out of the build. The spec named
   kube-rs; revisit when the panel creates many kinds of objects (R5b-2).
3. **Access control = cluster access.** No Ingress, no login. Whoever can port-forward to namespace
   `panel` can use it. Keycloak (a realm role) is the way to put it on a public name later.
   CSRF: every POST must carry `HX-Request: true` and, when the browser sends `Origin`, an origin whose
   host is the request's `Host` — a page on another site cannot drive a port-forwarded panel.
4. **Permissions are per world.** The `panel` module creates ClusterRole `world-panel` (what the panel
   may do inside a world namespace) and a ClusterRoleBinding only for listing namespaces. The `world`
   module binds `world-panel` in its own namespace when `var.panel` is set (a RoleBinding), so the panel
   can touch nothing outside world namespaces. Note: creating a Job lets the panel run the server image
   with the world's `world-db` Secret — that is the forge's purpose, and the reason the binding is per
   world.
5. **Start and stop follow the recipes exactly** (`world-start`, `world-stop`): stop refuses when the
   coordinator is at 0 with host pods left, follows each host pod's log until it ends, scales hosts to 0,
   reports each host's leave line (and WARNS on "left without a cut"), then the coordinator to 0; start is
   coordinator → 1, Ready, hosts → the count of `hosts-<i>.env` keys, all Ready. One operation per world
   at a time; its log is shown on the world's page.
6. **The forge is a Job** (`forge-<unix seconds>`) in the world's namespace, image and pull secret taken
   from the toolbox Deployment (the running release), `VOXEL_WORLD_DB` from Secret `world-db`, uid 10001,
   `backoffLimit 0`, `activeDeadlineSeconds 3600`, TTL 7 days, a memory limit chosen in the form
   (default 1Gi, as the toolbox). Forms: preset, size, cell, seed. `worldctl` refuses a store that holds
   a world; the panel also refuses while a forge Job is running or once a pod reports registered.
7. **Releases are `just orion-panel-push`** (image `mmo/panel` in ECR, set by digest, as login).

## Open (for Dawid)

- R5b-2: worlds created by the panel (kube-rs or this client; the database through a CNPG `Database`
  object; registration in login) versus a new `world` root instance per world.
- A public name behind Keycloak, and who gets the role.
- Scale (changing the host count) needs R5b-2 or R6.

## As built

- `crates/panel` (lib + bin + `tests/it.rs` against an in-process fake API server).
- `deploy/images/panel/Dockerfile`, `deploy/modules/panel`, roots `deploy/envs/orion/panel` and
  `deploy/envs/local/panel`; `var.panel` on `deploy/modules/world` (set in both world roots);
  `mmo/panel` added to the registry module's repositories.
- Recipes: `panel` (local, against any context through `kubectl proxy`), `orion-panel`
  (port-forward), `orion-panel-build`, `orion-panel-push`, `twin-panel`; `twin-images` / `twin-apply`
  include the panel.
- Nothing applied or pushed. Dawid's steps: `just orion-plan registry` + apply (the new repository) →
  `just orion-panel-push` → `just orion-plan world` + apply (the RoleBinding only) → `just orion-init panel
  && just orion-plan panel` + apply → `just orion-panel`, open http://127.0.0.1:8090/.

### Verified (2026-11-15)

- `just test-one panel ""`: 14 unit tests (metrics parsing, state derivation, forge form and Job, leave
  lines, the POST guard) and 7 in `tests/it.rs` against a fake API server (stop then start in the
  recipes' order with the leave lines read, the stop refusal, start without host files and on an
  unforged world, a forge Job on the toolbox's image and pull secret, a forged world refused, the pages,
  the POST guard over HTTP, `PRESETS` = the generator's). `cargo clippy -p panel --all-targets` clean;
  `cargo tree -i aws-lc-rs -p panel`: absent.
- `tofu validate` of `local/panel`, `orion/panel`, `local/world`, `orion/world`; `tofu fmt` clean.
- Read-only against orion (`kubectl proxy`, GETs only): `errantry` listed as running, coordinator 1/1, hosts
  2/2, 17 cuts committed, every pod's `/readyz` read through the pod proxy.
- Not run: start, stop or forge on any cluster; the image build; the RBAC (the ClusterRole has never been
  bound to a real ServiceAccount — a missing verb shows as a "forbidden" line on the world's page).
