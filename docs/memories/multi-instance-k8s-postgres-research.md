---
name: multi-instance-k8s-postgres-research
description: "Multi-instance world design (k3s + Postgres + central login): research 2026-09-20, all decisions settled 2026-11-06 in the doc's §0.10 (final state); roadmap R0–R6, nothing multi-process built yet"
metadata:
  node_type: memory
  type: project
  originSessionId: 4b5a0e1b-f98e-47e8-bbb0-e3a9b5af9c37
  modified: 2026-11-06T15:12:37.301Z
---

Doc: `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md`. **§0.10 is the
final state** (§0.1 changes since September, §0.3 / §0.6 / §0.9 / §0.10 decision rounds, §0.7
design, §0.5 + §0.10 roadmap). The Agones review is in `specs/2026-11-06-agones-review/`.
Don't re-research or re-decide.

**Settled 2026-11-06 (user):**
- **Seamless crossing between hosts ONLY**: no fallback, no border. It runs on the
  seam-transparent protocol (that plan was being implemented the same day).
- **Fixed super-regions** of S = 16 regions, runtime-configurable. Each is owned by one
  **region host**; hosts are a **StatefulSet** with a rendezvous hash over ordinals, and every
  write is fenced by a Postgres owner epoch. Agones was reviewed properly and rejected; client
  UDP goes through per-pod NodePort Services.
- **One world per deployment, one logical database per world.** Shared: Postgres (CNPG), the
  **login service** (email + password, Argon2id, JWT per world carrying the `PlayerToken` — so
  the token is no longer a secret; login tells the host to spawn the player), the **management
  panel**, the web client behind the Ingress, and Prometheus + Grafana.
- Per world: a **world coordinator** (singleton; history, claims) + the region hosts.
- An account can be in several worlds at once, **once per world** (sessions keyed
  `(account, world)`, newest wins per world).
- **Storage:** one schema on Postgres AND SQLite. Single-player is one `.sqlite` per world, no
  Postgres. The format is portable (terrain + `EntityId` bundles), not the memory image; saves
  are cut at one world tick.
- **The forged world is stored tiled in the database plus a coarse global layer**; hosts pull
  only their part. This is a big worldgen change (tile-cache sampling, bit-equal test).
- **Management panel:** a server-side-rendered **htmx** site in Rust using our crates. It forges
  (recommended as a `forge` subcommand run as a k8s Job), creates the database, seeds it,
  registers the world with login, and creates the k8s objects via kube-rs. Regions are not
  pre-generated.
- **LISTEN/NOTIFY** for terrain changes. No backups yet; concurrent-player target TBD.
- **Hosting:** ONE Hetzner server runs everything, on k3s (the user installs it separately);
  k3d locally. **No node autoscaling.** Region-host pod autoscaling is a later separate plan
  (R6); KEDA set aside.

**Rate limiting (doc §0.11) — DEFERRED by the user, NOT part of the initial k8s transition:**
- Limits apply at the edges only, never inside the sim. Six layers: login ticket issuance,
  nftables, quinn `Incoming` admission + retry, explicit `TransportConfig`, per-connection
  token buckets per message class with bounded queues + strikes, and per-account limits in
  login / coordinator.
- OPEN HAZARD: client→server frames use the snapshot-sized `MAX_DECOMPRESSED_PACKET_BYTES`
  (≥ 256 MiB) and unlimited bincode; the edge → manager channel is unbounded.

**Roadmap:**
1. Seam-transparent plan.
2. R0: dev compose Postgres.
3. R1: Postgres + SQLite stores, portable format, world-tick cut.
4. R2: split login / coordinator / hosts + panel.
5. R3: many hosts + the seamless go/no-go.
6. R4: takeover / failover.
7. R5: the single k3s server.
8. R6: pod autoscaling.

The leftovers listed in §0.10 (email provider, JWT lifetime, internal security, GDPR, CI,
client UX) go into the individual specs.

**Baseline:** the 09-22 Stage 0 Postgres plan was never run (marked obsolete). What shipped is
file stores, `PlayerStore` / `PlayerId`, and logout parks.

**Why:** the user's target is k8s + shared Postgres with central login; the test server is a
single Hetzner box.
**How to apply:** the next deliverable is two implementation plans (A: R0/R1 world store on
PG + SQLite; B: the forged world tiled in the database). The handoff to write them is
`docs/superpowers/plans/2026-11-06-persistence-plans-handoff.md`.
Lesson: I dismissed Agones without reviewing it and the user caught it — review before reversing
a past decision. Related: [[game-direction-pivot]], [[local-deployment-env]],
[[design-questions-need-tradeoffs]].
