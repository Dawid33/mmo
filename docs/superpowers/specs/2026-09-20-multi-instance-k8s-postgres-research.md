# Multi-instance servers on Kubernetes with a shared Postgres world — investigation & proposal

Date: 2026-09-20. Status: **research + proposal, nothing implemented.** Decisions marked
**[DECIDE]** are the user's; everything else is a recommendation with its reasoning.

Goal as stated: run several server processes for *one* game world on a k8s cluster, with
chunk/world data saved to and loaded from a shared Postgres cluster. A loading screen when a
player moves between instances is acceptable. The main question is how to subdivide the world
and the work; benchmarking the move between instances is secondary but wanted.

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
   pod; S = 16 would not). A player near a super-region corner touches up to 4 of them.
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

## 2. What exists today (verified in code, 2026-09-20)

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

## 12. Staged roadmap

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
