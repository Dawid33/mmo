# Local deployment environment (OpenTofu + k0s + Agones) — design and verified setup

Date: 2026-09-22. Status: **implemented and verified locally** (see "Verified"). Parent:
`2026-09-20-multi-instance-k8s-postgres-research.md` (§1a decision 5: Agones; production target
= single-node k0s). Deployments are declared in **OpenTofu** (user decision, 2026-09-22).

## Three tiers — use the lightest one that answers the question

| Tier | What runs | For | Command |
|---|---|---|---|
| 0 | native server + client, in-memory store | gameplay, netcode, rendering, all test suites | `just run` |
| 1 | native server + client, Postgres in one docker container | persistence, login, checkpoints (durable-world plan) | `just pg-up`, `just run-durable` (plan `2026-09-22-durable-world-persistence.md`, not built yet) |
| 2 | single-node k0s in docker, configured by OpenTofu | the deployment itself: Agones game servers, CloudNativePG, storage, drain, multi-pod | `just deploy-local` |

Tier 2 is never on the inner loop: an image rebuild + rollout is minutes, a native restart is seconds.

## Tier 2 layout

```
deploy/
  modules/
    platform/  # namespace `mmo`, OpenEBS LocalPV (default StorageClass), CloudNativePG operator, Agones
    world/     # CloudNativePG Cluster `mmo-db`; Agones Fleet `world` (only when game_image is set)
  envs/
    local/
      platform/  # root: module.platform, single_node = true
      world/     # root: module.world, 2Gi DB, smaller game resources
```

- **Two roots, applied in order.** `world` declares CloudNativePG and Agones custom resources with
  `kubernetes_manifest`, which needs their CRDs to exist *at plan time*; one root could never plan
  on an empty cluster. `just deploy-local` applies `platform`, then `world`.
- **Cluster outside OpenTofu.** The local node is created by `just dev-cluster-up` (k0s's
  documented single-container mode, same distribution and version line as production); production
  k0s is installed with `k0sctl`. OpenTofu configures what runs *on* a cluster, given a kubeconfig.
- **State**: local roots use local state (gitignored) — the cluster is disposable
  (`just dev-cluster-destroy`). A production root needs a remote backend (S3-compatible bucket, or
  the `pg` backend on a database that is *not* the game's); to be added with `envs/prod` when the
  host exists.
- **Secrets never enter state**: CloudNativePG generates `mmo-db-app`; its `uri` key is a complete
  `DATABASE_URL` (`postgresql://…@mmo-db-rw.mmo:5432/mmo`), injected into the game container by
  `secretKeyRef`.
- Versions pinned in the modules: Agones chart 1.60.0, cloudnative-pg chart 0.29.0 (operator
  1.30.0, Postgres **18.4** by default), localpv-provisioner 4.6.0, providers helm ~> 3.3,
  kubernetes ~> 3.2, k0s `v1.35.8-k0s.1`.

### Settings a bare single-node k0s needs (all found by running it)

1. **No LoadBalancer implementation.** Agones' allocator and latency-ping Services default to
   `LoadBalancer`, stay `<pending>` forever, and hang `helm --wait`. The platform module sets the
   allocator to `ClusterIP` and disables ping — clients dial game server pods directly, neither is
   used. (Holds for the production box too.)
2. **No default StorageClass.** k0s ships none; the Postgres PVC would never bind. OpenEBS LocalPV
   hostpath is installed as the default (`/var/openebs/local` on the node — a named docker volume
   in the dev container). Production single-node needs the same.
3. **Disk-pressure taint (dev machine only).** The kubelet's default eviction threshold is
   "< 15 % disk free"; this laptop's 938 GB disk at 87 % used tainted the node and nothing
   scheduled. The dev node runs with absolute thresholds (`nodefs/imagefs.available<5Gi`).
4. **One replica per operator component** (`single_node = true`): controller/extensions/allocator
   default to 2–3 replicas.
5. **Multi-container game pods must name the game container** (`spec.container = "server"` in the
   GameServer template) — Agones rejects the Fleet otherwise.

### Game servers (Fleet) — shape ready, image pending

`world` creates the Fleet only when `game_image` is set. Each pod gets **two Dynamic UDP host
ports** (`quic` → 6466, `wt` → 6467) on the node address, and `DATABASE_URL` from the DB secret.
Until the server speaks the Agones SDK, a `curlimages/curl` sidecar marks the GameServer Ready and
sends health pings over the SDK's REST port.

What the server still needs before a real image can run (multi-instance stage 3):
- bind `0.0.0.0` (it binds `127.0.0.1:6466/6467` today) and read bind/advertised addresses from env;
- learn its allocated host ports (Agones SDK `GameServer().status.ports`, or the downward API) so
  the WebTransport cert-hash / front door hand out the right port;
- a container image: `just image` building the release binary on the host (where the build cache
  lives) into a slim runtime image, pushed to a local `registry:2` the node pulls from;
- the wasm page currently fetches `assets/webtransport-cert-hash.json` from the server's disk —
  in a pod it must be served over HTTP (until the front door exists);
- the persistence plan (tier 1) for `DATABASE_URL` to mean anything.

## Verified (2026-09-22, this laptop)

1. **Agones on single-node k0s-in-docker, two game servers, dynamic ports** — two
   `simple-game-server:0.43` GameServers, each with two Dynamic UDP ports, both `Ready` on the one
   node with distinct ports (7807/7848, 7144/7333); UDP echo from the host to
   `172.17.0.2:<port>` answered `ACK` for both. (This is the §10a feasibility test of the parent
   doc; minikube was not needed.)
2. **`just deploy-local` from an empty cluster**: platform (namespace, LocalPV 13 s, CNPG 18 s,
   Agones 35 s) and world (Postgres cluster healthy in 45 s, PVC bound) applied first time;
   `just deploy-local-plan` afterwards reports **No changes** for both roots.
3. `psql` inside the DB pod answers (`mmo`, Postgres 18.4); the app secret's `uri` targets
   `mmo-db-rw.mmo:5432/mmo`.
4. **Fleet with a stand-in image** (`registry.k8s.io/pause:3.10`, `-var game_image=…`): the
   GameServer reached `Ready` via the sidecar alone, got two Dynamic ports (7570/7621), stayed
   `Ready` past 75 s of health checking with 0 restarts, and had `DATABASE_URL` from
   `mmo-db-app/uri`. Removed again afterwards (plain apply).

## Recipes

| Recipe | Does |
|---|---|
| `just dev-cluster-up` | create/start the `mmo-k0s` node, write `target/dev-cluster/kubeconfig`, wait for Ready |
| `just deploy-local` | cluster up + `tofu apply` platform then world |
| `just deploy-local-plan` | same, `tofu plan` only |
| `just kube <args>` | `kubectl` against the dev cluster (e.g. `just kube get gs -A`) |
| `just dev-cluster-down` | stop the node, keep its volumes (DB survives) |
| `just dev-cluster-destroy` | **delete** node, volumes and local state |

Game server ports are reachable from the host at the node's docker IP (printed by
`dev-cluster-up`), range 7000–8000.

## Next

- Server changes above → `just image` → `-var game_image=…` becomes the real server.
- `envs/prod/{platform,world}` + remote state once the k0s host exists (same modules;
  `single_node = true`; host firewall opens UDP 7000–8000).
- Agones Rust SDK in the server (replaces the sidecar; reports allocated ports; `Allocated` while
  it owns territory with players, which protects it from scale-down).
