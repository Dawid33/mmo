# Elastic region hosts on Kubernetes — design

**Date:** 2026-11-14
**Status:** draft from a design conversation with Dawid; not planned or scheduled. **It comes AFTER
R5** (Dawid, 2026-11-14): R4 (`docs/superpowers/plans/2026-11-13-r4-takeover.md`, in progress) and
R5 (k8s as designed in `2026-09-20-multi-instance-k8s-postgres-research.md` §0.7b/c: a StatefulSet of
hosts, per-pod Services, the login Ingress, CloudNativePG) are built first and unchanged; this is the
step that follows and replaces R5's StatefulSet with elastic hosts.
**Sources:** the code as of `a0ac55c1`+ (R4 Tasks 1–5 landed); a code assessment of moving regions
and a prior-art search, both of 2026-11-14 (summarised in §8).

## 1. What we want

- Region hosts are **ordinary, ephemeral pods**. A new one registers itself, is given part of the
  world, and runs it. Nothing about a host is special or numbered.
- Capacity follows load: more hosts when the world is busy, fewer when it is not, **none when
  nobody is online**.
- The world coordinator runs as **several replicas** with no single point that stops saves and
  takeovers for long.
- Removing a host never loses anything. An occupied host is removed only after **its land has been
  moved to another host, with ample warning to the players on it**.
- One coordinator deployment eventually serves **all worlds**.

Upgrades are out of scope for all of this: a new build is a stop of the world and a rejoin
(hosts have no version handshake). That is a maintenance window, by decision.

## 2. Decisions

1. **One Deployment (one pod) and one Service per region host.** Not a StatefulSet, not replica
   scaling. Each host has a stable in-cluster name (peer links, the login link) and its own UDP
   ports for clients; removing a host is deleting its Deployment, so *we* choose which host goes.
   A crashed pod is restarted by its own Deployment under the same address and, with R4, returns
   owning nothing.
2. **A host's identity is a fresh id per registration**, not an ordinal. A dead host never comes
   back as itself. Everything keyed by ordinal today (`hosts` rows, peer sessions, observer
   tickets, `departed`, the configured host count, the entry host) is re-keyed by host id.
3. **Hosts announce themselves through PostgreSQL** (the `hosts` row and its lease, as R4 builds
   them) with their addresses and pins. The coordinator sees a healthy, unassigned host there.
4. **The coordinator assigns land; hosts do not choose.** R4's on-demand claim by a rendezvous
   preference becomes "ask the coordinator", which answers with the least-loaded live host.
5. **Assignment is lazy, and idle land is released.** Land gets an owner when it becomes active
   (a player enters, a region must wake) and loses it when it has been idle and is committed in a
   cut. A new host fills from newly active land; load evens out without moving anything live.
6. **Who creates and deletes host Deployments: a reconciler, told what to do by the coordinator**
   (§4).
7. **The coordinator has no leader.** Every decision is a compare-and-set transaction in
   PostgreSQL that bumps an epoch the store checks; any replica may attempt it and exactly one
   wins. Timed work (start a cut, watch leases) is "claim it if it is due". A Kubernetes Lease is
   at most a hint, never the safety mechanism.
8. **Occupied land can be moved** (§5): first by a warned drain that makes clients rejoin, then by
   a short-freeze move in place with no reconnect.
9. **Correctness never depends on Kubernetes.** Fencing by owner epoch in PostgreSQL stays the
   only thing that stops two hosts writing the same land. Creating, deleting and scaling pods is
   an optimisation on top.

## 3. The pieces

| Piece | Kind | Does |
|---|---|---|
| Keycloak | Deployment | identity (exists) |
| Login service | Deployment, 2+ pods | accounts → characters, join tickets, sessions; links to the **coordinator**, not to "host 0" (exists; the link target changes) |
| World coordinator | Deployment, N interchangeable replicas | per world: history, claims, embark; sequences save cuts; watches host leases; **assigns land**; decides how many hosts should exist and which one drains; relays login's spawn orders and kicks to the host that holds the player |
| Host reconciler | Deployment, 1–2 pods | makes Kubernetes match the `desired_hosts` table: creates and deletes one Deployment + Service per host; nothing else |
| Region host | one Deployment + Service each | simulates the land it was given; serves clients over QUIC / WebTransport; peer links to other hosts; its part of each save cut; a lease |
| World PostgreSQL | CloudNativePG | the durable world, ownership epochs, leases, `hosts`, `desired_hosts` |
| Web bundle | static files | the browser client |

## 4. Who creates and destroys host Deployments

**The coordinator decides; a separate reconciler acts.**

- The coordinator writes rows: "host H should exist" when load calls for one, "host H is draining"
  when it should go. It never talks to the Kubernetes API and needs no cluster permissions.
- The reconciler is a small loop with a ServiceAccount allowed to manage Deployments and Services
  in the game namespace. For each desired host it creates the Service, reads back the ports the
  cluster gave it, then creates the Deployment with those ports as the host's advertised address.
  For a row that is gone it deletes both. It is idempotent: a crash half-way is repaired on the
  next pass, and everything it makes carries a label so strays can be found.
- It can be the same binary under another role (`VOXEL_ROLE=fleet`), as host and coordinator
  already share one.

Why not the coordinator itself: it would give game logic cluster-admin-shaped permissions, and it
would tie "can we save and take over" to "is the Kubernetes API reachable". The alternative
(coordinator replicas reconcile directly) is one deployment fewer and is workable because the loop
is idempotent; it stays open if the extra process proves to be a nuisance.

A first step needs no reconciler at all: a fixed set of host Deployments and Services declared in
OpenTofu, scaled by changing a number and applying.

**Scaling signals.** Each host reports load in its heartbeat row (players, running regions, tick
time headroom). The coordinator adds a host when the least-loaded host is above a threshold for a
while, and drains one when total load would fit in fewer hosts with room to spare (hysteresis and
a minimum host lifetime, so it does not flap). With nobody online it may drain every host; a login
for a world with no host makes it ask for one, and the join waits for the pod.

## 5. Removing a host, including an occupied one

A host marked draining gets no new land. Then, per piece of land it holds:

1. **Empty land** (no players, no subscribers): the regions park, a cut commits them, the host
   releases the land in that cut's decision and its fence shrinks. Nobody notices. This is the same
   mechanism as "release idle land" (decision 5).
2. **Occupied land, with warning.** Players standing on it are told, in game, that this area will
   move to another server at a stated time. The warning period is a tunable (`DRAIN_WARNING_SECS`;
   start with minutes, not seconds) and is shown as a countdown. At the deadline:
   - **Step A — warned drain (arrives with R4, nearly free).** The host commits a leave cut and
     exits; the coordinator gives its land to another host; clients rejoin by themselves and stand
     where they were. Nothing is lost (the cut is committed). Players see a reconnect of seconds.
   - **Step B — move in place (the target; new work).** One super-region at a time: its regions are
     frozen at a tick and committed in a cut; ownership passes to the taker inside that cut's
     decision under a new epoch; the taker loads the regions; players' sessions hop to the taker
     keeping their exact pose (their clients normally already hold a connection to it, or are told
     to open one before the deadline — the warning period is when that happens). Players see a
     pause of seconds and no reconnect. Messages for the land during the gap are held and delivered
     to the new owner.
3. When the host holds nothing, it exits; the coordinator deletes its `desired_hosts` row; the
   reconciler deletes the Deployment and Service.

With step B, "an occupied host is never removed" becomes "an occupied host is removed only by
moving its land first, after the warning". Step B also relieves a hotspot: one overloaded host can
hand a busy super-region to another.

What is **not** planned: a truly live move (ship the running region's memory, no cut, under a
second). It needs everything owed between hosts re-keyed from host to region and a new reliable
message carrying a whole region — estimated at the size of R4 or more — and the warning makes a
pause of seconds acceptable.

## 6. Failures and edge cases

| Case | Handling |
|---|---|
| Host dies | lease lapses; the coordinator reassigns its land from the last cut (R4); its Deployment restarts the pod, which registers as a new host |
| Host hung, then wakes | fenced at its next write; exits; restarted as a new host |
| Host dies mid-save | two-phase cut; staged parts resolved at start (exists) |
| Host dies while draining, during the warning | as "host dies": its players lose up to one save interval and rejoin; the move is abandoned |
| Taker dies during a move in place | the cut that would hand the land over is not decided or names a dead host: the land stays with the giver, which resumes it; retry with another taker |
| A coordinator replica dies mid-decision | a decision is one transaction: it either happened or not; another replica carries on from the database |
| All coordinator replicas down | hosts keep simulating; no saves, takeovers, assignment, scaling or moves until one returns |
| Reconciler down | no hosts created or deleted; everything running keeps running |
| Kubernetes API down | as "reconciler down" |
| Kubernetes evicts a host pod (node pressure, node drain) | SIGTERM → leave cut within the termination grace period; a PodDisruptionBudget per host allowing no voluntary disruption makes a planned node drain wait for us to empty the host first |
| New host registers, nothing to give it | it idles with a lease; removed again if still unused after the minimum lifetime |
| Login for a world with zero hosts | the coordinator asks for a host; the join waits (bounded) and reports "world starting" |
| Two hosts told the same land | impossible by fencing: the assignment is an epoch bump in one transaction; the loser is fenced |
| A crowded super-region | one host, one thread per region: more hosts do not help; a move in place (step B) can move it to a less loaded host, but not split it |
| Upgrade | stop the world, deploy, rejoin |

## 7. Order of work (after R4 and R5)

R5 ships first as designed: a StatefulSet of hosts numbered by ordinal, one Service per pod, the
coordinator as one replica. Everything below starts from that running system.

1. **E1 — hosts as individual Deployments, fixed set.** Host identity by id instead of ordinal;
   discovery through `hosts`; a Service and advertised UDP ports per host; the login link goes to
   the coordinator; OpenTofu declares N hosts in place of the StatefulSet. Step A (warned drain)
   works here: it is R4's drain plus an in-game warning.
2. **E2 — coordinator without a leader**, several replicas; world id on every link (so one
   deployment can serve several worlds later).
3. **E3 — the coordinator assigns land; idle land is released** (the empty-land move: release in
   a cut decision, fence shrink, peers told). Estimated 4–6 tasks.
4. **E4 — `desired_hosts`, the reconciler, load reporting and scaling decisions; scale to zero.**
5. **E5 — move occupied land in place (step B).** Estimated 10–14 tasks: a re-home that keeps the
   pose, client re-mapping of observed regions (shared with R4 Task 16), gating against an
   in-progress cut, the warning and countdown UI, the failure table above as tests.
6. **E6 — consolidate PostgreSQL into two CloudNativePG clusters, never one** (the final step;
   Dawid, 2026-11-14/15; §10): a new management instance takes Keycloak's, the login service's and
   chat's databases; R5's `pg`, the region instance, is unchanged.
7. **Later, not part of this plan — one coordinator deployment for all worlds.** Needs the
   process-global forged world untangled if the coordinator depends on it (not checked).

**What R5 can do to make this cheaper, without changing its scope:** take every host's advertised
client and peer address from the environment (never derive it from the ordinal); keep the per-pod
Services generated from a list so a list of hosts can replace a replica count; put the world id in
the link hello when the link protocol version is next bumped.

## 8. What this rests on

**Code (2026-11-14):** a player crossing from one host's land to another's is already seamless
when the client holds a connection to the neighbour (`peers.rs` `hand_off_to_peer` /
`accept_handoff`, `HomeHop`). A running region serialises whole and restarts bit-identically, but
only inside one process (`SerializedRegion`); no peer message carries a region. `release` and
`claim_free` exist; a fence never shrinks; `Owns` only announces a claim. Peer queues, R4's journal
and in-flight cut rows are keyed by host ordinal — the main obstacle to a truly live move. The R4
plan and the research spec had both deferred live moves and accepted a reconnect for rebalancing.

**Prior art (web search, partly from secondary sources):** nobody found moves a running region
live; they hand entities across a boundary (BigWorld, EVE at downtime, Star Citizen's replication
layer) or stop the unit, persist it and restart it elsewhere under a new owner (Akka/Pekko
sharding, Orleans grain migration) — the freeze → persist → new epoch → resume pattern that step B
follows. Agones and OpenKruise Game target many interchangeable servers; no persistent-world case
study was found, and Agones' protection of occupied servers also stalls rolling updates. QUIC
cannot follow a server to a new address, so a moved region always means the client talks to the
new host — which the multi-connection client already does. No UDP proxy with QUIC / WebTransport
support was found (Quilkin: unverified, probably unsuitable). Container checkpoint/restore is not a
realistic migration path. A Kubernetes Lease alone gives no fencing, which is why decision 7 leans
on PostgreSQL.

## 9. Open questions

- **Client UDP on one node:** a node port pair per host Service, or host networking with ports the
  pod picks and registers. Several nodes: one public address per node, or a load balancer per host.
- **The WebTransport certificate** for many hosts on one hostname (covers every port) versus a name
  per node later.
- **Where placement lives** once login links to the coordinator: today the entry host places
  players (`world_manager/placement.rs`).
- **How much of R4's ordinal-keyed design to revisit** (one heir per predecessor, the configured
  host count, `Static` ownership as the escape hatch) and when, without disturbing R4 while it is
  being built.
- **The warning period** and what a player can do during it (nothing; or walk off the land to avoid
  the pause).
- **Scale-to-zero cold start:** how long the first login of a quiet world waits.

## 10. Where this is heading: one management cluster, one cluster per region (Dawid, 2026-11-15)

Far off and possibly never — there is one cluster today and no budget for a second — but it decides
how things are grouped now.

| | Management cluster (the current one; serves clients worldwide) | Region cluster (one per geographic region, later) |
|---|---|---|
| Runs | Keycloak, the login service, the website and web client, chat, observability, the image registry's pull user | the world coordinator, a world-management web console, region hosts (and the host reconciler) |
| PostgreSQL | **CloudNativePG cluster "management"**: Keycloak's, the login service's, chat's databases | **CloudNativePG cluster "region"**: one database per world |

**Consequences to hold to from now on, while everything shares one cluster:**

- **Two CloudNativePG clusters, not one.** R5's `pg` cluster (namespace `postgres`, a database per
  world) IS the region instance: nothing of the management side goes into it. The later consolidation
  creates the second, management instance and moves Keycloak's, login's and chat's StatefulSet
  databases there. The two never share a cluster, a role or a backup target.
- **Nothing on the region side may read the management side's database, or the reverse.** The only
  ties are the ones that already cross a network boundary: hosts fetch the login service's public
  keys over HTTPS; the login service links to a world (the coordinator, today the entry host) with
  the service token and a pinned certificate; the login service's world registry names a world's
  addresses.
- **Those ties must work across clusters.** A world's link address and client endpoints in the
  registry are names that can become public ones — never something only meaningful inside one
  cluster's DNS when a second cluster exists. R5 already moved links from IP addresses to names.
- **A world belongs to one region cluster**; the account → character mapping stays in the login
  service (management), keyed by world.
- **Observability stays central**: a region cluster ships its metrics and logs to the management
  cluster; it does not grow its own stack.
- **The world-management web console is a new piece** on the region side (not designed here).
- The coordinator "for all worlds" (step 6 above) means all worlds *of its region cluster*.

