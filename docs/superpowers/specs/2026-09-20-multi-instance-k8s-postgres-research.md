# Multi-instance servers on Kubernetes with a shared Postgres world — investigation & proposal

Date: 2026-09-20, **revised 2026-11-06** (§0; user decisions in §0.3, §0.6, §0.9 and §0.10 — §0.10 is the final state of this pass). Status: the multi-instance part is **not
implemented**; single-instance persistence shipped in a different form (files, §0.1). Decisions
marked **[DECIDE]** are the user's; everything else is a recommendation with its reasoning.
**Read §0 first** — §1–§12 are the 2026-09-20 text, kept as the record, with pointers where
they went stale.

## 0. Revision 2026-11-06 — what changed since 2026-09-20 and what it means

### 0.1 What changed (verified in code / docs on 2026-11-06)

**Direction.**
- **Dwarf Fortress pivot (2026-10-20).** One huge world, forged up front, is the game. Players
  hold a *fortress* (a site) or play an *adventurer*; the MVP is fortress mode. The later
  2026-09-20 direction "super region = one pod = its own finite world, loading screen between"
  is superseded in its *own-world* part. There is one world file per world (the generator
  defaults to 100 km ≈ 392² regions of 256 m). Whether a pod owns a *piece* of that world is
  the open question this doc is about.
- **Scale numbers moved.** Play regions are **256 m** (they were 64 m; per-region cost scales
  with area, so ×16). The sim chunk is 16³ blocks. Cell mode was built and then removed. A
  fortress client simulates the **3×3 fortress neighbourhood** (768 m square). LOD reaches a
  **15 km horizon** (~4,400 LOD keys per client), so "view ≫ territory" (§3) is worse than
  before. The §2 measurements (40 MiB/region, colliders dominate) are from 64 m regions with
  2-block chunks: **stale, re-measure before quoting.**
- **Terrain is a stored artifact** (`terrain.world`, keyed by `content_crc`). Continents can no
  longer re-roll under a world, which defuses most of §6.1's pristine-policy hazard. What
  remains is a *code* hazard: per-block sampling (trees, slopes, materials) still changes
  between builds — the slope change bumped `REGION_FILE_VERSION` to 7 and set every save aside.
- **World-global state now exists:**
  - **The world history**: people, sites, civs, journeys and a calendar on wall time. It lives
    inside `WorldManager`, is delivered into regions as sim events and is saved to
    `terrain.history`.
  - **World-global claims and tables**: embark / return / adventure exclusivity
    (`fortress_of`, `adventurer_of`), the `WorldMap` sent on connect, and legends.

  None of this partitions by region, so §4 has a new actor: a **world coordinator**.

**Shipped single-instance persistence: files, not Postgres.** The Stage 0 Postgres plan
(`plans/2026-09-22-durable-world-persistence.md`) was never executed: 0 of 9 tasks, and no
sqlx in `Cargo.lock`. What shipped instead:
- **Region autosave** (`server/src/region_store.rs`):
  - Every 60 s, plus on park and at orderly exit, each region is written to
    `<world>.regions/b<R>-c<C>/r.x.z.region`.
  - The payload is the lz4 bincode **`Rollback` memory image**, which §6 argued against.
    §6's warning has already come true once: a version bump set every save aside (as `.bak`)
    and regenerated the regions. That is fine for dev; it is not fine for a world players own.
  - Known limit: LOD for a saved region that has not run this session is built from pristine
    worldgen. This is §8 case 2, unsolved even in one process.
- **Player identity and records**:
  - `PlayerId(u64)` behind a secret `PlayerToken`, sent in a `Hello` handshake as the first
    packet. There is no authentication.
  - Records go through the `PlayerStore` trait (`resolve` / `store`), which lives in `server`,
    not `game`. Today's implementation is `FilePlayerStore` at `<world>.players/`, written by
    a `player-saver` thread.
  - **Decision 4 is implemented**: a disconnect *parks* the body, claims and inventory into a
    `PlayerRecord`, and a reconnect restores it at the nearest standable cell.
  - **The newest session takes over.** The 09-22 plan had chosen "reject double login".
  - In-sim `ClientId` is still a per-process `usize`.
- **Orderly exit** (last client gone, stdin closed, SIGINT/SIGTERM) parks every region, saves
  them all and joins the savers. That is the drain primitive §10 needs.
- **History** is saved every 60 s and at exit.

**Planned and being implemented today: `plans/2026-11-06-seam-transparent-regions.md`.**
It delivers several things this doc listed as prerequisites or missing pieces:
- **One world tick numbering** (`WorldClock`, anchored to wall-clock ms), persisted with the
  autosave.
- **`EntityId { birth_region, serial }`**, allocated deterministically inside the region.
  This is the persistent id §6.2 needed, and it needs no database sequence, because a region
  has a single writer.
- **Generated, exhaustively classified transfer bundles**: a portable entity format.
- **Latency-tolerant cross-region messages.** A message applies at
  `max(T+1, the receiver's next tick)`, is recorded in the receiver's event stream, and **no
  region ever waits for another**. The manager keeps an `in_flight` table, re-delivers to a
  respawned target, and a conservation invariant checks that no entity is duplicated or lost.
- **Halo**: neighbours' terrain and entities, read-only, `HALO_BLOCKS` = 8, with an explicit
  "gone" message instead of TTLs. **Intents** carry writes to anything a region does not own.
  A **location directory** maps `EntityId` → region.
- **Task 3.3 `held_key_across_seam_settles`** is the in-process "seamless crossing under
  sustained input" that §1a named as the biggest prerequisite risk.

**Deployment.**
- `deploy/` (OpenTofu, single-node k0s in docker) exists, and the Agones test passed on
  2026-09-22. The Fleet still waits for a game image.
- **Real production today is one box running docker compose**: the bind-mounted server binary
  in `debian:trixie-slim`, `VOXEL_BIND_ADDR=0.0.0.0` (so the "127.0.0.1 binds must go" item
  is done), and WebTransport on a **Let's Encrypt certificate** for the hostname (not
  cert-hash pinning).
- The browser and native clients join by `host:port` (`--join`, the Join-server field).
- Single-player already runs as **one server process per world**, spawned by the client.

### 0.2 Section by section: what still holds

| § | Verdict |
|---|---|
| 1 / TL;DR 1: the region is the partition unit, single writer per region | **Holds.** The seam plan keeps regions as the threading, rollback and persistence unit. |
| 1 / TL;DR 3, §9: `Transfer` packet, client redirect | **Still needed** for login placement and drain. The client still has no redirect; it takes an address from the menu or the CLI. |
| 1 / TL;DR 4, §6: durable ≠ parking format | **Holds, and is now urgent**, because the memory image is what actually ships. The seam plan's generated bundle + `EntityId` gives the entity half almost for free. Terrain rows are naturally per 16³ chunk or per column. |
| 1 / TL;DR 5, §6.1, §1a.3: pristine policy | Freeze-on-first-run is **de facto implemented** (every region that ran is saved). The forged artifact removes the continent-scale reason for it; the per-block-code reason (above) keeps it right. |
| §1a.1: super-region S = 8 | **Arithmetic slip.** At 64 m regions S = 8 was 512 m, not "≈ 8 km". At 256 m it is 2 km. A random 3×3 fortress neighbourhood straddles a super-region edge ~44 % of the time at S = 8 and ~23 % at S = 16. **Decided 2026-11-06: fixed super-regions stay the claim unit, start S = 16** (§0.3, §0.4e). |
| §1a.2: seamless cross-pod (L2) is the goal | **Reopened, then decided 2026-11-06: seamless** (§0.3). DF changes the pressure: sites are ≥ `EMBARK_MIN_DISTANCE_REGIONS` (4 regions, 1 km) apart, so fortress players rarely meet a seam, and DF itself travels between sites on the world map. At the same time the seam plan makes L2 *much cheaper*: the region boundary becomes a message protocol (transfers, halo, intents, all T+1 and latency-tolerant) — exactly what a pod↔pod link would carry. |
| §1a.4: logout extracts the body | **Done** (parking + `PlayerRecord`). |
| §1a.5: Agones / single-node k0s | Agones is feasible on k0s; the public box runs docker compose. **Decided 2026-11-06:** compose for development (Postgres + services), k8s-native in production, tested on minikube, then a k0s/k3s test server (§0.3). On one node, more pods still add no capacity. |
| §2: facts table | Largely stale: identity, persistence, the address, region size, LOD reach, ghosts (now halo), arrivals keyed by `EntityId`. Use §0.1. |
| §3: constraints | Hold. A player still pins 9 regions, now at 16× the area each. The scaling ceiling is still one region per core. |
| §4: options A / B / C | Decided: fixed super-regions with swappable ownership policies (§0.4b). A's main drawback — every new player lands on the one spawn pod — is gone, because embark picks a tile. C (gateway) is still rejected **for region traffic**, but world-level state now needs one owner: the world coordinator of §0.4a, which is narrower than C. |
| §5: border levels L0 / L1 / L2 | L1 (observer connections) is now less interesting than L2-over-seam-protocol, because the halo already gives the near-seam detail L1 was for. |
| §6.2: crash consistency | **Problem exists today, in one process**: regions answer `Save` at different ticks, so an entity crossing between two saves can be duplicated or lost on a crash restore. Fix: a **consistent cut at a world tick** — each region saves at the same world tick T, and the manager saves the `in_flight` transfers departed < T and not yet applied. The shared world tick from the seam plan makes this cheap; nothing has to wait. |
| §7: leases + epoch fencing in Postgres | **Unchanged**, applies once there is more than one process. |
| §8: LOD across instances | **Unchanged in shape.** Fix it single-process first (R1): a saved region becomes an LOD overlay source. The same code then serves foreign regions. |
| §9: identity, front door, ticket | Identity half done (`PlayerId` / `PlayerToken`). Front door + signed ticket still needed for multi-pod. Newest-wins takeover across pods needs a fenced session row (`session_instance` / `epoch`). |
| §10: k8s shape | Holds, with one simplification: with a hostname-bound Web-PKI certificate, one box needs no per-pod cert hashes. Multi-node needs per-node names (or the cert-hash route). Drain = wire SIGTERM / preStop to the orderly exit that exists. |
| §11: benchmarks | The go/no-go metric becomes the seam plan's harness (conservation + `world_state_hash`) with an injected per-link delay, plus client corrections around the flip. |
| §12: roadmap | **Replaced by §0.5.** |

### 0.3 Decisions taken (user, 2026-11-06) — supersede §1a and the reopened questions

1. **Seamless crossing between pods** (L2) is the goal. Cross-pod traffic is the seam
   protocol of `plans/2026-11-06-seam-transparent-regions.md` (transfers, halo, intents, all
   T+1 and latency-tolerant), carried over a host↔host link.
2. **Transition to Postgres** now; it is not deferred until a second process exists. The file
   stores stay behind the same traits for single-player and no-database runs.
3. **Fixed super-regions are the claim unit**: an S×S square of regions on a grid fixed from
   day one. `SUPER_REGION_SIZE` is a named tunable.
4. **Development:**
   - docker compose runs Postgres, and later the other services.
   - The details are **relaxed and swappable**: *how* a process takes ownership of regions,
     *how* processes are spawned and *how many* run are picked for testing convenience.
5. **Production:** k8s-native mechanisms for ownership and spawning. They are tested locally
   on minikube, then on a k0s or k3s test server for near-production testing (whichever gets
   set up).
6. **Login is its own centralised deployment.**
   - It does all authentication.
   - It **tells the region hosts to spawn a user**.
   - Region hosts never authenticate anyone.

### 0.4 What the decisions imply (recommendations)

**(a) Process roles.**

| Role | Count | Owns |
|---|---|---|
| **Login service** | one deployment (replicas share Postgres) | Authentication, accounts, the **session table** (one live session per player — "newest wins" is enforced here, centrally), placement (player → super-region → owning host), the `SpawnPlayer` order to that host, re-placement on reconnect and drain |
| **Region host** | N processes / pods | The region threads of the super-regions it owns, client connections that carry a login-issued ticket, region and player writes to Postgres, links to neighbouring hosts |
| **World coordinator** | singleton | World-global state that does not partition: history + calendar, site claims, embark / adventure exclusivity, the `WorldMap`, legends. It may share a process with the login service in development (open, §0.6) |
| **Postgres** | one cluster | Durable truth plus the ownership fencing epochs |

Today's single `server` binary becomes these roles. The binary can still run all of them in
one process (the `AllInOne` setup below), which keeps single-player and `just run` working.

**(b) Ownership is a policy behind a trait, fencing always lives in Postgres.**
- Swappable ownership sources:
  - `AllInOne`: one process owns everything (today).
  - `Static`: dev config or env — host *i* owns these super-regions, or a modulo rule over N
    hosts.
  - `PgClaim`: claim on demand with lease rows, §7.
  - `K8s`: prod — e.g. StatefulSet ordinal → a shard map, a k8s `Lease` per super-region, or
    Agones allocation labels. The choice belongs to the k8s-stage spec.
- **Invariant, whatever the source:** each super-region row in Postgres carries an **owner
  epoch**. A new owner bumps it in a transaction when it takes over, and every region or
  player write checks it in the same transaction.
- Why the invariant matters: §7's argument still holds — a k8s `Lease` alone cannot fence a
  Postgres write. A paused old owner would write over the new one. So k8s (or a dev config)
  decides *who* owns a super-region, and the Postgres epoch makes a stale owner's writes fail.
  This is also what makes relaxing the policy safe in development.

**(c) Spawning is outside the binary.**
- The binary reads its role and identity from env (12-factor): role, host index / name,
  advertised address, `DATABASE_URL`, ownership policy.
- **Development:** a `just` recipe and a dev compose file start Postgres + login + N hosts
  (`--scale` or a loop). The count is a variable.
- **Production:** a k8s controller (Deployment / StatefulSet / Agones Fleet) starts them.

**(d) Login flow** (the "ticket" below is the signed access JWT, §0.7a). This replaces §9's model, where the client presented a ticket and the
host loaded the record itself; now login *pushes* the spawn.
```
client → login (HTTPS): credentials                  → authenticate
login: open session (player, session_epoch); an old live session → tell its host to park it
login: player record → super-region → owner host     (Postgres: records + ownership rows)
login → host (internal):  SpawnPlayer { player, session_epoch, ticket }
host:  region thread restores / places the body (today's RestorePlayer path), holds the slot
login → client: { host QUIC / WT address, ticket }
client → host:  Hello { ticket }  → matched to the pending spawn → play
```
- **Crossing between hosts** does not involve login on the hot path. The receiving host
  updates the session's current host in Postgres, so a reconnect is placed correctly.
- **Logout / disconnect:** the host parks the body, writes the record (fenced) and tells login
  that the session ended.
- **Takeover:** login orders the old host to park and drop that session *before* it orders
  the new spawn. That is the cross-host form of today's newest-wins.
- **Credentials:** today's `PlayerToken` either becomes a credential the login service checks,
  or is replaced (open, §0.6). The `Hello` the host receives carries the login ticket.

**(e) Super-region size.** S is fixed but tunable. At 256 m regions, S = 8 is 2 km and
S = 16 is 4 km. With a fixed grid, a fortress neighbourhood near a grid line spans two owners
(~44 % of random placements at S = 8, ~23 % at S = 16). With seamless crossing that is a
cost question, not a correctness one. **S = 16, configurable** (§0.7d).

**(f) Client.**
- It logs in over HTTPS first (native and browser), instead of taking `host:port` from a menu
  or CLI.
- It holds **one connection per host** that owns a region of its window, with a
  `region → connection` routing table. This is required for seamless crossing.
- Login gives it its home host; any other host comes from a `RegionElsewhere` redirect
  carrying a ticket.

### 0.5 Revised roadmap (replaces §12)

- **Today: the seam-transparent plan.** It is the prerequisite for everything below.
- **R0 — dev infrastructure:**
  - a dev compose file (e.g. `deploy/dev/compose.yml`, separate from the production
    `deploy/web/compose.yml`) with Postgres;
  - `just pg-up` / `pg-down` / `pg-reset`;
  - sqlx migrations.
- **R1 — Postgres persistence, one host:**
  - `RegionStore`, `PlayerStore` and a history store implemented on Postgres, with file
    implementations kept;
  - a portable region format (terrain per chunk / column + entity bundles keyed by
    `EntityId`);
  - a world-tick-consistent save cut that includes `in_flight`;
  - LOD built from stored regions;
  - re-measure sizes at 256 m / 16-block chunks.

  The parts of `plans/2026-09-22-durable-world-persistence.md` that still fit are reused.
- **R2 — split the roles, still one machine:** **DONE 2026-11-08** (`plans/2026-11-07-r2a-login-accounts.md`, `plans/2026-11-07-r2b-role-split.md`)
  - a login service (auth, sessions, `SpawnPlayer`), the world coordinator and the region
    host as separate processes;
  - the `AllInOne` and `Static` ownership policies;
  - super-region owner epochs in Postgres;
  - client login over HTTPS.
- **R3 — many hosts locally:**
  - N hosts under `Static` ownership, launched by `just` / compose;
  - a host↔host link carrying the seam protocol;
  - client multi-connection.
  - **Go / no-go:** conservation holds, and the client's corrections at a cross-host flip
    match a cross-thread flip at p95 under 1–5 ms injected latency.
- **R4 — takeover:** the `PgClaim` policy, failover tests (kill a host mid-crossing), drain
  (SIGTERM → orderly exit → release → login re-places the players).
- **R5 — k8s (§0.7b/c/e: StatefulSet + headless Service, per-pod NodePort Services, login Ingress, CloudNativePG; the Agones Fleet in `deploy/modules/world` is replaced):**
  - images and a k8s-native ownership + spawning policy;
  - run on minikube (the existing `deploy/` k0s-in-docker environment can stand in);
  - then the k0s / k3s test server: readiness, metrics, blue/green by `build_id`.

### 0.6 Resolved 2026-11-06 (second round) — see §0.7

1. World coordinator: **its own deployment**.
2. Auth: **email + password** at the login service. A **JWT** carries authentication between
   services, and the JWT **contains the `PlayerToken`**.
3. k8s ownership mechanism: **a StatefulSet**, confirmed by the Agones review (§0.7b′); the
   user's OK is pending.
4. Internal links go through **k8s Services**. Anything clients reach — login — gets an
   **Ingress**.
5. Super-region size: **S = 16 regions**, configurable (§0.7d).
6. Single-player / offline: **file stores, one `AllInOne` process**.

### 0.7 Design after the second round

**(a) Auth and the JWT.**
- **Login service:**
  - Accounts are email + password, hashed with Argon2id. Login is rate-limited and the
    endpoint is HTTPS-only behind the Ingress.
  - Each account owns one player, i.e. one `PlayerToken` and its `PlayerId`.
  - Login issues a short-lived **access JWT** (minutes) plus a refresh token that the login
    service stores and can revoke.
- **Signing:**
  - The JWT is signed **asymmetrically** (EdDSA / ES256). Region hosts and the coordinator
    hold only the public key (from a Secret / JWKS), so a compromised host cannot mint
    tokens.
  - Claims: `sub` = `PlayerId`, `player_token`, `session_epoch` (§0.4d), `exp`, `aud` = the
    game hosts.
- **Hosts:** the client's `Hello` carries the JWT instead of the bare token. The host checks
  the signature, `exp` and `session_epoch` against the pending `SpawnPlayer` from login. The
  JWT *is* the connect ticket, so no separate ticket is needed.
- **Consequence of putting the `PlayerToken` in the JWT:** JWT payloads are only signed, not
  encrypted — anyone holding the JWT (logs, proxies, browser storage) can read them. So the
  `PlayerToken` must **stop being a secret**: today possessing it *is* the identity
  (`FilePlayerStore::resolve`). In the multi-process game it becomes a stable,
  non-secret player identifier, and identity comes only from a verified JWT signature. Hosts
  must never accept a bare token again.
- Single-player keeps today's token-only `Hello`, because its trust model is the local
  machine. Encrypting the JWT (JWE) would avoid the change, at the cost of a more complex
  token for no gain once the signature is what is trusted.

**(b) Recommended k8s mechanism: a StatefulSet of region hosts, deterministic assignment,
Postgres fencing.** No Agones.

> **Reviewed 2026-11-06 — StatefulSet confirmed.** Agones was first dismissed here without a fresh
> review. A proper review then ran: current docs and releases, plus real-world use and OpenKruise
> Game. Raw findings with sources are in `specs/2026-11-06-agones-review/`
> (`01-agones-features.md`, `02-persistent-world-experience.md`). The verdict and its reasons are
> in (b′) below.

**(b′) Agones review: the result.**
- Facts are as of Agones **v1.62** (2026-11-04), which supports Kubernetes 1.34–1.36.
- The two load-bearing claims (#2286 refused, v1.62) were not re-checked by hand; the GitHub
  API was unreachable from here.

| Need | Agones | StatefulSet + per-pod Services | OpenKruise Game (OKG) |
|---|---|---|---|
| Stable host identity (ownership, host↔host DNS) | **None.** Ordinal names were requested and closed "not planned" (#2286, 2023). A crashed server becomes Unhealthy (terminal); a Fleet replaces it under a new random name, possibly on a new port. A standalone named GameServer is never recreated | **Ordinals + per-pod DNS**; a crash restarts the same ordinal | Ordinal-like ids, reservable ids, per-pod DNS |
| Client UDP ports / address | **Automatic**: Dynamic ports, two per server, address in status. On bare-metal k0s/k3s it falls back to the node's InternalIP unless the node has an ExternalIP (`--node-external-ip`) | Fixed NodePort per ordinal, generated by OpenTofu; `externalTrafficPolicy: Local` | HostPort / NodePort network plugins write the address back to the resource |
| Long-lived servers | Allocated / `Reserve(0)` stay until `Shutdown()` | Native | Native |
| Build upgrades | Fleet rolling updates **skip Allocated servers**, so you drain and shut down by hand. One report (Agones 1.39, Apr 2026) of an always-Allocated fleet stalling at half capacity; unverified whether 1.59's fix covers it | `updateStrategy: OnDelete` / `partition`, driven by our drain | In-place updates without recreating pods (its strongest feature) |
| Single-node drains (k0s/k3s upgrades) | `eviction.safe: Never` → a PDB that **blocks `kubectl drain`** on one node (inference from standard k8s behaviour) | No PDB needed; drain = SIGTERM → orderly exit | n/a |
| Assignment super-region → host | Needs the **coordinator in the assignment path** (members are dynamic). Counters/Lists (stable in 1.62, ≤ 1000 values) can *index* "who owns X", but do not stop two servers listing the same X | A rendezvous hash over ordinals, computed by everyone (same in dev); the coordinator only for pins | Same as StatefulSet |
| Persistent-world precedent | None in Agones' docs. Nitrado GameFabric runs "Persistent Servers" on Agones through its own named-instance layer, mapping not documented | The #2286 requester (Minecraft network) used StatefulSets | Documented MMO zone-server use, almost all Chinese studios |
| Footprint on one node | Controller ×2, allocator ×3 (LoadBalancer), extensions ×2, webhooks, an SDK sidecar per pod; minikube needs a narrowed port range; k3s/k0s not in its docs | Nothing beyond core k8s | Needs OpenKruise core, incl. a **privileged daemon per node**, with k3s fixes (kruise#1447); no k0s / minikube docs |
| Rust | Official `agones` crate, released with each version | n/a | n/a |

**Verdict: StatefulSet.**
- Agones solves one thing we would otherwise hand-roll: ports and addresses. For that it charges
  us the identity we need (super-region → host, host↔host DNS), a coordinator in the
  assignment path, a single-node drain blocker and a sizeable control plane.
- Its model is disposable session servers. The one persistent-world operator found had to
  build its own named-instance layer on top.
- OKG fits best on features but costs the most to run (privileged per-node daemon,
  cloud-oriented network plugins). It is worth revisiting only if in-place updates without
  pod recreation become a requirement.

Consequences:
- The Agones Fleet in `deploy/modules/world` is replaced in R5. The 2026-09-22 feasibility
  test stays valid as evidence, but is no longer the plan.
- **Build upgrades are a whole-world cutover**, not blue/green during play: client and server
  must run the same sim build, so every player reconnects with the new client anyway. The
  procedure: StatefulSet `OnDelete`, drain every host (orderly exit saves and releases), roll
  all pods, players log in again.
- **UDP exposure:**
  - per-pod NodePort with `externalTrafficPolicy: Local` (no source NAT; free on one node);
  - **Kubernetes ≥ 1.34** (also Agones' floor, and past kube-proxy's v1.32 UDP conntrack
    cleanup — stale entries otherwise black-hole a restarted pod's port for up to ~120 s);
  - the k0s/k3s node needs `--node-external-ip` (or equivalent) so the advertised address is
    the public one;
  - fallback if conntrack trouble shows up anyway: `hostNetwork` with port = base + 2·ordinal.
- **Autoscaling across many nodes does not need Agones** (§0.7e). Revisit Agones only if we
  ever want dynamic, non-ordinal host membership — and then the assignment moves into the
  coordinator.

*The original reasoning that follows is kept for the record; item 1's facts are superseded by
(b′).*

*Trade-off in prose.* There are four k8s-native ways to decide which pod runs which
super-region:
1. **Agones** (already in `deploy/modules/world` as a Fleet). It allocates game servers for
   *sessions*: its Ready → Allocated → Shutdown lifecycle and random pod names fit a match
   server, not a long-lived owner of a fixed piece of a persistent world. Its main gift,
   per-pod dynamic UDP ports, is replaced below by per-pod Services.
2. **A k8s `Lease` per super-region** (625 for a 100 km world at S = 16). This is dynamic
   takeover, but it means hundreds of renewals against the API server, and (§7) a `Lease`
   cannot fence a Postgres write — so it would duplicate the Postgres epoch it still needs.
3. **A custom operator with a CRD** for assignments. This is the most "k8s" option and the
   most work. It is only worth it once rebalancing needs to be smarter than a hash.
4. **A StatefulSet.** It gives each host a **stable identity** (`region-host-0..N-1`), a
   **stable DNS name** through a headless Service, ordered rolling updates, a PDB, graceful
   termination, and **at most one pod per ordinal**. Ownership becomes a pure function of
   `(super-region, replica count)` that every process computes the same way.

Option 4's cost: when a host dies, k8s restarts **the same ordinal** rather than letting
neighbours take over. That host's super-regions pause for a pod restart plus region restores
(seconds to tens of seconds), and the players there see a stall or reconnect. Faster takeover
(the `PgClaim` policy) can be layered on later, because fencing is in Postgres either way.

*Recommendation: option 4.* It is the simplest option that is genuinely k8s-native, and
development runs the very same assignment function:
- **Assignment:**
  - `owner(sx, sz) = rendezvous_hash(world_id, sx, sz, ordinal) → max over ordinals 0..N`.
  - When the replica count changes, only ~1/N of the super-regions move. Those are handed
    over: park, save, release the epoch; the new owner bumps the epoch and loads.
  - An optional override table in Postgres (`super_region_pins`) pins a hot super-region to a
    host (§4.1 rule 8).
- **Fencing:** a `super_region_owner(sx, sz, ordinal, epoch)` row is bumped by the owner on
  takeover, and every write checks it in the same transaction. This covers the cases a
  StatefulSet does not: a force-deleted pod on a partitioned node, or a paused process
  waking up.
- **Development:** `just` / compose start N processes with `VOXEL_HOST_ORDINAL=i` and
  `VOXEL_HOSTS=N`. That is the same function, so development exercises production's
  assignment. The relaxations are `AllInOne` (one process owns everything) and
  `VOXEL_OWNERSHIP=static:<file>` for hand-placed tests.
- **Other workloads:**
  - Login service: a Deployment (stateless, replicas ≥ 2) + ClusterIP Service + Ingress
    (HTTPS).
  - World coordinator: a Deployment with `replicas: 1` and a `Recreate` strategy. If it ever
    runs two replicas, use a single k8s `Lease` for leader election — the one place a `Lease`
    fits.
  - Postgres: CloudNativePG, as `deploy/` already has.

**(c) Networking.**
- **Internal (Services):**
  - login → coordinator: a ClusterIP Service.
  - login → *a specific* host, and host ↔ host: through the **headless** Service's per-pod
    DNS (`region-host-3.region-hosts.<ns>.svc`). A normal ClusterIP Service load-balances
    across pods, which is wrong when the target must be the owner.
- **Clients → login:** the Ingress (HTTPS).
- **Clients → region hosts: cannot use an Ingress.**
  - QUIC and WebTransport are UDP and must reach the *owning* pod end-to-end (§10). An
    ingress terminates HTTP/3 and balances across pods.
  - Use **one Service per host pod**, selected by the StatefulSet label
    `statefulset.kubernetes.io/pod-name`, as `type: NodePort` with fixed ports per ordinal
    (e.g. QUIC `30466 + 2i`, WebTransport `30467 + 2i`; generated by OpenTofu from the
    replica count) and `externalTrafficPolicy: Local`.
  - Later, with a load-balancer implementation (MetalLB / cloud), the same Services become
    `type: LoadBalancer`.
  - This is k8s-native, needs no Agones, and works the same on minikube, k0s and k3s.
  - Login answers with `{node public host, port}` for the owning host, and the hosts read
    their advertised ports from env.
- **WebTransport certificate:** a Let's Encrypt certificate for the node's hostname covers
  every port on it, as on today's box. Multi-node needs a name per node (cert-manager DNS-01,
  wildcard), or the cert-hash route.

**(d) Super-region size: S = 16 regions.**
- At the 256 m play regions that is 4 km square. A 100 km world has 25 × 25 = 625
  super-regions; the 16 km debug world has 4 × 4.
- Why 16 and not 8:
  - a fortress neighbourhood (3×3) straddles a super-region edge ~23 % of the time instead
    of ~44 %;
  - an adventurer crosses a host boundary every 4 km instead of every 2 km.

  Why not 32: a host's load moves in 4 km units, which is already coarse. With fortresses
  ≥ 1 km apart, one super-region holds at most ~16 fortresses (~144 regions), and per-region
  cost at 256 m is still unmeasured.
- Configuration:
  - **runtime config, not a build dial**: `VOXEL_SUPER_REGION_REGIONS` (default 16), plus a
    `world.super_region_regions` row that every process checks at startup, refusing to start
    on a mismatch;
  - expressed in **regions**, since the region size is itself a build dial.
- Changing S is cheap **because storage is keyed by region, not by super-region**. S only
  decides ownership, so changing it is a full restart that rebuilds the owner / epoch table,
  with no data migration.
- Revisit S after R3's host-load measurements.

**(e) Autoscaling across many nodes, without Agones** (added 2026-11-06 after the user asked).
*§0.10: no node autoscaling (a single server); host pod autoscaling becomes its own later plan (R6); KEDA is set aside.*
Everything Agones would have provided across nodes is small code on
[kube-rs](https://github.com/kube-rs/kube) (`kube` 5.0.0, 2026-11-01). Dynamic membership is
never needed: StatefulSet ordinals stay dense when scaling (scale-up adds `N`, scale-down
removes `N-1`).

There are two levels, and they are independent.

1. **Host count (pods): a StatefulSet `replicas` change.**
   - **Scale-up:** pod `N` starts and the rendezvous hash over `0..N+1` moves ~1/(N+1) of the
     super-regions to it. Each moving super-region is handed over: the old owner parks, saves
     and releases its epoch; the new owner bumps the epoch and loads.
   - **Scale-down:** k8s removes the **highest ordinal** with SIGTERM. Its orderly exit parks,
     saves and releases, and the survivors take its super-regions. With rendezvous hashing
     only that pod's super-regions move. `terminationGracePeriodSeconds` must cover the
     worst-case drain.
   - **Hosts must agree on `N`.** It is published in Postgres (`world.host_count`, bumped by
     whoever scales), not read from each pod's view of the StatefulSet. A host claims a
     super-region only after the previous owner's release, or after its lease lapsed. The
     epoch makes a race harmless, just slower.
   - **Who changes `replicas`:**
     - **(i) Existing autoscalers, no code.** An HPA on custom metrics (Prometheus adapter), or
       **KEDA** (Prometheus or PostgreSQL scaler on a load query), targeting the StatefulSet's
       `scale` subresource. Signals: tick-budget % and running regions per host, not CPU
       (§10). Use long stabilisation windows, because every scale step is a handover players
       can feel.
     - **(ii) Our own controller in Rust on kube-rs.** It can live in the world-coordinator
       deployment, which already holds world-level decisions and reads load from Postgres. It
       patches the `scale` subresource and writes `host_count`, and it can do what a generic
       autoscaler cannot:
       - scale *before* a predicted crowd (a pre-announced event, a busy embark);
       - avoid scaling while a super-region is mid-handover;
       - prefer off-peak scale-downs;
       - use the `super_region_pins` table to move one hot super-region to the new host
         instead of re-hashing.

       Leader election is not needed while the coordinator runs one replica; otherwise use a
       k8s `Lease` (the community `kube-leader-election` crate, 0.44).
     - **Recommendation:** start with (i) KEDA (configuration only) once there are several
       nodes. Write (ii) when handovers on scale steps prove too blunt — it is a few hundred
       lines, and the coordinator is its natural home.
2. **Nodes: an infrastructure autoscaler, not ours.**
   - Kubernetes' **cluster-autoscaler** adds a node when pods are Pending and supports many
     providers (Hetzner included; that is where the current box is).
   - **Karpenter** is the AWS/Azure-native alternative.
   - On a self-managed k0s/k3s server without a provider integration, adding a node is manual
     (join token), and the host autoscaler above still works within the nodes that exist.
   - Give region-host pods real CPU / memory requests (one region thread per core-ish), so
     Pending → node scale-up triggers correctly, plus a pod anti-affinity preference to spread
     hosts.

**What multi-node needs from us** (what Agones did for us; ~100–200 lines on kube-rs + RBAC
`get nodes`):
- **Advertised address:** the downward API gives the node name. The host reads the `Node` via
  kube-rs, takes its `ExternalIP` (or a per-node DNS name from a label) and publishes
  `{address, QUIC port, WT port}` to Postgres for login.
- **Ports:**
  - per-pod NodePort Services, generated per ordinal (OpenTofu up to a maximum host count, or
    created by the controller when scaling);
  - `externalTrafficPolicy: Local` means the client must dial the node the pod runs on, which
    the published address already ensures;
  - alternative: `hostNetwork` + port = base + 2·ordinal, unique cluster-wide.
- **WebTransport certificates:** one name per node (`node-k.<domain>`), each node's
  certificate from cert-manager DNS-01 (one wildcard `*.<domain>` works for all). Cert-hash
  pinning stays the fallback.
- **Node loss:** the pod reschedules onto another node with the same ordinal, so its
  super-regions resume at a new address. Login re-reads it from Postgres; clients reconnect.
  Its stall lasts as long as reschedule + restore (§0.7b's StatefulSet cost).

### 0.8 Plan summary and open questions (2026-11-06)

**The plan in one paragraph.**
- One forged world is split into a fixed grid of super-regions (16×16 regions, configurable).
- Each super-region is owned by one **region host** in a StatefulSet, chosen by a rendezvous
  hash over host ordinals and fenced by an owner epoch in Postgres.
- Players cross between hosts **seamlessly**, using the seam-transparent protocol (world
  tick, `EntityId`, T+1 transfers, halo, intents) over host↔host links.
- A central **login service** (email + password, JWT carrying the `PlayerToken`) places
  players and tells the owning host to spawn them.
- A singleton **world coordinator** holds the history, site claims and other world-global
  state.
- **Postgres** holds everything durable: regions in a portable format, players, history, and
  ownership.
- **Development:** docker compose runs Postgres; ownership, spawning and process count are
  relaxed there.
- **Production:** k8s — minikube, then a k0s/k3s test server, then multi-node. KEDA (later
  our own kube-rs controller) scales the hosts; cluster-autoscaler scales the nodes.
- **Single-player** stays one `AllInOne` process on file stores.
- Order: the seam plan (today) → R0 dev Postgres → R1 Postgres persistence on one host → R2
  role split → R3 many hosts + the seamless go/no-go → R4 takeover / failover → R5 k8s.

**Pending confirmation (recommendations already written):**
1. StatefulSet over Agones (§0.7b′).
2. Autoscaling: KEDA first, our own controller later (§0.7e).
3. The portable region format replaces the memory-image blob (which stays only as a
   same-build resume cache), and saves are cut at a world tick (§0.2, §0.5 R1).
4. ~~Fallback if the seamless go/no-go fails~~ **Resolved §0.9: seamless only, no fallback.** static super-regions with an in-game border
   mechanic (§1a.2) — still the fallback?

**Open: answer before the R0/R1 spec:**

5. **Resolved §0.9: one world per deployment, one logical database per world, shared login / database / panel.** ~~One world or many per deployment?~~ The game already has many worlds (`worlds/<slug>`).
   Does the cluster run one world, or several with world selection at login? This shapes
   every Postgres key (a `world_id` everywhere) and the login flow.
6. **Checkpoint interval**, i.e. how much progress a crash may lose. Today: 60 s.
7. **Resolved §0.9: no backups yet.** ~~Postgres operations:~~ CloudNativePG backups need an object store (S3-compatible:
   Hetzner Object Storage, MinIO, …). Which one, and what retention / PITR window?

**Open: answer before R2 (role split, login):**

8. **Email:** verification and password reset need an outbound mail provider (SMTP / API).
   Which one? Are verification and reset required at launch?
9. **JWT lifetime vs long sessions:** recommended — the JWT is checked only at connect /
   reconnect / cross-host handover, and revocation is a login → host kick. Keys are rotated
   through a JWKS endpoint.
10. **Internal call security** (login → host, coordinator ↔ host, host ↔ host): a
    NetworkPolicy alone, a shared service token, or mTLS?
11. **Coordinator downtime:** while it restarts, should hosts keep simulating, with world
    events (embark, history, journeys) paused and queued? (Recommended.)
12. **Personal data:** storing emails means GDPR duties — a privacy notice, account deletion
    and data export. What does deleting an account do to the player's body and records?

**Open: answer before R3 / R5 (scale, deployment):**

13. **Resolved §0.9: to be decided.** ~~Target scale:~~ expected concurrent players and world size. This sizes S, hosts per
    node, Postgres and the go/no-go thresholds.
14. **Resolved §0.9(d): the forged world is stored in the database, tiled, and pulled piecewise.** ~~World file distribution:~~ every host loads the forged world (~1 GiB resident for a
    100 km / 16 m world, §0.1), so N hosts cost N × 1 GiB. Ship it in the image, on a
    read-only volume, or fetch it from object storage at start? Can hosts load only their
    super-regions' part of it? (That would need a worldgen change.)
15. **Resolved §0.9: LISTEN / NOTIFY.** ~~LOD across hosts:~~ each host builds the 15 km rings for its clients from saved regions
    in Postgres (§8). Invalidate by polling `terrain_version`, or LISTEN/NOTIFY?
16. **Images and CI:** where images are built and which registry. Today's deploy rsyncs a
    binary.
17. **Resolved §0.9: behind the Ingress.** ~~Web client hosting in k8s:~~ the static site behind the Ingress, or keep the current
    Caddy box?
18. **Resolved §0.9: Prometheus + Grafana.** ~~Observability:~~ a Prometheus / Grafana stack in the cluster (the local Pyroscope /
    Grafana setup exists).
19. **Client UX:** a login screen (Slint, native + browser), a "reconnecting" state for host
    loss, and how a browser holds several WebTransport connections (one per host).

**Known risks to keep in view:**
- One overloaded region is still one core; nothing here splits it.
- Seamless cross-host crossing is unproven until R3's go/no-go.
- Per-region cost at 256 m is unmeasured, so host sizing and S are guesses until R1 / R3
  measure them.

### 0.9 Decisions, third round (user, 2026-11-06) and what follows

**Decided:**
1. **Seamless crossing only — no alternative.** The static-border fallback (§1a.2, §0.8 item
   4) is dropped. If R3's go/no-go misses, the work is to fix the seam, not to fall back.
2. **One world per deployment, one logical database per world.** Shared among all world
   deployments: the login service, the database server, the management panel, the web
   client and monitoring.
3. **No Postgres backups yet.**
4. **Target concurrent players: to be decided.**
5. **The world store must work on Postgres *and* in a file:** SQLite for single-player /
   single-server, so single-player needs no Postgres running.
6. **Worldgen runs up front from a cluster management panel.** The panel creates the world's
   database and seeds it with the generated world. **Servers pull only what they need from
   the database.**
7. **Terrain-change notification via LISTEN / NOTIFY.**
8. **The web client goes behind the Ingress**, deployed with everything else.
9. **Prometheus + Grafana.**

**What follows (recommendations):**

**(a) Deployment shape.**

| Shared (one each) | Per world |
|---|---|
| Postgres server (CloudNativePG; one cluster, one **logical database per world** + one for login, declared with CNPG's `Database` resources) | world coordinator (one replica) |
| login service + its database (accounts, sessions, the **world registry**: world id → database, coordinator Service, status) | region-host StatefulSet + per-pod NodePort Services |
| management panel | the world's database (on the shared server) |
| web client (static, behind the Ingress) | |
| Prometheus + Grafana (kube-prometheus-stack), KEDA, cluster-autoscaler | |

- Every world-scoped key lives *inside* that world's database, so no table needs a
  `world_id` column.
- A cross-world mix-up becomes a wrong-connection error, not a wrong-row bug.

**(b) Accounts across worlds.**
- An account (email + password) is global. A **player is per world**: an account has at most
  one player in each world, each with its own `PlayerToken` / `PlayerId`.
- Login flow gains a step: log in → pick a world (from the registry) → login issues a JWT for
  *that* world, with claims `world`, `player_token`, `session_epoch` and `aud` = that world's
  hosts.
- Hosts reject a JWT for another world.
- **Open:** may one account play several worlds at once (one session per world), or only one
  in total?

**(c) One storage layer, two backends.**
- A `WorldStore` holds regions, players, history, the forged world (below) and ownership. Its
  schema stays in the dialect subset both Postgres and SQLite share: integers, text, blobs
  (`bytea` / `BLOB`), no Postgres-only types.
- sqlx supports both. Two migration sets (`migrations/pg`, `migrations/sqlite`) kept in step
  by a test that applies both and compares the logical schema.
- Single-player: one **`<world>.sqlite`** file per world, replacing today's `terrain.world`,
  `.regions/`, `.players/` and `.history`. WAL mode; one writer per world, held by an OS
  file lock instead of epoch fencing.
- What SQLite lacks is harmless there: change notification is in-process (no LISTEN), and
  there is no pooling question.
- **Export / import is the same schema copied**, so the panel can import a single-player
  `.sqlite` world into a new Postgres database, or export one. A world is portable by design.
- Recommended because it is cheap: a `just` tool to convert the existing file worlds into
  SQLite once.

**(d) The forged world in the database, pulled piecewise.** This is the largest new work item.
- **Today:** a `WorldArtifact` is one zstd stream, and a server loads it whole: ~1 GiB
  resident for 100 km / 16 m (§0.1). That cost repeats in every host, so it does not scale to
  N hosts.
- **Proposed layout:**
  - **Global small tables:** world parameters, the geology column, lake bodies, river
    reaches, volcano sites, edges, biome-region records, and the `WorldMap` tile summaries
    the client gets on connect.
  - **Tiled rasters:** one row per raster per **forge tile** (e.g. a super-region square plus
    a halo of the cells bicubic sampling and river carving read across the edge), holding
    height, uplift, bedding, temperature, precipitation, drainage, volcanism, salinity, biome,
    savagery and region id.
  - **A coarse whole-world layer** (downsampled rasters) for LOD and the world map, which reach
    15 km and beyond — far past any host's tiles.
- **A host loads:** the global tables, the coarse layer, and the tiles of its super-regions
  plus their neighbours (for the halo and seam LOD). Memory then scales with territory, not
  world size.
- **Worldgen change needed:** `ForgedWorld` must sample through a tile cache instead of whole
  rasters, and produce exactly the same bits as today for any sample.
  - Pin it with a test: sample the whole world from tiles vs whole rasters, bit-equal.
  - Coast distance is recomputed on load today; it must become a stored tiled raster (or be
    computed per tile with a large enough halo).
- **Regions are not pre-generated.** The seed is the forge output; a region is generated from
  it the first time it runs and frozen then (freeze-on-first-run, §1a.3). Pre-generating
  ~150k regions of a 100 km world would cost hours and terabytes for land nobody may visit.
- **The forge runs as a k8s `Job` launched by the panel, not inside the panel.**
  - A 100 km forge needs up to ~12 GiB and minutes (`play::MAX_PLAY_FORGE_RAM_BYTES`), so it
    gets its own resource requests and can land on whichever node has room.
  - It writes straight into the new database. Single-player forges in the client, as today,
    into the `.sqlite` file.

**(e) Cluster management panel** (new, shared).
- It owns the world lifecycle:
  - **create:** pick forge parameters (today's generator screen as a web page) → create the
    database → run migrations → run the forge `Job` → register the world in the login
    registry → create the world's coordinator + StatefulSet;
  - **start / stop / scale** a world;
  - **delete / archive**;
  - **import / export** `.sqlite`.
- It creates k8s objects through **kube-rs** (the same crate as the autoscaler, §0.7e)
  rather than OpenTofu, since worlds come and go at runtime. OpenTofu keeps the shared
  platform: CNPG, KEDA, monitoring, the Ingress, the panel itself.
- **Open:** the panel's UI technology (web), and its access control (admin accounts in the
  login database with a role, behind the Ingress).

**(f) LISTEN / NOTIFY.**
- A channel is per database, so per world, which suits one database per world.
- The terrain write transaction sends `NOTIFY terrain_changed, '<rx>,<rz>,<terrain_version>'`.
  Each host keeps one **direct** (unpooled) listening connection.
- On reconnect, hosts re-sync with `terrain_version > last_seen`, because notifications are
  lost while disconnected (§8).
- Keep it off the hot checkpoint path. Through Postgres 18 a commit carrying NOTIFY takes a
  global lock (§8), so only debounced terrain writes notify.

**(g) Monitoring.** kube-prometheus-stack (Prometheus + Grafana + Alertmanager). Hosts,
coordinators and login export `/metrics` (`metrics` + `metrics-exporter-prometheus`): today's
`[server]` / `[region …]` counters, tick budget %, handovers, checkpoint latency, NOTIFY
lag. The KEDA scaler reads from the same Prometheus. Grafana dashboards live in git.

**(h) k3s or k0s? Recommendation: k3s, with k3d locally** (user asked 2026-11-06; from general
knowledge of both projects, not a fresh web review — verify versions when installing).

*Trade-off.* Both are single-binary, CNCF-conformant Kubernetes distributions that run on one
node and grow to several. They differ in what they bundle and in the ecosystem around them.

| Need | k3s (SUSE / Rancher) | k0s (Mirantis) |
|---|---|---|
| Ingress for login / web / panel | Bundled Traefik (can be swapped) | None bundled; install one |
| Per-pod client UDP (NodePort / LoadBalancer) | flannel CNI (portmap included); bundled **ServiceLB** makes `type: LoadBalancer` work on bare metal with no MetalLB | kube-router by default; hostPort worked in our Agones test; no bundled LB |
| Storage for CloudNativePG | Bundled local-path provisioner | Install one |
| Public node address | `--node-external-ip`, widely documented | Configurable; less documented |
| Hetzner + node autoscaling (§0.7e) | Mature community tooling (`hetzner-k3s`, `kube-hetzner`) with cluster-autoscaler integration | Generic cluster-autoscaler only; less ready-made |
| Local copy of production | **k3d** (k3s in docker), same distribution as the server | k0s in docker (what `deploy/envs/local` does today) |
| Upgrades | system-upgrade-controller | k0sctl / autopilot |
| What we already have | Nothing | `deploy/envs/local` (OpenTofu, k0s in docker) and the 2026-09-22 setup notes |

**Why k3s:**
- It ships the three things we would otherwise add by hand on day one: an ingress, a bare-metal
  LoadBalancer and a storage class.
- It has the ready-made Hetzner + cluster-autoscaler path.
- k3d gives local tests the exact distribution the test server runs. That is better parity
  than minikube, which can stay as an occasional second check.

**Cost:** move `deploy/envs/local` from k0s-in-docker to k3d. The node-creation part of the
OpenTofu roots changes; the manifests and modules do not.

**When k0s would win:** if a vanilla cluster with nothing bundled is wanted, or Mirantis
support. Neither applies here.

**Still open after this round:**
- Confirmations: StatefulSet; KEDA → own controller; portable region format + world-tick
  save cut.
- Checkpoint interval (crash loss window).
- Email provider; JWT lifetime / revocation; internal call security; coordinator-downtime
  behaviour; GDPR / account deletion.
- Images / CI and registry; client UX (login screen, world picker, reconnecting state, several
  connections).
- New this round: several worlds at once per account (b); the panel's UI and access control
  (e); k3s + k3d instead of k0s (h, recommendation).

### 0.10 Decisions, fourth round (user, 2026-11-06) — final state of this design pass

**Decided:**
1. **One Hetzner server runs everything** (the test server): k3s, the shared services and
   every world. **No node autoscaling**; cluster-autoscaler is dropped from §0.7e.
2. **Region-host pod autoscaling (up and down, to load regions) is wanted, but as a later
   plan of its own.** KEDA is set aside for now. R5 runs a fixed replica count per world, set
   in the panel.
3. **Everything else proposed so far is accepted:**
   - the StatefulSet (§0.7b′);
   - the portable region format + world-tick save cut (R1);
   - the §0.9 recommendations: accounts global / players per world, regions not
     pre-generated, the panel driving k8s via kube-rs, `NOTIFY` details;
   - k3s on the server, k3d locally.
4. **One account may play several worlds at the same time, but only once per world.**
   - Sessions are keyed `(account, world)`.
   - "Newest session wins" applies per `(account, world)`: a second login to the same world
     takes over and parks the first.
   - Sessions in other worlds are untouched.
5. **The management panel is a simple server-side-rendered website (htmx), served by Rust,**
   using our crates directly for worldgen and the like.
   - Recommendation: one Rust binary (axum + htmx templates) with a `forge` subcommand. The
     panel launches that subcommand as a k8s `Job`, so a 100 km forge (up to ~12 GiB) does not
     sit in the panel pod's memory limit.
   - On the single server, running the forge in-process is acceptable too, with the panel
     pod's limit raised.
6. **The user installs k3s on the server separately** (another context). This repo only
   targets it (kube context / kubeconfig), and k3d replaces k0s-in-docker locally.

**Updated roadmap** (supersedes §0.5's R5 and §0.7e's node half):
- R0 → R4 as in §0.5, with §0.9's storage layer: Postgres + SQLite, the forged world tiled
  in the database, the management panel creating worlds.
- **R5 — the single k3s server:**
  - shared: CNPG with one logical database per world, login, the panel (htmx), the web
    client behind the Traefik Ingress, kube-prometheus-stack;
  - per world: the coordinator + a region-host StatefulSet with a fixed replica count and
    per-pod NodePort Services;
  - local twin: k3d.
- **R6 — region-host pod autoscaling (separate plan):** scale on region load; drain the
  highest ordinal on scale-down; `host_count` in Postgres (§0.7e level 1). The mechanism
  (KEDA vs our own kube-rs controller in the coordinator) is decided in that plan.

**Left for the individual specs** (defaults in brackets; none blocks R0 / R1):
- Checkpoint interval [keep 60 s].
- Email provider for verification and reset [pick at R2].
- JWT lifetime [checked at connect / reconnect / handover; revoke = kick].
- Internal call security [k8s NetworkPolicy + a shared service token at R2].
- Coordinator downtime [hosts keep simulating, world events queue].
- GDPR / account deletion [design at R2].
- Image build + registry [decide at R5].
- Client UX: login screen, world picker, reconnecting state, several connections [R2 / R3].

### 0.11 Rate limiting overly active clients (design, 2026-11-06)

> **DEFERRED (user, 2026-11-06): not part of the initial k8s transition (R0–R5).** Kept as the
> design for a later plan. Its "When to build it" schedule below no longer applies.

**Two rules shape everything:**
1. **Limits live at the edges, never inside the sim.** Anything the sim decides must be
   decided identically by the predicting client. Dropping or delaying a message *before* it
   becomes a sim event is safe: the client just mispredicts and gets corrected. A limit inside
   the deterministic sim would be a desync risk.
2. **Limit as early and as cheaply as possible.** The kernel beats the QUIC handshake, which
   beats decoding, which beats the manager / region threads.

"Overly active" covers four different things, each limited at a different layer:
- connection churn (reconnect storms, many connections per IP or account);
- message floods on an open connection;
- bandwidth;
- *expensive* requests (LOD subscriptions, pathfinding orders, legends paging, person info).

**Today** (verified in code):
- The `Hello` has a size cap and a timeout (`HELLO_MAX_BYTES`, `HELLO_TIMEOUT`).
- WebTransport has a 10 s idle timeout. quinn runs `TransportConfig::default()`.
- Tick-stamped inputs are bounded in time (`MAX_INPUT_LEAD_TICKS`).
- **Nothing limits message rate per client**, and the edge → manager channel
  (`client_packet_send`) is **unbounded**, so a flood becomes memory growth on the server.
- **Decode-path hazard, fix first:** client → server packets use the same `decode_packet` as
  server snapshots. The decompressed-size cap is `MAX_DECOMPRESSED_PACKET_BYTES` (≥ 256 MiB,
  scaling with region size), so one small malicious frame makes the server allocate that
  much. Plain `bincode::deserialize` also has no limit on lengths inside a packet. Client
  frames need their own small cap (a client never needs more than ~64 KiB) and bincode
  options with a byte limit.

**Layers, outermost first:**

| # | Layer | What it stops | How |
|---|---|---|---|
| 1 | **Login service** (HTTP behind Traefik) | password guessing; ticket / connection churn | Traefik `RateLimit` middleware per IP (coarse). In-app token buckets per IP and per account on login, refresh and `SpawnPlayer` / JWT issuance — the counters live in Postgres or a shared cache, since login has ≥ 2 replicas. **This is the strongest connection limiter:** a host admits only a valid JWT that matches a pending spawn or a handover, so connection rate per account ≤ the rate login issues tickets |
| 2 | **Node firewall** (nftables on the k3s host, managed as code) | UDP floods and handshake storms per source IP, before any process sees them | In a hook that runs before kube-proxy's DNAT: limit new UDP flows per source IP and cap packets/s per source IP on the game NodePort range, generously above real play. Coarse — it cannot tell a good player from a bad one. Volumetric DDoS is upstream (Hetzner's network protection), not ours |
| 3 | **QUIC admission in the host** | handshakes from one IP / subnet; spoofed sources | quinn hands each attempt over as an `Incoming` *before* the crypto work: per-IP / per-/24 token bucket → `refuse()` / `ignore()`, and `retry()` (stateless address validation) when the source is unvalidated or the host is under load. Also cap concurrent connections per IP. For WebTransport, reject at the session request (`wtransport`'s incoming session / request API — verify its pre-handshake hooks; they are fewer than quinn's) |
| 4 | **Per-connection transport limits** | bandwidth, stream abuse | Set `TransportConfig` explicitly instead of the defaults: few concurrent streams (the game uses a handful), small per-stream and per-connection receive windows, datagram buffer cap, idle timeout. QUIC flow control then is the byte-rate limit: a server that reads slowly blocks the client with backpressure instead of buffering |
| 5 | **Per-connection message limiter in the edge task** (tokio, before anything reaches the manager) | message floods, expensive requests | A token bucket per connection **per message class**, with burst. **Bounded** per-client queue to the manager: when full, drop that client's newest non-input messages, never block the shared writer. Over-limit messages are dropped and counted. **Strikes:** sustained violation → disconnect + report to login, which refuses new tickets for that account for a cooldown that grows with repeats |
| 6 | **Per-account limits across hosts** | one account multiplying its budget over several host connections | A client holds one connection per host in its window. Only the **home** connection carries input, so non-home ("observer") connections get a near-zero input budget. Global per-account budgets (e.g. orders per minute across the world) live in login / coordinator, not in any one host |

**Message classes for layer 5** (starting budgets; named `pub const`s with an env override, per
the tunables convention):

| Class | Examples | Budget idea |
|---|---|---|
| Input | `PlayerInput` (tick-stamped) | ≈ 1 per tick (`1000 / TICK_RATE` /s) + burst for jitter; extra inputs for an already-filled tick are coalesced (keep the latest), not forwarded |
| Orders / UI | `Order` (designations, `MoveTo`, `SetMode`), `UiAction`, inventory clicks | ~10/s, burst 20. Pathfinding orders get their own lower budget, since each can spend up to `MAX_PATH_NODES` of A* |
| Queries | `RequestLegends`, `RequestPersonInfo`, `RequestEmbarkOptions` | a few per second (the client already self-paces person info at `PERSON_NAME_REQUESTS_PER_SEC`; the server must enforce, not trust) |
| Streaming | `RequestLod` / `ReleaseLod` | a cap on **outstanding** LOD keys per client (the ring table's maximum, ~4,400 at the 15 km horizon, plus margin) and a request rate; extra requests are refused |
| World moves | `PlayRegion`, `Embark`, `ReturnTo`, `StartAdventure` / `EndAdventure`, `Retire` | ≤ 1 per few seconds — each one parks and creates a body |

**Clients should self-limit to the same budgets** (shared constants in `game`), so honest
clients never hit the server limiter and never mispredict because of it.

**Observability** (§0.9g): a Prometheus counter per class for drops, refused handshakes
(per reason), strikes and disconnects. Alert on spikes. Grafana shows the worst offenders by
`PlayerId`.

**When to build it:**
- **Now, single process, before any public test:** the client-frame decode caps; layer 5
  (limiter + bounded per-client queues + strikes as disconnects); layer 4 explicit
  `TransportConfig`; layer 3 per-IP admission.
- **R2:** layer 1 (login limits, ticket issuance, cross-session strikes) and layer 6.
- **R5:** layer 2 (nftables rules as code next to the k3s setup) and the Traefik middleware.

## 1. TL;DR

1. **The partition unit already exists: the region** (1024² units, one `RegionRunner` actor,
   message-only seam). Do not invent a second unit. An *instance* (pod) owns a **set of
   regions**, recorded in a Postgres directory with a lease + fencing epoch. One region is
   simulated by at most one instance at any time — that single-writer rule is the whole
   consistency model.
2. **Ownership is dynamic and demand-driven, not a static grid**: an unowned region is claimed
   by the instance whose player needs it, so contiguous play areas cluster on one pod by
   themselves, and a border only exists where two populations actually meet. A static
   zone grid is the simpler fallback (§4).
3. **Crossing into a region owned by another instance = a transfer**: the source extracts the
   player's `EntityBundle` (already exists, already serde, already carries the inventory),
   writes it durably to Postgres, and sends the client a new `Transfer{addr, cert_hash, ticket}`
   packet; the client shows the loading screen and performs what is essentially today's join
   against the new instance. **Login is the same code path** ("transfer from nowhere").
4. **Durable format ≠ parking format.** The `Rollback` blob is 40.3 MiB raw / 1.15 MiB lz4 per
   region, >50 % of it derived colliders, and its bincode layout changes with every struct edit
   in `game` or the rapier fork. Persist **terrain + entity bundles** (both already portable
   formats with existing, tested load paths: `Region::from_columns` + `EntityArrived`), and keep
   the blob only as an optional same-build fast-resume cache.
5. **Never-edited regions store nothing.** Worldgen is a pure function; only edited terrain
   and entities need rows. (Policy switch for "freeze on visit" once worldgen stabilises — §6.)
6. **LOD is the part that does not shard naturally**: a client's rings reach 32 regions out —
   far beyond any one instance's territory. Instances therefore build LOD for *foreign* regions
   from worldgen + the persisted terrain rows of edited regions, with a NOTIFY-driven
   invalidation. This is what makes "Postgres is the shared truth" load-bearing (§8).
7. **Infra stays small for the first cut: k8s + Postgres only.** No NATS/Redis until seamless
   borders are wanted (stage 4). Pods need direct per-pod UDP reachability (QUIC +
   WebTransport), which rules out ordinary Services/Ingress (§10).
8. **Prior art agrees with the shape** (Appendix A): Albion Online runs exactly "zone change =
   loading screen + handoff through the database"; EVE uses a system→node directory with forced
   reconnect on remap; Minecraft's native Transfer packet is the same redirect; Second Life's
   child-agent connections are our optional L1. SpatialOS, Mammoth and Star Citizen's dynamic
   meshing are the evidence for *not* starting with seamless borders.
9. Staged path (§12): **0** persistence + identity on one instance → **1** directory, leases,
   two instances, transfer → **2** transfer benchmark → **3** k8s packaging, drain, autoscale →
   **4** (optional) observer connections / seamless borders.

## 1a. Decisions taken (user, 2026-09-20) — these supersede the recommendations below where they differ

1. **Dynamic assignment, but the claim unit is a *super-region*** (a fixed S×S square of
   regions), not a single region, so that region-to-region handoffs stay thread-to-thread as
   often as possible. Consequences: the directory table is keyed by super-region coords
   (`floor(rx / S), floor(rz / S)`), one lease row per super-region; *owning* a super-region is
   free — only its running regions cost — so S can be generous. `SUPER_REGION_SIZE` is a named
   tunable; starting point S = 8 (64 regions, ≈ 8 km square: a fully active one still fits one
   pod; S = 16 would not). *(2026-11-06: "≈ 8 km" was a slip — 512 m at the then 64 m regions,
   2 km at today's 256 m; see §0.2.)* A player near a super-region corner touches up to 4 of them.
2. **Seamless crossing between pods is the goal (§5 level L2); no loading screen.** A hard
   border that appears at arbitrary, moving places is **unacceptable for gameplay**, so the
   "L0 with dynamic claims" intermediate stage in the original roadmap is dropped. **Fallback if
   seamless proves infeasible: static assignment of super-regions to pods with a hard border on
   the fixed super-region grid, explained by in-game mechanics** (Option A policy on the same
   machinery). The super-region grid is therefore fixed from day one and shared by both
   outcomes; only *which borders are felt* differs. In the fallback, the border mechanic should
   exist on every super-region edge (even between two super-regions on the same pod), otherwise
   borders would still appear and disappear with assignment.
3. **Pristine policy = (b) freeze on first run**, because the world is player-editable and must
   never change under players. Every region that has ever run gets a terrain row, written at
   its first checkpoint; `regions.worldgen_version` + an `edited` flag let LOD keep using cheap
   worldgen sampling for a frozen region while `worldgen_version` is current and it is
   unedited, and read the stored row otherwise (§6.1, §8). The in-memory "edited regions" set of
   §8 becomes a "stored regions" set `{coords → (terrain_version, worldgen_version, edited)}`
   (~20 bytes per visited region). New worldgen only ever applies to never-visited land; the
   seam sits at the exploration frontier.
4. **Logout removes the body and stores it in Postgres; login inserts it** (§6.3 as proposed).
5. **Hosting: Agones, tested on local minikube first** (fallback test bed: the user's k0s
   server). **Production target is a single-node k0s cluster, possibly expanded later.**
   Consequences: several game pods must share one node, so a fixed `hostPort` per pod is out —
   Agones' Dynamic port policy (or a port range indexed by pod ordinal) is what makes pods
   co-exist on a node. And, stated plainly: **on one node, several pods add no capacity over
   one process** — the server already spreads region threads over every core. What pods buy on
   a single node is fault isolation, drain/restart of part of the world without full downtime,
   and an architecture already exercised when a second node arrives; what they cost is
   duplicated LOD/worldgen caches and pod↔pod traffic across borders that would be free
   in-process. So the single-node deployment should run **one game pod by default** (two when
   exercising the cross-pod path), and stage 0 (durable world, identity) is the part that
   matters most for that target. See §10a for the minikube feasibility test.

What decision 2 changes technically:

- **Prerequisite, independent of pods:** `TODO.md` still lists "seamless local-player crossing
  under latency" as open — input held across a seam oscillates the home region and needs a
  server-side input-routing / predicted-authority-transfer design. A cross-pod crossing cannot
  be better than a cross-thread one, so **this must be solved in-process first**. It is the
  single biggest risk to the seamless goal and it is a netcode problem, not an infrastructure one.
- The encouraging part: neighbouring regions are *already* asynchronous peers (own thread, own
  tick counter, own clock, TTL'd ghosts, idempotent arrivals). Moving a neighbour to another pod
  adds ~1 ms and new failure modes, **not a new consistency model**.
- **Client holds one connection per pod that owns something in its near window** (up to 4 at a
  super-region corner), with a `region → connection` routing table. A subscribe for a foreign
  region is answered with `RegionElsewhere{rc, addr, cert_hash, ticket}`; the ticket authorises
  an *observer* session on the other pod, later upgraded to home when the bundle arrives.
- **Pod↔pod link** (direct QUIC via quinn, pod IPs from `instances`; serde derives on the
  `RegionInput`/`RegionOutput` payload path): carries `GhostUpdates` (only entities within the
  32-unit margin of a cross-pod edge), `Departures`, and **input forwarding** for the window in
  which the client still sends input to the old home.
- **Departures go direct, persistence is asynchronous.** With one row per entity (§6.2) the
  no-duplicate/no-loss property does not need a commit on the crossing path; the loss window is
  the checkpoint interval, as everywhere else. (Durable-first remains the rule for the
  reconnect-style `Transfer`.)
- **`Transfer` (reconnect) stays, demoted**: login, drain/rebalance, and the fallback when a
  pod↔pod link or the neighbour pod is down.
- **LOD subscriptions follow the home lazily**: keep receiving from the old pod while that
  connection lives, re-request from the new home paced as today; the client already retires LOD
  roots only once a replacement is meshed, so there is no visual gap.
- **Go/no-go measurement** (replaces the transfer-time headline of §11 as the primary metric):
  correction magnitude and mispredict count around the flip tick, cross-pod vs cross-thread, on
  the existing `crossing.rs` harness shape and the client's reconcile metrics. Seamless is
  "done" when the two distributions are indistinguishable at p95 under injected 1–5 ms pod
  latency; if that cannot be reached, fall back per decision 2.

Revised stages: **0** persistence, identity, logout-extract → **0.5** in-process seamless
crossing under sustained input (the open TODO) → **1** super-region directory + leases, two
instances, multi-connection client, pod↔pod link, `Transfer` as fallback → **2** go/no-go
measurement → **3** k8s packaging → (fallback branch: static super-region policy + border
mechanic).

## 2. What exists today (verified in code, 2026-09-20 — largely stale, see §0.1)

| Fact | Where |
|---|---|
| Hub-and-spoke in one process: tokio edge → manager thread (`WorldManager`) → one OS thread per running region, crossbeam channels only | `crates/server/src/lib.rs`, `region_threads.rs` |
| `RegionInput`/`RegionOutput` is documented as "the future network seam". The envelopes are not serde, but **every payload in them is** (`GameEventKind`, `EntityBundle`, `GhostData`, `TerrainSnapshot`, `Rollback`, `LodKey`, `LodPayload`) | `crates/game/src/region_runner.rs:30-66`, `protocol.rs` |
| `WorldManager` is threadless, time arrives as `now_ms`, regions are spawned through a `RegionSpawner` trait (`InlineSpawner` for tests/wasm) → a multi-instance setup can be tested **in-process, deterministically** | `crates/game/src/world_manager.rs` |
| Parking lot is `BTreeMap<RegionCoords, SerializedRegion>` in memory; `TODO.md` lists "Durable region persistence" as open | `world_manager.rs:187`, `TODO.md` |
| Region restore paths that exist and are tested: blob (`RegionSeed::Parked/Deferred(Some)`), worldgen (`Fresh/Deferred(None)`), blob-corrupt → regenerate | `region_runner.rs:71-117` |
| `homes: ClientId → RegionCoords` is manager-authoritative input routing; survives disconnect but not restart | `world_manager.rs:199-201` |
| `ClientId = usize` from a per-process `AtomicUsize` starting at 0; no account, name, token, or client auth anywhere | `server/src/lib.rs:207,228`, `webtransport.rs` |
| Client holds **one** connection to a **hardcoded** `127.0.0.1:6466` (native) / `127.0.0.1:{port}` (wasm; port + cert hash fetched from `assets/webtransport-cert-hash.json` each attempt). No redirect packet in the protocol | `client/src/main.rs:198`, `netcode_web.rs:201-236`, `protocol.rs:236-271` |
| Client already treats **every region as an independent sim** with its own snapshot, event stream and clock; only the home region is predicted | `instance.rs`, spec 2026-07-07-home-only-prediction |
| Handoff: `scan_boundaries` → `Departures(Vec<(EntityBundle, target)>)` → manager rebases + `ensure_running(target)` + `EntityArrived`. 8-neighbour (Moore). Ghost margin 32 units, TTL 25 ticks; ghosts never wake a region. Arrival idempotent by `(source_region, source_key)` | `state.rs:928-1231`, `protocol.rs:92-182`, `world_manager.rs:437-472` |
| Known gap, quoted from the handoff spec: target region dies before consuming an arrival → **the entity is lost**; "durable persistence deferred" | `specs/2026-07-05-entity-region-handoff-design.md:202-218` |
| Terrain change detection is whole-region: `terrain_fingerprint()` every 25 ticks (1.25 s) → `TerrainChanged(TerrainSnapshot)` push → LOD invalidation. **No per-chunk dirty flag, no diff vs worldgen** | `region_runner.rs:255-272`, `state.rs:1233-1252` |
| LOD: sub-block factors (2/4/8) need the region's full-res chunks; factor ≥16 is coarse worldgen sampling + overlays of regions with authoritative terrain. `LodBuildJob::{Region, Pristine, Tile{overlays}}` | `lod_builder.rs:21-104`, `world_manager.rs:734-807` |
| Worldgen: `generate_region(rc, seed, materials)` pure; `WORLD_SEED` constant; disk cache keyed by crc32(seed ‖ block manifest ‖ worldgen sources) | `worldgen/src/cache.rs` |
| Determinism requires client and server to run the same sim build (vendored forks, libm) | CLAUDE.md |

Measured today (`cargo run --release -p worldgen --example snapshot_size`, region (0,0),
fresh from worldgen, this laptop):

```
40.28 MiB  whole snapshot (GameData)
18.43 MiB    ecs
 0.12 MiB      chunk blocks              <- the actual voxel truth
18.13 MiB      chunk chisel data         (4096 chiseled blocks x 4.5 KiB dense; highly compressible)
21.85 MiB    physics
20.87 MiB      colliders                 <- derived from voxels
 0.98 MiB      bodies
 1.15 MiB  on the wire (lz4), encode 40 ms, decode 49 ms
```

Previously recorded (`specs/2026-09-19-client-perf-findings.md`): zstd-1 brings the same blob to
0.35 MiB in 25 ms; region genesis from columns is ≤ 0.58 s (`generate ≤ 143 ms`, `build ≤ 433 ms`)
after the collider work; `Region::from_columns` alone 231 ms.

## 3. Constraints that shape every option

- **Single writer per region.** The sim is deterministic and authoritative; two instances
  ticking the same region is unrecoverable divergence, not a merge problem. Every mechanism
  below exists to make this impossible *even under partitions and pod kills* (→ leases with
  fencing, §7).
- **Sims are region-local.** An entity collides only with its own region's colliders; the
  neighbour exists only as TTL'd ghosts. So a hard border between two instances costs nothing
  physically — what is lost at a border is *visibility* (entities, fresh edits) and the ghost
  mirror, not correctness.
- **Region size is baked** (packed records, shaders, LOD math). One region is the smallest
  unit of load; a single overcrowded region cannot be split across pods. The scaling limit of
  this whole design is "one region's tick fits one core". Flagged, not solved.
- **A player costs regions, not just CPU.** Each isolated player pins a 3×3 near window = 9
  region threads + 9 × ~60–100 MB of state. Cluster size scales with how *spread out* players
  are. (`VOXEL_NEAR_RADIUS`-style dials and "no dynamic entities → skip physics" are the cost
  levers; out of scope here.)
- **View distance ≫ territory.** LOD rings reach 32 regions (≈ 33 km) out. No partition makes
  a client's view local to one instance. Near window: instance-local. Far view: global.
- **Same build everywhere.** Client/server determinism means a client can only play on a pod
  of its own build → rolling upgrades with mixed versions are not possible for the sim; the
  directory must carry a `build_id` and the front door must route by it (§10).
- **`game` stays engine- and IO-agnostic, sync, wasm-safe**; async is confined to edges. So
  Postgres access lives in `crates/server` behind traits defined in `game`, exactly like the
  `RegionSpawner`/`LodBuilder` injections.

## 4. Decision 1 — how the world is divided among instances **[DECIDE]**

The trade-off in prose first. There are two independent axes: *who decides the map from region
to instance* (fixed by configuration vs. negotiated at runtime), and *what the client is
connected to* (the owner of its home region, vs. a gateway that hides ownership). Fixed maps
are trivially understandable and need no coordination beyond liveness, but they balance load
badly — this world has one spawn point, so a static grid puts every new player on one pod
while most pods idle over empty ocean — and resizing the cluster means re-cutting the map.
Runtime negotiation needs a directory and leases (which the persistence layer needs anyway to
be safe), but lets territory follow players. On the second axis, a gateway makes borders
invisible and keeps instances private, but it inserts a hop into a rollback-netcode input path,
has to relay ~1 MiB snapshots, and needs a message bus between gateways and region hosts —
a lot of machinery whose only benefit is avoiding a loading screen the brief says is fine.

### Option A — static zone grid

`zone = (floor(rx / Z), floor(rz / Z))`, zone → pod by table or rendezvous hash over a
StatefulSet. Pods may own several zones so empty territory costs nothing.

- ✔ Simplest mental model; borders are fixed lines you can place (e.g. in oceans) and even
  signpost in-game. Matches EVE (system→node) and classic zone servers.
- ✘ Load follows geography, not players. Spawn-area hotspot lands on one pod by construction.
- ✘ Scaling the cluster = changing `Z` or the table → mass transfers.
- ✘ Still needs liveness + fencing for safe failover, so it saves less than it seems.

### Option B — dynamic per-region leases with locality (recommended)

Ownership unit = region. `region_owners` table in Postgres; an instance claims an unowned
region with a compare-and-set when one of *its* players needs it (near-window subscribe,
arrival, LOD never claims). Released on park (after a linger). An instance over budget stops
claiming, and an allocator rule sends the *player* elsewhere instead (§4.1).

- ✔ Territory follows population; a group exploring together stays on one pod with zero
  transfers, however far they walk. Borders appear only where two populations touch.
- ✔ No map to re-cut when pods are added/removed; drain = release + let others claim.
- ✔ Same lease machinery gives crash failover for free.
- ✘ Borders move, so they can't be dressed up in world design; a player may hit a border in
  an arbitrary place. Needs the "foreign region" client behaviour of §5 from day one.
- ✘ Pathology: two pods' territories interleave like a checkerboard if claims race in a busy
  frontier. Mitigation: claim in *blocks* (when claiming R, prefer/also claim its unowned
  neighbours — "claim the 3×3 you need, atomically, or none") and hand a contested frontier to
  whoever owns more of the surrounding 5×5.

### Option C — gateways + region hosts (the literal "network seam")

Clients connect to stateless gateway pods running the session half of `WorldManager`; regions
run on host pods; `RegionInput/Output` travel over a bus (NATS subjects per region) or direct
QUIC. `homes`, subscriptions and handoff relay become distributed state.

- ✔ No borders, no loading screens, no per-pod public UDP endpoints (only gateways are public).
- ✔ It is what the 2026-07-05 multi-region spec gestures at.
- ✘ Extra hop on every input and every echoed event; snapshot relay; a bus to operate; fan-out
  rebuilt; distributed `homes`. Largest blast radius of the three, and none of it is needed to
  meet the brief.
- Note B does not close this door: B's stage 4 (observer connections + cross-pod ghosts)
  converges on the same capability from the other side, without the gateway hop.

**Recommendation: B**, with A as the fallback if dynamic borders turn out to feel bad in play.
B and A share ~90 % of the work (persistence, identity, directory, transfer, client reconnect);
the difference is only the claim policy, so starting with B's machinery and a *static policy*
("claim only regions in my configured zones") is a legitimate first milestone.

### 4.1 Placement rules (B)

1. **Login**: front door reads the player's saved home region. Owned → send the client to the
   owner. Unowned → pick the least-loaded non-draining instance of the client's `build_id`
   (load = running regions and tick-budget %, both already measured per region in
   `region_threads.rs`), which claims on arrival.
2. **Near-window subscribe to an unowned region**: the home instance claims it (block claim).
3. **Near-window subscribe to a foreign region**: not subscribed; client shows it as LOD (§5).
4. **Departure into a foreign region**: transfer (§9).
5. **Departure into an unowned region**: home instance claims, in-process handoff as today.
6. **Over budget**: refuse new claims → rule 2 degrades to rule 3 and rule 5 to a transfer to
   the least-loaded instance, which claims. Territory sheds at the frontier, never mid-region.
7. **Rebalance / drain**: park region → save → release lease → players homed there receive a
   `Transfer` pointing at the front door. Forced loading screen; acceptable per brief (EVE has
   never had live remap without disconnect either).
8. **Hot region**: an operator- or policy-set `pinned_instance` on a region row lets one pod
   hold a crowded region (and little else) — EVE's "reinforced node". It is the only lever
   against the one-region-one-core ceiling short of changing the sim.

## 5. Decision 2 — what a player sees and feels at a border **[DECIDE]**

The client today subscribes its 3×3 near window on the one server it knows. With several
instances, part of that window can be foreign. Three levels, each a strict extension of the
previous:

| Level | Foreign near regions are… | Crossing | Cost |
|---|---|---|---|
| **L0 hard border** | rendered from LOD only (factor 2 at close range, no entities, no collision — none needed, sims are region-local) | loading screen ≈ a fresh join to the new instance | one `Transfer` packet + client reconnect. No cross-pod traffic at all |
| **L1 observer connections** | full detail: client opens a **second, read-only connection** to the foreign owner and subscribes those regions exactly as it does non-home regions today | near-instant: the target's regions are already loaded client-side; only the bundle moves | client multi-connection (per-region routing table `region → connection`); `RegionElsewhere{rc, addr}` reply to a subscribe. Still no pod↔pod traffic |
| **L2 meshed border** | as L1, plus cross-pod `GhostUpdates` so border entities mirror/collide like in-process neighbours, and `Departures` relayed pod→pod without touching Postgres first | seamless | pod↔pod channel (direct QUIC or NATS), serde on the seam envelopes, distributed `homes` hand-over protocol |

L0 is what the brief asks for and needs the least. **L1 is unusually cheap in this codebase**
because the client's per-region independence already exists — a region's snapshot/event/clock
stream does not care which socket it arrives on — and it removes most of the visual cost of a
border. Recommended: ship L0, measure, then L1. L2 only if borders in busy places prove to be a
gameplay problem.

Known L0 artefacts to accept knowingly: two players facing each other across a border cannot
see each other; an edit across the border appears after the LOD refresh path (§8), not live;
factor-2 LOD at point-blank range looks coarse. Block-claiming (§4 B) keeps borders away from
where people actually stand most of the time.

## 6. Decision 3 — what is persisted, in what shape **[DECIDE: pristine policy]**

Trade-off in prose. The zero-effort option is to write today's parking blob to a `bytea`
column: restore is already bit-exact and tested. But that blob is a *memory image* — bincode of
`GameData` including rapier's collider set, slotmap free lists and generation counters. Any
field added to a component, any change in the vendored rapier/parry forks, silently makes every
stored region undecodable, and the corrupt-blob fallback then **regenerates the region from
worldgen, wiping player builds**. That is acceptable for a parking lot that lives for minutes
and unacceptable for a world that lives for years. It is also 2–10× more bytes than the truth
it encodes, rewritten wholesale at every checkpoint. The alternative is to persist the two
things that are *already* designed as portable interchange formats — terrain columns/chunks and
`EntityBundle`s — and rebuild the rest with the existing genesis path (≤ 0.58 s/region, off
the manager thread). The rebuilt region is not bit-identical to the pre-save one (collider
handles, slotmap generations differ), and that is fine: a restored region has no subscribers,
and every subscriber starts from a fresh snapshot of whatever the server rebuilt.
(The perf-findings note that colliders are "not safely rebuildable" is about rebuilding them
*underneath live clients*; it does not apply to a cold restore.)

**Recommendation — three kinds of rows, one transaction per region checkpoint:**

1. **Terrain**, only for regions whose terrain differs from worldgen. v1: one row per region,
   `zstd(bincode(TerrainSnapshot))` with a format byte — estimated ~0.1–0.3 MiB, **not yet
   measured** (the whole blob incl. colliders is 0.35 MiB under zstd, so it is below that). Written only when the existing `TerrainChanged` push fires
   (debounced, e.g. ≥ 10 s apart), so an idle or merely walked-through region writes no terrain.
   v2 when write amplification shows up in heavily built regions: per-column rows
   (`rx, rz, cx, cz`), dirty set found by keeping the per-chunk crc vector that
   `terrain_fingerprint` already computes and then sums away.
2. **Entities**, entity-generic (players are entities): one row per persistent entity, keyed by
   a **global persistent id**, carrying its current region and a versioned `EntityBundle`.
   Players are entity rows with an `account_id`. See §6.2 for why rows and not a per-region list.
3. **Region meta**: tick, format/worldgen versions, `terrain_version` (bumped per terrain
   write; the LOD invalidation key), `saved_at`.

Optional 4: the lz4/zstd `Rollback` blob + `build_id`, as a fast-resume cache used only when
`build_id` matches. Skip until a measurement says restore latency matters; genesis is 0.58 s.

### 6.1 Pristine policy **[DECIDE]**

- **(a) Absent = pristine (recommended now).** Never-edited regions have no terrain row and are
  regenerated. Storage and write volume proportional to *edits*, not exploration. Hazard: a
  worldgen change re-rolls all unedited land while edited regions keep old terrain → cliffs at
  their edges. Worldgen is still moving in this project (continents re-cut pending), so this is
  the right default *during development*.
- **(b) Freeze on first run (Minecraft model).** Every region that ever ran gets a terrain row.
  Immune to worldgen changes; costs ~0.1–0.3 MiB × regions visited (10 000 regions ≈ 2 GiB —
  not a problem for Postgres) and a write on first visit.
- Make it a switch recorded in a `world` row next to `world_seed` and `worldgen_version`
  (the existing `WorldgenCache` key is already exactly that version hash). Flip to (b) at the
  point where wiping the dev world stops being acceptable.

### 6.2 Crash consistency of handoffs (why entity rows)

Regions checkpoint independently, so consider entity E walking R1→R2 between R1's checkpoint
(E present) and R2's (E present), followed by a crash: restoring both duplicates E and its
inventory. The reverse order loses E. With per-region entity *lists* this needs a coordinated
cut across regions. With **one row per entity keyed by persistent id, holding its region**,
duplication is structurally impossible — a row is in one place — and loss is avoided by one
rule: *a region checkpoint upserts the entities present and deletes only entities it saw
despawn; it never deletes by absence.* A departed entity's row is simply overwritten by the
next owner (guarded by a per-entity `seq` so an older checkpoint cannot clobber a newer one).
After any crash every entity resumes from its most recent checkpoint, wherever that was; the
worst case is losing up to one checkpoint interval of progress.

This needs a persistent id, which does not exist: handoff identity is
`(source_region, EntityKey)`, explicitly non-portable, and `GameData::arrivals` grows without
pruning. Proposal: `persistent_id: Option<u64>` on `EntityBundle`, allocated server-side from a
Postgres sequence in blocks; the sim treats it as opaque data. Exact placement (component vs.
runner-side side table) is a spec-phase question.

Cross-*instance* transfers tighten this further: the source commits
`{entity row → target region, seq+1, session cleared}` **before** sending `Transfer`, so the
bundle is durable before the client ever dials the target. This also closes the "target dies
before consuming the arrival → entity lost" gap the handoff spec documents today.

### 6.3 Logout

Today a disconnected player's body stays in its region and parks with it. Recommended with
persistence: **logout extracts the entity** (same `extract_entity_safe` path as a departure)
into its row; login is an arrival. One mechanism for login, logout, transfer and crash
recovery, and no orphan bodies pinning or haunting regions. (A linger timer against
combat-logging can be added later without changing the mechanism.)

### 6.4 Schema sketch

```sql
CREATE TABLE world (                      -- one row
  world_seed bigint, worldgen_version text, pristine_policy text, format smallint);

CREATE TABLE instances (                  -- liveness: ONE heartbeat per process
  instance_id uuid PRIMARY KEY,
  epoch       bigint NOT NULL,            -- nextval at startup: the fencing token
  build_id    text   NOT NULL,            -- sim determinism: clients must match
  addr_quic   text, addr_wt text, wt_cert_hash bytea,
  load        jsonb,                      -- running regions, tick budget %, players
  draining    boolean NOT NULL DEFAULT false,
  heartbeat_at timestamptz NOT NULL);     -- always now() of Postgres, never pod clocks

CREATE TABLE region_owners (              -- the directory
  rx int, rz int, PRIMARY KEY (rx, rz),
  instance_id uuid NOT NULL, epoch bigint NOT NULL, claimed_at timestamptz);

CREATE TABLE regions (
  rx int, rz int, PRIMARY KEY (rx, rz),
  tick bigint, terrain_version bigint NOT NULL DEFAULT 0,
  format smallint, saved_at timestamptz,
  terrain bytea,                          -- NULL = pristine; STORAGE EXTERNAL (pre-compressed)
  resume_blob bytea, resume_build_id text);

CREATE TABLE entities (
  entity_id bigint PRIMARY KEY,
  rx int NOT NULL, rz int NOT NULL,       -- index (rx, rz)
  seq bigint NOT NULL,                    -- monotonic per entity; guards stale writers
  account_id uuid UNIQUE,                 -- NULL for non-players
  session_instance uuid, session_epoch bigint,   -- who holds the live player, if anyone
  format smallint, bundle bytea NOT NULL);

CREATE TABLE accounts (account_id uuid PRIMARY KEY, name text UNIQUE, created_at timestamptz /* auth later */);
```

Every write from an instance is fenced in-transaction:
`… WHERE EXISTS (SELECT 1 FROM region_owners o JOIN instances i USING (instance_id)
WHERE o.rx=$1 AND o.rz=$2 AND o.instance_id=$me AND o.epoch=$my_epoch)`.

## 7. Decision 4 — ownership, liveness, fencing

Simplest robust option when Postgres is already the source of truth: **lease rows in Postgres,
fenced by an epoch, one heartbeat per instance.**

- Instance start: `epoch = nextval('instance_epoch')`, insert `instances` row.
- Heartbeat every ~2 s: `UPDATE instances SET heartbeat_at = now(), load = … WHERE instance_id
  = $me AND epoch = $my_epoch`. Zero rows updated → someone declared me dead → **stop ticking,
  drop clients, exit** (self-fencing).
- Region ownership rows reference `(instance_id, epoch)` and are valid only while that
  instance's heartbeat is fresh (TTL ~10 s). So 500 regions cost one heartbeat, not 500.
- Claim: `INSERT … ON CONFLICT (rx, rz) DO UPDATE … WHERE <current owner's lease expired>`
  returning whether it won. Atomic; block claims are one statement over several rows.
- Takeover of a dead instance's regions is just a claim; the dead instance's late writes fail
  the fencing predicate. A paused-then-resumed pod (GC-style stall, node freeze) finds its
  heartbeat rejected before its next checkpoint can land.
- Local safety margin: an instance that has not *succeeded* a heartbeat for `TTL − margin`
  pauses its region threads pre-emptively, so it never simulates on an expired lease.
- All lease arithmetic uses Postgres `now()` — one clock, no pod skew.

Why not the alternatives: session **advisory locks** die with the connection — they misbehave
behind transaction-pooling PgBouncer and turn a brief connection blip into loss of all regions.
**k8s `Lease` objects** put the truth about *world state ownership* outside the database the
fenced writes go to, so fencing can't be checked in the same transaction. **Consistent/
rendezvous hashing** is static placement (Option A) with nicer resize behaviour — useful for
A, irrelevant for B. Orleans/Akka-style placement directories are this same design with a
bespoke coordinator instead of a table.

## 8. LOD across instances

This is where a naïve shard breaks: today LOD for a running region is sourced from that
region's live terrain (`RequestTerrain` / `TerrainChanged`), and a client's rings cover
thousands of regions that will mostly be foreign or unowned.

Proposal — three sources, chosen per region, no pod↔pod traffic:

1. **Locally running region** → as today (live, 1.25 s cadence).
2. **Region with a terrain row** (edited, foreign or parked) → load `regions.terrain`, feed it
   into the existing `LodBuildJob::Region` / `Tile{overlays}` — the overlay model is already
   exactly "worldgen base + authoritative regions on top".
3. **Everything else** → pristine worldgen / coarse sampling, as today. This is ~all of the
   world, needs no coordination, and stays a pure local computation on every pod.

To know which case applies without a query per key, each instance keeps the **set of edited
region coords** in memory (tiny: one `(i32,i32)` + version per edited region), loaded at start
and kept fresh by `LISTEN terrain_changed` + a periodic full re-sync (NOTIFY is a hint, not a
log: it is lost while disconnected and does not pass through transaction-pooling PgBouncer, so
the listener needs one direct connection and the re-sync is the correctness backstop).
A notification → invalidate that region's LOD keys → rebuild for local subscribers, same as
`invalidate_region_lod` does today. Keep NOTIFY low-rate (it is, at one per debounced terrain
write): through Postgres 18 a commit containing NOTIFY takes a global lock that serialises
commits (fixed in 19, not GA at the time of writing), so it must never ride on the hot
checkpoint path — send it from the terrain-write transaction only. If that ever becomes a
bottleneck, polling `regions.terrain_version > $last_seen` every few seconds is a complete
replacement; the design does not depend on NOTIFY.

Freshness for far viewers on another instance becomes *terrain-write debounce (~10 s) + NOTIFY
+ rebuild* instead of 1.25 s. Acceptable for terrain kilometres away; with L1 observer
connections the adjacent foreign regions are live anyway.

Later, not now: LOD payloads are pure functions of `(LodKey, terrain_versions, worldgen_version)`
→ a shared payload cache (table or object store) or a dedicated stateless LOD-worker Deployment
would deduplicate the ~570-build join burst across pods. Measure first.

## 9. Front door, identity and the transfer protocol

**Identity.** Introduce `AccountId` (uuid) and make the in-sim `ClientId` a cluster-unique value
derived from it (today's per-process counter collides across instances, and `ClientId` is a
`usize` inside `GameData` — mind the wasm32 width hazard already noted for sentinels; make it
`u64`). Real authentication can come later; the *shape* (account → signed ticket → instance)
should exist from stage 0 because everything else keys on it. This also fixes the open TODO
"stable client identity across connections".

**Front door.** A small stateless HTTP service (`axum`, its own Deployment, ordinary Ingress):
`POST /login {account, build_id}` → `{addr_quic, addr_wt, wt_cert_hash, ticket}` using placement
rule 4.1(1). It replaces the static `assets/webtransport-cert-hash.json` the wasm client
already re-fetches on every connect attempt, so the web client's flow barely changes; the native
client gains the same fetch instead of its hardcoded address. `ticket` = signed
`{account_id, instance_id, epoch, expires}` (ed25519/HMAC); the instance verifies it without a
DB round trip, then loads the entity row and claims the session
(`session_instance/epoch`, takeover allowed only if the previous holder's lease is dead).

**Transfer** (L0), one new packet each way is enough:

```
A: region tick → Departures[(bundle, target)]           (exists)
A: manager: target owned by B (directory cache, confirmed in-txn)
A: TXN  entities[E] := {region=target, bundle, seq+1, session=NULL}   -- durable first
A: → client  ServerPacket::Transfer{addr_quic, addr_wt, cert_hash, ticket}
   client: loading screen; tear down world/subscriptions/LOD/reconcile state
           (the GameInstanceManager fields listed in the code survey); dial B
B: ticket ok → claim session → load entity row → ensure_running(target)
   → RegionInput::Event(EntityArrived(bundle)) → PlayerRegion(target)
   client: today's join from here on (3×3 subscribe, paced LOD requests, world_ready)
```

If B is unreachable or died, the client falls back to the front door, which re-places the
player (the region gets claimed by someone; the row is already durable). If A dies mid-way, the
row is either pre- or post-transfer; both are valid logins.

Client work is mostly *deleting assumptions*: address comes from a response instead of a
constant; "first `PlayerRegion` bursts subscriptions" must re-arm per connection; the join
timeline latches reset; the render bridge drops all regions. The wasm `?offline` path is
untouched (`InlineSpawner` + in-memory store).

## 10. Kubernetes shape

Specifics below come from the research pass in Appendix A/B (web sources dated up to
2026-09-20; items the pass could not confirm from a primary source are marked there).

- **Workloads**: `game-server` pods (the sim; stateful-in-memory, each directly reachable),
  `front-door` Deployment (stateless HTTP), Postgres via an operator (CloudNativePG is the
  active one; it also ships a `Pooler` CRD for PgBouncer — optional at tens of pods × 4–8
  connections). Nothing else for stages 0–3.
- **Per-pod UDP reachability** is the non-negotiable networking requirement: clients must dial
  *the* pod that owns their region, on QUIC (6466) and WebTransport/HTTP3 (6467). Nothing in
  stock k8s routes to a *specific* pod over UDP: LoadBalancer Services and NLB QUIC listeners
  balance across pods; Gateway API `UDPRoute` (GA in v1.6, 2026-06) is port-only with no
  matching; L7 ingress terminates HTTP/3 and breaks the end-to-end WebTransport session;
  Quilkin's token routing is not QUIC-aware. So: **node IP + hostPort, published by the pod**.
  - **Agones** (v1.60) does exactly this (Dynamic/Passthrough port policies, address published
    on the `GameServer`), lists persistent worlds as a supported pattern, and gives
    `eviction.safe: Never` + an auto PDB. Its Ready→Allocated→Shutdown lifecycle is
    session-shaped; for us "Allocated" only means "don't scale me down".
  - **Plain StatefulSet + `hostPort`/`hostNetwork`** is the lighter alternative when the
    hosting environment has routable node IPs; the instance publishes its advertised address
    into `instances` either way, so the choice is contained. **[DECIDE]** once the hosting
    target is known.
  - Direct per-pod endpoints also sidestep QUIC connection-migration-behind-an-L4-LB problems
    (QUIC-LB is still a draft).
- **WebTransport certificates**: browsers accept either a Web-PKI cert or
  `serverCertificateHashes` (self-signed, ECDSA P-256, validity < 14 days). The project already
  uses the hash route; per-pod certs minted at startup with the hash in the `instances` row and
  handed out by the front door means **no per-pod DNS or wildcard cert is needed**, and pods
  are reachable by bare node IP. *Risk:* support is uneven (Safari ≥ 26.4 works only if the
  server sends WebTransport flow-control settings; Firefox unconfirmed; an open W3C issue
  proposes removing the feature) → test `wtransport` against all three early, and keep
  "wildcard cert via cert-manager DNS-01 + per-pod DNS names" as the fallback (costs: DNS lag
  on reschedule, wildcard key on every pod).
- **Graceful drain** on SIGTERM (cloud-native rule: pods die any time): mark `draining`, stop
  claiming, checkpoint + park all regions, release leases, send clients `Transfer → front
  door`, exit. Order is preStop → SIGTERM → SIGKILL after `terminationGracePeriodSeconds`
  (default 30 s, preStop counts against it) → set it to worst-case drain + margin. A
  PodDisruptionBudget limits voluntary evictions, but **cloud providers honour a PDB only for
  a bounded time during node upgrades (1 h on GKE)** — a long-lived shard cannot opt out of
  being moved, so drain-and-transfer is a first-class path, not an edge case. A pod killed
  without grace costs ≤ one checkpoint interval of progress in its regions plus a reconnect —
  that *is* the failure model, so keep the interval short (30–60 s; entity rows are small).
- **Config from env** (12-factor): bind addresses, advertised address, `DATABASE_URL`,
  ticket key, claim policy/zone config, checkpoint interval. `127.0.0.1` binds must go.
- **Health**: liveness = process; readiness = heartbeat succeeding *and* not draining (so the
  front door's placement and k8s agree). Metrics: the existing `[server]` and `[region]` lines
  become Prometheus gauges (running regions, tick budget %, overruns, checkpoint latency, claim
  conflicts, transfers).
- **Autoscaling** signal = tick budget % and running-region count, not CPU (region threads
  sleep between ticks; CPU under-reports pressure until overruns start). Scale-up is cheap (a
  new pod just starts winning claims). Scale-down is a drain, i.e. loading screens — do it
  rarely and off-peak.
- **Upgrades**: because of sim determinism, treat a sim-affecting release as blue/green per
  `build_id`: new pods register with the new id, the front door routes each client to its own
  build, old pods drain as their players leave or at a cutoff. Two builds must never own
  regions of the same world at the same time *unless* the durable format is build-independent —
  one more reason for §6's portable format over the memory-image blob.
- **Postgres load** is modest by construction: steady state is one heartbeat/instance/2 s,
  small entity upserts per checkpoint, terrain rows only on edits, claims on frontier movement.
  Mechanics worth designing for: pre-compressed `bytea` → `SET STORAGE EXTERNAL` (skip the
  pglz attempt); TOAST values are never updated in place — rewriting a 1 MiB blob deletes and
  reinserts ~500 TOAST tuples and ≥ 1 MiB of WAL (1 000 regions × 1 MiB / 60 s ≈ 17 MiB/s WAL
  for the whole-blob design vs. near zero for edit-driven terrain rows — the quantitative case
  for §6); an UPDATE that doesn't touch the TOASTed column costs no TOAST work, so **leases and
  hot metadata live in their own tables**, as sketched; tune `toast.autovacuum_*` on `regions`.
  PgBouncer in transaction mode needs ≥ 1.21 with `max_prepared_statements` for sqlx /
  tokio-postgres, and can carry neither LISTEN nor session advisory locks.
- **Object storage** (S3/MinIO) for blobs is the generic advice above ~1 MB/row, but it splits
  the fenced write across two systems (needs conditional puts + a pointer flip). Start
  all-in-Postgres — the fencing check and the data land in one transaction — and move only the
  optional `resume_blob` out if backup/WAL volume ever says so.

## 10a. Agones feasibility test — **PASSED 2026-09-22 on k0s-in-docker** (blocked on minikube 2026-09-20)

2026-09-22: re-run on single-node k0s in docker (production distribution) instead of minikube:
two Agones GameServers with two Dynamic UDP ports each, both Ready on one node with distinct
ports, UDP echo from the host answered for both. Now the local deployment environment (OpenTofu
roots, `just deploy-local`) — see `2026-09-22-local-deployment-environment-design.md`, which also
lists the bare-k0s settings it needed. Original 2026-09-20 notes below.

Tooling is present on the dev box (minikube 1.38.1 docker driver, kubectl 1.30, helm 3.15,
docker 29.8; Agones chart 1.60.0 resolves from `https://agones.dev/chart/stable`). Creating a
separate profile (`minikube start -p agones-spike --keep-context …`, so the existing `minikube`
profile and the kubectl context are untouched) was refused by minikube:
`RSRC_DOCKER_STORAGE: Docker is out of disk space (/var is at 100%)` — `/` is 938 G with 1.7 G
free (docker: ~45 G reclaimable images, ~27 G build cache, ~15 G volumes; this repo's `target/`
is 87 G). The half-created profile was deleted again; nothing was pruned or forced. **Not a
verdict on Agones** — the test simply has not run.

The test to run once there is room (~5 GB is enough), chosen to answer the single-node k0s
question rather than just "does it install":

1. `minikube start -p agones-spike --driver=docker --cpus=2 --memory=3g --kubernetes-version=v1.35.1 --keep-context`
2. `helm install agones agones/agones -n agones-system --create-namespace --kube-context agones-spike`
3. Create **two** `GameServer`s from Agones' `simple-game-server` example (UDP echo, `portPolicy: Dynamic`)
   on the one node.
4. Pass criteria: both reach `Ready` with **distinct host ports on the same node address**, and
   `echo hi | nc -u -w1 <minikube ip> <port>` gets `ACK: hi` from each, from the host. On Linux
   with the docker driver the node IP is directly routable from the host, so this exercises
   the same nodeIP:hostPort path k0s would.
5. Then the project-specific half, which needs the stage 3 container image: QUIC + WebTransport
   (two UDP ports per pod) through Dynamic ports, and the pod learning its advertised
   address/ports from the Agones SDK to publish into `instances`.

If minikube stays impractical, the same five steps run unchanged on the k0s server (Agones
supports generic Kubernetes; k0s needs nothing special beyond the UDP port range being open on
the host firewall).

## 11. Benchmarking transfers

The `[perf]` join timeline already measures most of a transfer, because a transfer *is* a join
with a durable hand-over in front. Add events (per transfer, never per tick — same volume
contract as `perf.rs`):

| Side | Event | Meaning |
|---|---|---|
| A | `transfer_begin` | departure into a foreign region detected |
| A | `transfer_committed` | entity row durable (`db_ms=`) |
| A | `transfer_sent` | `Transfer` packet handed to the writer |
| client | `transfer_received`, then the existing `endpoint_up` → `connected` → `player_region` → `snapshot_received` → `first_near_mesh` → `world_ready` | re-armed per connection |
| B | `session_claimed` (`db_ms=`), `arrival_applied`, plus existing `region_genesis` (`parked=`/`stored=`), `snapshot_sent` | cold vs warm target |

Headline metric: **`transfer_begin` → `world_ready`**, reported as p50/p95 with the breakdown
DB commit / redirect+connect / target region start / snapshots / client meshing. Expectation
from existing numbers: the DB and connect parts are tens of ms; a **cold** target adds region
restore (≤ 0.6 s each, parallel per region thread); the client side (up to 9 new snapshots at
~1 MiB + meshing) dominates — i.e. a transfer costs about what a join costs today, and the
levers are the same ones already identified in the perf findings. Two transfer-specific levers:
**pre-warm** (A tells the directory "player approaching B's border" → B starts the regions
early; at L0 this is a row/NOTIFY, no pod↔pod link) and **L1 observer connections**, which
remove the snapshot+mesh cost from the crossing entirely.

Harness, in increasing realism:

1. **In-process, deterministic** (`crates/server/tests/` or `game/tests/`): two `WorldManager`s
   with `InlineSpawner`, sharing in-memory `RegionStore`/`Directory` fakes. Covers claim races,
   transfer, crash-restore (drop a manager, expire its lease, let the other claim), and the
   §6.2 no-dup/no-loss property under randomized kill points — same spirit as `random_ops.rs`.
2. **Two real servers + one Postgres on localhost** (`just pg-up`, `just run-cluster`), the
   headless client harness walking back and forth across a forced border N times;
   `just transfer-bench` reduces the `[perf]` lines to the table above
   (extend `scripts/perf_capture_report.py`). Compare cold vs pre-warmed target, lz4 vs zstd.
3. **kind/k3d cluster** for the real network path (host ports, front door, PgBouncer), then a
   real cluster for WAN numbers. Per the repo's own caveats: release builds, and compare ratios
   not seconds while on the clamped laptop.

## 12. Staged roadmap (2026-09-20 — superseded by §0.5)

Each stage is independently shippable and testable, and each would get its own spec → plan.

- **Stage 0 — durable single instance.** A `RegionStore` persistence trait in `game`
  (in-memory impl for wasm/tests; no directory yet), Postgres impl in `server` on a dedicated persistence
  thread bridged by channels (same pattern as the network edge). Portable format (terrain +
  entity rows), periodic + on-park checkpoints, restore path = `from_columns` + arrivals,
  `AccountId`/stable `ClientId`, logout-extracts. Closes two `TODO.md` items by itself and is a
  prerequisite for everything else. *Risk to retire first:* restore-by-rebuild equivalence
  test (save → restore → state hash of terrain+entities equal).
- **Stage 1 — two instances, L0 transfer.** `instances`/`region_owners`, heartbeat + fencing,
  claim policy (start with static zones = Option A policy on Option B machinery), manager state
  "claiming" next to the existing "stopping", `Transfer` packet, client reconnect, front door,
  foreign-region LOD sourcing (§8). Harness level 1 and 2.
- **Stage 2 — measure.** `transfer-bench`, pre-warm, checkpoint cost, Postgres write volume.
  Decide here whether L1 is needed and whether terrain needs per-column rows.
- **Stage 3 — k8s.** Container image, env config, SIGTERM drain, readiness, metrics, Agones or
  StatefulSet networking, CloudNativePG, blue/green by `build_id`, dynamic claim policy (B
  proper), autoscaling on tick budget.
- **Stage 4 — optional seamlessness.** L1 observer connections; then, only if needed, L2
  cross-pod ghosts/departures over direct QUIC (serde on `RegionInput/Output`) or NATS.

## 13. Risks and open questions

- **[DECIDE]** Option B (dynamic) vs A (static zones) as the target; B-machinery-with-A-policy
  as the first milestone either way?
- **[DECIDE]** Border level to aim for first: L0 only, or plan L1 into stage 1's client work?
- **[DECIDE]** Pristine policy now (absent = pristine) and the trigger for freeze-on-visit.
- **[DECIDE]** Logout extracts the body (recommended) vs bodies persist in the world.
- **[DECIDE]** Agones vs plain StatefulSet + host ports (depends on where this will be hosted).
- Persistent entity ids touch the sim's handoff identity — needs care with prediction
  (`matches_prediction` matches transfers on identity) and with the unpruned `arrivals` map.
- Chisel data dominates terrain bytes (4.5 KiB per chiseled block, dense); it compresses well
  but a sparser encoding would shrink rows, snapshots and LOD inputs alike.
- A single hot region is the hard scaling ceiling; nothing here raises it.
- Checkpoint cost inside a region tick: serialising terrain is 45–110 ms (the reason
  `terrain_fingerprint` exists). Checkpoints must snapshot cheaply in-tick and encode off-thread.
- Cross-region item flows other than entity handoff don't exist yet; when they do (trade,
  mail, containers spanning borders) they need the same single-row-ownership discipline.

## Appendix A — prior art

From a web research pass on 2026-09-20. Most claims rest on search-result summaries with a
handful of primary pages read directly; *(unverified)* marks what could not be confirmed from a
primary source. What it means for this proposal is in the right-hand column.

| System | How it partitions / moves players / persists | Takeaway here |
|---|---|---|
| **Albion Online** | ~600 clusters (~1 km²) statically assigned to game servers, one single-threaded sim each. **Every zone change is a loading screen and a handoff through the database, even between clusters on the same machine.** Separate world server for guilds/parties. Cassandra for game state, Postgres for accounts/market. Pain: large fights bounded by one thread per cluster. [slides](https://www.slideshare.net/slideshow/albion-online-software-architecture-of-an-mmo-talk-at-quo-vadis-2016-berlin/62724504), [forum](https://forum.albiononline.com/index.php/Thread/218858-How-Albion-Online-Tackles-ZvZ-Performance/) | Production proof of the brief's exact model (L0 + DB handoff). Same ceiling we flagged: one hot region = one core. We do better than Albion inside an instance (in-process handoff is already seamless). |
| **EVE Online** | Solar system = unit, many per node, directory-assigned; hot systems get dedicated nodes; planned fights pre-pinned to reinforced nodes. **No live remap without disconnect.** Jump = "session change", rate-limited. One SQL cluster. Overload → time dilation, never splitting a system. [HS](https://highscalability.com/7-sensible-and-1-really-surprising-way-eve-online-scales-to/), [TQ Tech IV](https://www.eveonline.com/news/view/tranquility-tech-iv) | Directory-assigned ownership + forced reconnect on remap is good enough for a 20-year MMO. "Dedicated pod for a hot region" is a placement policy worth having in §4.1. |
| **Minecraft Transfer packet** (1.20.5+) | Server tells the client to reconnect to `host:port`; client carries ≤ 5 KiB "cookies" to the next server; community practice is to sign them or use one-time tokens because the client is untrusted. Velocity: `accepts-transfers`, off by default. [protocol](https://minecraft.wiki/w/Java_Edition_protocol/Packets) | Same shape as §9's `Transfer{addr, ticket}`. Confirms: signed ticket, state travels server-side (DB), never via the client. |
| **MultiPaper** | Chunk-level, dynamic, first-come ownership tracked by a central Master; non-owners subscribe to the owner's changes peer-to-peer; players stay on the server they joined. Master persists Anvil files on its own disk. [how it works](https://multipaper.io/multipaper/how-it-works.html) | Closest relative of Option B + L2. Note what made it hard: fine-grained units and live P2P sync. Our unit is 1 km² and L0/L1 need no P2P. Our "Master" is a Postgres table, not a process. |
| **WorldQL / Mammoth** | N servers + a spatial DB over ZeroMQ; "sliced" (static borders, transfer with inventory) vs "seamless"; block changes stored as **deltas from the seed**. Incomplete, dormant. [post](https://worldql.com/posts/2021-08-worldql-scalable-minecraft/) | Precedent for "absent = pristine, store only edits". Also a cautionary tale for starting with "seamless". |
| **SpatialOS** | Central runtime owns state; workers hold per-component write authority handed over by a platform load balancer. Struggled: every change crosses the runtime (latency, cost), pricing, flagship titles shut down; no formal post-mortem found. [docs](https://documentation.improbable.io/spatialos-overview/docs/handing-over-write-access-authority) | The argument against Option C's hop-through-a-middle-layer as a *starting point*. |
| **Star Citizen** | Replication layer separate from game servers, entities persisted to a graph DB; **static** meshing live since Alpha 4.0 (Dec 2024); **dynamic** meshing still preview years later. Clients reportedly connect to the replication layer *(unverified)*. [wiki](https://starcitizen.tools/Server_meshing) | Static first, dynamic later — and dynamic/seamless is where the years go. Supports shipping B-machinery with a static policy first. |
| **BigWorld** | Cells with dynamically moved boundaries; entities are *real* on one cell and *ghost* on neighbours; handoff converts ghost → real. [mirror](https://kiddinglife.github.io/page3/) | Identical to the existing `GhostUpdate` / upgrade-in-place design → L2 is known-good architecture when wanted. |
| **Second Life / OpenSim** | One simulator per 256 m region; viewer keeps **child-agent connections to neighbouring simulators**; crossing upgrades child → root. Vehicle/attachment crossings are a notorious failure point (bundle transfer + timeout). [wiki](https://wiki.secondlife.com/wiki/Region_crossing) | This is L1 (observer connections), in production for two decades. The failure mode is non-durable bundle transfer under timeout — §6.2's durable-first commit is the countermeasure. |
| **Folia** | In-process region threading with merge/split and isolation rules. [docs](https://docs.papermc.io/folia/reference/region-logic/) | Already have the equivalent (region threads). |
| **New World / Dual Universe** | Only secondary/marketing-level detail found. | — |
| **Rust ecosystem** | No multi-process world sharding found in Veloren, Valence or lightyear. | No crate to adopt; build on the existing seam. |

Coordination sources: Kleppmann on why a lease needs a fencing token checked by the storage
layer ([post](https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html));
client-go's own disclaimer that k8s leader election does not guarantee a single actor
([pkg](https://pkg.go.dev/k8s.io/client-go/tools/leaderelection)); Orleans' default grain
directory allowing duplicate activations under churn
([docs](https://learn.microsoft.com/en-us/dotnet/orleans/host/grain-directory)); Akka Cluster
Sharding adding an external per-shard lease for hard single-writer
([docs](https://doc.akka.io/libraries/akka-core/current/typed/cluster-sharding.html)); session
advisory locks leaking/losing behind pooled connections
([write-up](https://www.snowinch.com/en/blog/postgres-advisory-lock-connection-pool-leak));
Google Slicer on directory-based, load-aware assignment over hashing
([paper](https://www.usenix.org/system/files/conference/osdi16/osdi16-adya.pdf)). A k8s `Lease`
remains the right tool for *one* thing later: electing a singleton rebalancer.

Postgres sources: [TOAST](https://www.postgresql.org/docs/current/storage-toast.html),
[NOTIFY](https://www.postgresql.org/docs/current/sql-notify.html) and its commit-lock problem
([Recall.ai](https://www.recall.ai/blog/postgres-listen-notify-does-not-scale)),
[PgBouncer prepared statements](https://www.crunchydata.com/blog/prepared-statements-in-transaction-mode-for-pgbouncer).
Voxel-store schemas in the wild are one upserted blob row per chunk: Luanti/Minetest's Postgres
backend `blocks(posX, posY, posZ, data bytea)`
([source](https://github.com/luanti-org/luanti/blob/master/src/database/database-postgresql.cpp)),
Vintage Story's SQLite `chunk(position, data)` — the shape §6's v2 per-column rows would take.

k8s sources: [Agones releases](https://agones.dev/site/blog/releases/),
[port policies](https://agones.dev/site/docs/reference/gameserver/),
[controlling disruption](https://agones.dev/site/docs/advanced/controlling-disruption/),
[Gateway API v1.6](https://kubernetes.io/blog/2026/08/03/gateway-api-v1-6-release/),
[QUIC-LB draft](https://datatracker.ietf.org/doc/draft-ietf-quic-load-balancers/),
[`serverCertificateHashes`](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/WebTransport),
[Safari cert-hash test](https://github.com/lidel/wt-safari-test),
[W3C removal issue](https://github.com/w3c/webtransport/issues/623).

Inter-pod messaging (only relevant from stage 4): prior art overwhelmingly uses **direct
server-to-server links** for simulation traffic (MultiPaper, Albion, BigWorld, Second Life);
brokers appear for chat/presence. Direct quinn links map 1:1 onto `RegionInput`/`RegionOutput`
and add no infrastructure; core NATS (`async-nats`) is the candidate if global fan-out (chat,
presence) ever needs a bus.

## Appendix B — crates (crates.io, 2026-09-20)

| Need | Candidate | Note |
|---|---|---|
| Postgres | `sqlx` 0.9 **or** `tokio-postgres` 0.7 + `deadpool-postgres` 0.14 + `refinery` 0.9 | sqlx bundles pool, migrations and a reconnecting `PgListener`; tokio-postgres is the lighter build. Either way behind a sync trait from `game`, on a persistence thread |
| Front door | `axum` | stateless; ordinary Ingress |
| Tickets | `ed25519-dalek` / HMAC | verify without a DB round trip |
| k8s | `agones` 1.60 SDK (if Agones), `kube` 4.2 (only if a rebalancer/operator appears) | game pods themselves should not need the k8s API |
| Metrics | `metrics` 0.24 + `metrics-exporter-prometheus` 0.18 | export the existing `[server]`/`[region]` counters |
| Compression | `zstd` for rows at rest (0.35 MiB vs lz4's 1.15 MiB on the region blob), keep `lz4_flex` on the wire | measured in `2026-09-19-client-perf-findings.md` |
| Later | `async-nats` 0.50, `tonic` 0.14, `object_store` 0.14 | stage 4+ only |
