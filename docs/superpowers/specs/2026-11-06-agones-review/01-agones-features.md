# Agones features vs. a StatefulSet for super-region hosts (research, 2026-11-06)

Confidence tags: **[P]** confirmed from a primary source (agones.dev docs, GitHub release notes, crates.io).
**[S]** secondary source. **[I]** my own inference from primary facts. **[U]** unverified.

Caveat: agones.dev's default docs pages still show some features as "Beta" (e.g. Counters and Lists). The v1.62.0 release
notes and the feature-stages page promote them, so the page text lags the release in places. Where they disagree, I follow
the release notes.

## 1. Version, cadence, Kubernetes support

- **Latest release: v1.62.0, published 2026-11-04** [P] (GitHub release page; the crates.io `agones` crate 1.62.0 is
  dated 2026-11-04T08:08Z).
  https://github.com/googleforgames/agones/releases/tag/v1.62.0 , https://crates.io/api/v1/crates/agones
- **Cadence: about every 6 weeks** [P], from the crates.io / GitHub dates: 1.54.0 2025-12-03, 1.55.0 2026-01-13,
  1.56.0 02-24, 1.57.0 04-07, 1.58.0 05-20, 1.59.0 07-01, 1.60.0 08-12, 1.61.0 09-24, 1.62.0 11-04.
- **Supported Kubernetes: 1.34, 1.35, 1.36** for 1.60, 1.61 and 1.62 [P]. Earlier releases: 1.54 = 1.32–1.34,
  1.57 = 1.33–1.35. The docs say "Other versions may work, but are unsupported."
  https://agones.dev/site/docs/installation/
- **Changes since v1.60** [P]:
  - v1.60: `PortPolicyNone` promoted to Stable.
  - v1.61 (2026-09-24): the `gameservers.lists.maxItems` setting now reaches the SDK server (fix). During Fleet
    rolling updates, active GameServerSet replicas are clamped at 0.
  - v1.62 (2026-11-04):
    - **CountsAndLists promoted to Stable**: always on, and setting the gate to false no longer disables it.
    - **PlayerTracking removed** from the SDKs, examples and docs.
    - New alpha `RollingUpdateSurge` gate (details not read [U]).
    - **BREAKING**: Fleet `maxSurge` / `maxUnavailable` percentages now need a trailing `%`; a bare `25` means 25
      GameServers.
    - Fix: one GameServer could get the same host port twice. All of a GameServer's ports now come from a single node.
    - SidecarContainers: a GameServer whose container crashes before Ready is now marked Unhealthy.
    - Fixed rate-limiter exhaustion in the Succeeded, MissingPod and Health controllers.
  - Earlier stable promotions, from the feature-stages page: `PortRanges` (1.59), `RollingUpdateFix` (1.59),
    `AutopilotPassthroughPort` (1.54).
  - Beta gates still listed: `ScheduledAutoscaler`, `FleetAutoscaleRequestMetaData`, `SidecarContainers` (Beta since
    1.56, on by default).
  - Alpha gates: `WasmAutoscaler`, `RollingUpdateSurge`.
  https://agones.dev/site/docs/guides/feature-stages/ , https://github.com/googleforgames/agones/releases

## 2. Counters and Lists

- **Stage** [P]: Beta since 1.41.0 (gate `CountsAndLists`, on by default), Stable in 1.62.0.
  https://agones.dev/site/docs/guides/counters-and-lists/ , v1.62.0 release notes
- **What they store** [P]:
  - Counters: named int64 `count` + `capacity`. The default capacity is 1000; the maximum is int64, though the docs advise
    against using max int64.
  - Lists: named lists of string `values` + `capacity`.
  - Keys must be declared in `GameServer.spec.counters` / `spec.lists` at creation and cannot be added or removed later.
  https://agones.dev/site/docs/reference/gameserver/
- **List size limit** [P]: capacity 0–1000. Helm `gameservers.lists.maxItems` (default 1000, added in 1.55) replaced the
  hard cap. ~625 super-region ids fit under the default.
  https://agones.dev/site/docs/installation/install-agones/helm/
- **SDK updates from the running server** [P]: the server can update its own lists and counters.
  - Lists: `AppendListValue` (errors on a duplicate or at capacity), `DeleteListValue`, `SetListCapacity`,
    `GetListValues`, `ListContains`, `GetListLength`.
  - Counters: Get/Set/Increment/Decrement, Get/SetCapacity.
  - Updates are **batched about once per second and eventually consistent**. The allocator may see a change about 1 s
    late.
  https://agones.dev/site/docs/guides/client-sdks/ , https://agones.dev/site/docs/integration-patterns/high-density-gameservers/
- **Allocation filtering** [P] (`GameServerAllocation.spec.selectors[]`, an ordered list where the first match wins):
  - Each selector takes `matchLabels` / `matchExpressions` and `gameServerState: Ready|Allocated`.
  - `counters.<key>`: `minCount`, `maxCount`, `minAvailable`, `maxAvailable`.
  - `lists.<key>`: `containsValue`, `minAvailable`, `maxAvailable`.
  - `priorities[]` (`Counter|List`, `key`, `Ascending|Descending`) sort the candidates. Under Packed they only break
    ties; under Distributed they sort the whole set.
  - The allocation can mutate the chosen server:
    - counters: `Increment|Decrement` by `amount`, or set `capacity`;
    - lists: `addValues` (duplicates ignored), `deleteValues`, `capacity`;
    - `metadata` labels and annotations are patched on.
  - **Caveat**: a mutation that would exceed capacity still allocates but silently skips the update. Guard it with
    `minAvailable`.
  https://agones.dev/site/docs/reference/gameserverallocation/
- **Can they model "GameServer owns these super-regions"?**
  - **Lookup, yes** [P]+[I]. Each host keeps `lists.superregions` with the ids it owns. "Who owns X" is a
    GameServerAllocation with `gameServerState: Allocated` and `lists.superregions.containsValue: "X"`. It returns that
    GameServer's address and ports and stamps `agones.dev/last-allocated`; an already-Allocated server stays Allocated.
  - **"Give me a host with capacity and assign X to it"**: one allocation request with two parts.
    - Selectors: Allocated with `minAvailable: 1`, falling back to Ready with `minAvailable: 1`.
    - Mutations: `addValues: [X]`, and `priorities` to pack or spread.
  - Caveats:
    - (a) Nothing enforces uniqueness across GameServers [I]. Two concurrent "ensure X" allocations could each add X to a
      different server; neither saw the other's add because of the 1 s eventual consistency. Your Postgres owner epoch
      stays the source of truth, and the list is only a cache or index.
    - (b) Reading through an allocation has side effects (`last-allocated` annotation, possible state change). Read-only
      lookups would go through a plain `kubectl`/API list of GameServers instead, filtered client-side [I].
    - (c) The list is not durable ownership. If the pod dies, the GameServer goes Unhealthy and its lists die with it.

## 3. Directly created GameServers, fixed names, restarts, identity

- **Direct creation is supported** [P]:
  - Use `kind: GameServer` with either a fixed `metadata.name` (≤ 63 characters) or `generateName`.
  - "The GameServer resource does not support updates". Spec changes mean delete + recreate; for rolling changes the
    docs point to Fleets.
  https://agones.dev/site/docs/reference/gameserver/
- **Crash behaviour** [P]:
  - **Before Ready**: the container restarts per `restartPolicy`.
  - **After Ready (Ready/Allocated/Reserved)**: the container restarts, but the GameServer "immediately moves to
    `Unhealthy`".
  - **Missed `Health()` pings** (`periodSeconds` 5 × `failureThreshold` 3 by default): the GameServer goes Unhealthy with
    no restart.
  - **SidecarContainers (Beta, default on)**: the pod is `restartPolicy: Never` and the SDK runs as a sidecar. A non-zero
    exit means Unhealthy; a clean exit means Shutdown.
  https://agones.dev/site/docs/guides/health-checking/
- **Unhealthy outcome** [P]:
  - **Standalone GameServer**: "stays Unhealthy until explicitly deleted". Nothing recreates it, so your own controller
    must delete it and create it again under the same name.
  - **Fleet / GameServerSet**: the Unhealthy GameServer is deleted and a replacement created immediately. Replacements
    get new generated names, `<fleet>-<set-hash>-<random>` per the examples [P/S]. There is no ordinal or name reuse.
- **Stable identity like StatefulSet ordinals: none in Agones** [P/I]. I found no Agones feature or issue providing
  ordinals. The only route is standalone GameServers with fixed names plus your own recreate loop, i.e. writing an
  operator. Identity in Agones is "whatever the allocation returns".

## 4. Lifecycle, long-lived servers, scale-down, updates, disruption

- **States** [P]: Scheduled → RequestReady → Ready → Allocated / Reserved → Shutdown; also Unhealthy and Error.
  - `Ready()` makes the server allocatable. Calling Ready again moves an Allocated server back to Ready.
  - `Allocate()` self-allocates (sets the `last-allocated` annotation). The docs prefer GameServerAllocation.
  - `Reserve(seconds)`: the server cannot be allocated, is "not deleted on scale down or Fleet update", and returns to
    Ready after the duration. `0` means forever.
  - `Shutdown()` terminates the pod. On SIGTERM the SDK sidecar stays up through `terminationGracePeriodSeconds` until
    Shutdown is called, which fits park + save + release.
  https://agones.dev/site/docs/guides/client-sdks/
- **Allocated indefinitely** [P]: an Allocated GameServer "should not be removed until `SDK.Shutdown()` is called or it
  is manually deleted". There is no documented maximum lifetime. Reserved(0) is the other "forever" state.
  https://agones.dev/site/docs/reference/gameserverallocation/
- **Fleet scale-down** [P]: removes **Ready** GameServers. Packed takes them from the nodes with the fewest
  Ready + Allocated servers; Distributed picks at random, or by `priorities` then newest first. Allocated servers are
  never chosen; the docs imply this rather than stating it.
  - If replicas drop below the number Allocated, `allocationOverflow` (Beta) can label or annotate the excess servers so
    the server process can drain itself.
  https://agones.dev/site/docs/advanced/scheduling-and-autoscaling/ , https://agones.dev/site/docs/reference/fleet/
- **Rolling update** [P]:
  - "Allocated GameServers are not deleted until they are specifically shutdown through the game servers SDK".
    Each step adds `maxSurge` new servers and removes up to `maxUnavailable` non-allocated old ones.
  - `Recreate` replaces only non-allocated servers.
  - Blue/green pattern: a second Fleet, with allocation selectors preferring v2 and falling back to v1, then scale v1
    down.
  - **For you** [I]: a build change produces new Ready v2 hosts, while v1 hosts stay up holding their super-regions until
    your coordinator migrates ownership and the v1 host calls `Shutdown()`. That is a controlled drain, compatible with
    the epoch handover.
  https://agones.dev/site/docs/guides/fleet-updates/
- **Disruption** [P]:
  - `spec.eviction.safe`: `Never` (default), `OnUpgrade`, `Always`.
  - `Never` labels the pod `agones.dev/safe-to-evict=false`, which the Helm-installed PDB
    `agones-gameserver-safe-to-evict-false` matches. That blocks the Cluster Autoscaler and, for a limited time, node
    upgrades.
  - Beyond one hour, protection "requires cluster configuration changes outside of Agones". On a self-run k0s/k3s node,
    you decide when to drain, so this matters less [I].
  https://agones.dev/site/docs/advanced/controlling-disruption/

## 5. Official guidance for persistent / MMO / sharded worlds

- **No persistent-world or MMO pattern exists in the docs** [P]. The integration patterns are:
  - Allocation from a Fleet
  - Matchmaker registration
  - Canary Testing
  - Reusing GameServers
  - High Density GameServers
  - Player Capacity
  https://agones.dev/site/docs/integration-patterns/
- The closest is **High Density GameServers** [P]: re-allocate an already-Allocated server using a Counter (`rooms`) or a
  label lock (`agones.dev/sdk-gs-session-ready`), with the server process managing sessions. It notes the pattern
  "requires more management on behalf of the integrated game server process and external systems".
  https://agones.dev/site/docs/integration-patterns/high-density-gameservers/
- Secondary sources describe Agones as built for session-based servers that live minutes to hours [S]. I found no
  first-party talk or example of an Agones-hosted sharded persistent world [U]. Agones Discussions, KubeCon talks and the
  agones-discuss list were not searched in depth.

## 6. Networking

- **Port policies** [P]:
  - `Dynamic` (default): a random free hostPort from the range, mapped to the declared `containerPort`.
  - `Static`: you pick the `hostPort`; collisions are your problem.
  - `Passthrough`: containerPort = the chosen hostPort, so the process must read its port from the SDK or env.
  - `None` (Stable since 1.60): no hostPort; `containerPort` optional. Traffic goes to the pod IP directly
    (PR #3807 [S]).
- **Protocols** [P]: `UDP` (default), `TCP`, `TCPUDP` (the same hostPort for both).
- **Multiple ports and containers** [P]: a GameServer can declare multiple ports, each with an optional `container` and
  a named `range`. PortRanges have been Stable since 1.59; Helm `gameservers.additionalPortRanges` defines them.
  - Default range: `gameservers.minPort` 7000 – `maxPort` 8000.
  - Since 1.62, all of a GameServer's ports are allocated from one node, with no duplicate host ports.
  https://agones.dev/site/docs/reference/gameserver/ , https://agones.dev/site/docs/installation/install-agones/helm/
- **Address in status** [P]:
  - `status.address` is chosen in the order ExternalDNS > ExternalIP > InternalDNS > InternalIP; PodIP is never used.
  - `status.addresses` lists all node addresses plus the pod IPs.
  - `status.ports` lists the allocated ports; the field is not described on the reference page [U].
  - Allocation responses carry `gameServerName`, `ports`, `address` and `nodeName`.
  - **Consequence for single-node k0s/k3s** [I]: if the node has no ExternalIP, `address` will be the InternalIP. Set
    the node's ExternalIP (k3s `--node-external-ip`; on k0s, kubelet/cloud configuration [U]), or publish your own public
    hostname instead of trusting `status.address`.
- **Single-node / local clusters** [P/I]:
  - The installation page lists GKE, AKS, EKS and Minikube as supported. **k3s and k0s are not mentioned** [P].
  - Minikube [P]:
    - publish a narrow UDP range with the docker driver (`--ports 7000-7100:7000-7100/udp`), because publishing all of
      7000–8000 "may cause Minikube to crash";
    - keep that range in step with Helm `minPort`/`maxPort`;
    - connect via `minikube ip`, or 127.0.0.1 with the docker port publish;
    - `minikube tunnel` UDP is unreliable.
    https://agones.dev/site/docs/installation/creating-cluster/minikube/
  - hostPort needs a CNI with portmap support. k0s's kube-router hostPort support is unverified from docs [U], but this
    project's own 2026-09-22 test ran two Dynamic-UDP GameServers on k0s-in-docker, which is empirical evidence.
  - Dynamic hostPorts solve the "fixed hostPorts collide on one node" problem by construction [I].
- **Per-GameServer DNS names: no such feature found** [U/I]. Agones works through `address` + ports. For WebTransport
  you would need a TLS certificate valid for the node hostname and a port number in the URL. That works with one
  wildcard or node certificate, since the hostname is per node, not per pod [I].

## 7. SDKs (Rust)

- **Official Rust SDK** [P]:
  - crate `agones` on crates.io, published in lockstep with every release (1.62.0 on 2026-11-04);
  - async-only, on tonic + tokio;
  - lives in-tree at `sdks/rust`;
  - the docs page lists all of these as implemented: Ready, Health (an `mpsc::Sender<()>`), Reserve, Allocate,
    Shutdown, GameServer, WatchGameServer, SetLabel, SetAnnotation, all Counter ops, all List ops.
  - The docs example pins the old "1.34.0"; match the crate to the cluster version.
  https://agones.dev/site/docs/guides/client-sdks/rust/ , https://crates.io/api/v1/crates/agones
- **Language-neutral API** [P]: the SDK sidecar serves gRPC on `AGONES_SDK_GRPC_PORT` (9357) and REST/JSON via
  grpc-gateway on `AGONES_SDK_HTTP_PORT` (9358). Local-mode SDK tooling exists for development without k8s.
  - `SetLabel` / `SetAnnotation` keys are prefixed `agones.dev/sdk-`.
  - Counters and Lists sit under `Beta()` in the SDKs; with 1.62 Stable they may move [U].
  https://agones.dev/site/docs/guides/client-sdks/

## 8. Allocation API / allocator service / multi-cluster

- `GameServerAllocation` is a create-only Kubernetes API object (`allocation.agones.dev/v1`) [P]. Any in-cluster client
  with RBAC can create one, for example the login service through kube-rs.
- **Allocator service** [P]:
  - gRPC + REST behind mTLS, with client certificates allow-listed in `allocator-client-ca`;
  - Helm `agones.allocator.service.serviceType` defaults to `LoadBalancer` and `agones.allocator.replicas` to 3; you can
    switch to ClusterIP and lower the replicas;
  - the request takes the same selectors and metadata (`metaPatch` deprecated); the response gives name, ports, address
    and node.
  https://agones.dev/site/docs/advanced/allocator-service/
- **Can login use it to find or ensure an owner?** [I]
  - **Find**: yes, via `containsValue` on Allocated servers (section 2).
  - **Ensure**: possible, but without cross-GameServer uniqueness and with ~1 s staleness. It would duplicate what your
    coordinator + Postgres epoch already decide. Treat it as a lookup cache at most. The authority stays in Postgres and
    the coordinator.
- **Multi-cluster** [P]: `GameServerAllocationPolicy` CRD (priority / weight / connectionInfo). The docs now "recommend
  implementing a Service Mesh in each of your Agones clusters" instead. The stage is not stated.
  https://agones.dev/site/docs/advanced/multi-cluster-allocation/

## 9. Autoscaling

- FleetAutoscaler policies [P]:
  - `Buffer` (`bufferSize`, min/max replicas);
  - `Webhook`;
  - `Counter` and `List` (`key`, `bufferSize`, `minCapacity`, `maxCapacity`; Beta, via CountsAndLists → now Stable);
  - `Schedule` and `Chain` (ScheduledAutoscaler, Beta);
  - `Wasm` (Alpha).
  https://agones.dev/site/docs/reference/fleetautoscaler/
- **Relevance** [I]: a `List` policy on `superregions` could keep N free super-region slots of spare hosts (a buffer of
  Ready hosts). With a fixed shard count and coordinator-driven assignment, a static `replicas` or a Webhook policy
  driven by the coordinator would be simpler. Autoscaling barely matters on one node.

## 10. Operations

- **Install** [P]: Helm chart (Helm ≥ 4.1.3 per the current docs).
  - Default replicas: controller 2, allocator 3 (a LoadBalancer Service), extensions/webhook 2.
  - `resources` default `{}`; feature gates via `agones.featureGates`.
  - OpenTofu: `helm_release` from the existing `deploy/` roots [I].
  https://agones.dev/site/docs/installation/install-agones/helm/
- **Resource cost**: no official number found [U]. On a small node, setting the replicas to 1 each and the allocator to
  ClusterIP (or disabling it) trims it [I].
  - The allocator's default LoadBalancer Service will be served by k3s's klipper ServiceLB, binding host ports [S].
    On k0s with no LB provider it stays Pending [I].
- **Known single-node issues**:
  - none Agones-specific found [U];
  - Minikube's UDP range and tunnel caveats above [P];
  - static-port allocator bug #2314 (old, stale) [S];
  - the duplicate-host-port bug, fixed in 1.62 [P];
  - version skew: Agones 1.62 supports only Kubernetes 1.34–1.36, so the k0s/k3s version must be in that window [P].

## Assessment

- **Where Agones genuinely helps**:
  - per-pod Dynamic hostPorts on a shared node, with status reporting address + ports;
  - "don't kill a busy server" semantics: Allocated / Reserved are protected from scale-down and rolling updates, plus
    the PDB;
  - SIGTERM → Shutdown drain plumbing;
  - a Rust SDK.
- **Where it fights the design**:
  - no stable identity (Fleet replacements get new names; standalone servers are never recreated);
  - Unhealthy is terminal;
  - GameServers are immutable;
  - ownership lists are not durable or unique;
  - the whole allocation model assumes Ready pools handing out sessions, not a fixed set of long-lived owners.
- **Rendezvous hashing on Agones**: you cannot hash onto a stable member set, because members are just "currently live
  hosts". You would need the coordinator to assign super-regions to live hosts (a dynamic membership table in Postgres),
  which is what the epoch row already supports.

**Fits better than the StatefulSet IF** the design is coordinator-assigned ownership, i.e. not a pure rendezvous hash
over ordinals:

- a Fleet of region hosts, `eviction.safe: Never`;
- each host self-`Allocate()`s (or `Reserve(0)`s) once it holds any super-region, so scale-down and rolling updates never
  take it;
- Dynamic UDP ports, two per host (QUIC + WebTransport), so no per-ordinal NodePort Services are needed;
- the host publishes its address and ports into the Postgres membership row;
- the coordinator moves ownership on drain or death;
- login reads Postgres, not the Agones allocator;
- new builds: a second Fleet, then migrate and `Shutdown()` the old hosts;
- Counters/Lists are optional observability.

**If you want pure rendezvous-by-ordinal with no coordinator in the assignment path**, the StatefulSet is the better
fit: stable names and headless DNS give you that directly. Its costs:

- per-ordinal NodePort Services, with fixed ports or `hostPort` per ordinal through per-pod config;
- an ordinal's super-regions are dark while it restarts;
- rolling updates kill hosts in reverse ordinal order without asking. `partition` + OnDelete give manual control.
