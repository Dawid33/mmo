# Agones vs StatefulSet vs OpenKruise Game for a persistent, sharded voxel world: real-world experience and failure modes

Research date: 2026-11-06. Web research only. Confidence tags:
- **[P]** primary source: project docs, the issue tracker, or the vendor's own docs.
- **[S]** secondary source: a vendor or cloud blog, a republication, or an aggregator.
- **[U]** unverified: the claim was seen only in search snippets or low-quality pages.

Use case under evaluation: about 625 super-regions, each owned long-term by one host pod, and one pod owns many of them. Clients connect over UDP (QUIC on one port, WebTransport on another) straight to the owning pod. Ownership is fenced in Postgres. Production runs on a single node (k0s or k3s); local testing runs on minikube.

---

## 1. Who runs persistent, always-on, MMO or sharded worlds on Agones?

### Evidence found

- **Nitrado GameFabric** [P]. This is the strongest data point. GameFabric is Nitrado's commercial hosting platform, it is built on Agones and GKE, and it took over Hathora's customers after Hathora shut down in 2026. Its glossary keeps two separate product categories:
  - *Dynamic Fleets*: Armada → Agones Fleet, for session-based games.
  - *Persistent Servers*: **Formation** (a template) → **Vessel** (a single, named, long-running server tied to game or player progression; env and args can be overridden per Vessel). Its definition of a persistent server: "Use this model when players return to the same server."

  So a large Agones-based host found the Fleet abstraction a poor fit for persistent servers and built its own named-instance layer on top of Agones. The docs do not say how a Vessel maps to an Agones `GameServer`. It is probably a standalone GameServer with a fixed name, but that is my inference [U].
  - https://docs.gamefabric.com/multiplayer-servers/getting-started/glossary (fetched 2026-11-06)
  - GamesBeat on GameFabric being Agones-based: https://gamesbeat.com/nitrados-gamefabric-aims-to-offload-multiplayer-infrastructure-from-game-devs/ [S]
  - Arma Reforger (Bohemia) is a customer quoted on https://server.nitrado.net/pages/business [S]

- **AWS Compute Blog, "How to run massively multiplayer games with EC2 Spot using Aurora Serverless"** (18 Aug 2021; Yahav Biran, Pritam Pal) [S]. A Python Minecraft clone (Craft) runs as Agones GameServers on EKS Spot. The world is chunked, all state is in Aurora Postgres with lazy writes, a new player calls `/allocate`, and a Spot notice drains the node and sends SIGTERM to the game servers. Two caveats from the post:
  - Agones' unpredictable dynamic ports make an NLB in front hard.
  - The post itself offers a plain Deployment + Service + NLB as the alternative.

  This is a demo, not a production MMO, but it matches our design: state lives in a DB, so a server is disposable.
  - https://aws.amazon.com/blogs/compute/how-to-run-massively-multiplayer-games-with-ec2-spot-using-aurora-serverless

- **Alibaba Cloud, "Agones series"** (19 Oct 2022; Qiuyang Liu "Liming", who later maintains OpenKruise Game) [S]. Two claims:
  - Agones is "not suitable for long-lifecycle game servers" such as PvE big-world games and MMO/MMORPGs, because each instance's state differs a lot and the horizontally scaling Fleet model fits badly.
  - Agones' network type is fixed: hostPort cannot cover every case, such as a fixed IP and port.

  This is the founding argument for OKG, written by an interested party.
  - https://www.alibabacloud.com/blog/599431

- **Agones issue #2286, "Numeric Based Naming for Game Servers"** (opened 2 Oct 2021, closed "not planned" 1 Dec 2023) [P]. A Minecraft network wanted StatefulSet-style `lobby-0`, `lobby-1` names and was using StatefulSets or manual numbering instead. Responses:
  - Maintainer Mark Mandel: "Don't use numbers for lobbies." Gaps in numbering or poor packing after scale-down, and races with allocation and autoscaling.
  - Robert Bailey asked for more detail. Nothing was built.

  **Agones has explicitly declined StatefulSet-like ordinal identity for Fleets.**
  - https://github.com/googleforgames/agones/issues/2286

- **Agones FAQ, "Why not use Deployment or StatefulSet?"** (PR #2824, closes #2770) [P]. Deployments are for unordered stateless workloads and StatefulSets for ordered stateful ones. Game servers are "sometimes stateful" (only while players are on them) and need scale-down and allocation priority. The FAQ does not address MMOs or persistent servers. Its only reuse advice is to call `SDK.Ready()` to return an Allocated server to the pool.
  - https://agones.dev/site/docs/faq/

- **Who uses Agones at all.**
  - Ubisoft co-announced Agones in 2018 [S].
  - Supersolid (mobile) presented at GDC 2019 [S]: https://www.gdcvault.com/play/1026155
  - TheirStack's "companies using Agones" lists are noisy (trucking and construction firms) [U].
  - PlayStation's answer to The Stack was "some on-prem, some in the cloud, all using custom solutions", so not confirmed as Agones [S]: https://thestack.technology/playstation-gameservers-kubernetes-agones-multiplayer-games/
  - **I found no public case study of an MMO, or any persistent sharded world, running its zones as plain Agones Fleets.**

- **Project status** [P/S].
  - Agones was accepted into the CNCF Sandbox (Dec 2025); the transition was announced 23 Mar 2026: https://www.cncf.io/blog/2026/03/23/agones-moves-to-the-cncf-a-new-era-for-open-source-multiplayer-game-infrastructure/
  - Releases now appear under `github.com/agones-dev/agones`.
  - v1.59.0 (July 2026) made `PortRanges` (multiple named port ranges) and `RollingUpdateFix` stable, and removed PlayerAllocationFilter/PlayerSelector from GameServerAllocation (a breaking change): https://github.com/agones-dev/agones/releases/tag/v1.59.0

- **Hosting-market context** [S].
  - Hathora (Kubernetes-based) shut down its hosting on 5 May 2026 after an AI acquihire; Stormgate lost multiplayer as a result.
  - Unity Multiplay hosting ended 31 Mar 2026.
  - The managed-host landscape is volatile; self-hosting reduces that risk.
  - https://crux.supercraft.host/blog/hathora-shut-down-where-to-go-after-may-2026/ and https://www.techspot.com/news/111969-stormgate-servers-go-dark-following-ai-focused-hosting.html
  - A Komodor podcast with Hathora, on running stateful game servers on k8s, returned **HTTP 403**. Recorded as a gap; not routed around. A search snippet says Hathora found hostPort / NodePort / ingress awkward for per-match ports, and used Talos + Omni to mix bare metal and cloud [U].

### Moved away from Agones, or onto it

- **To StatefulSets:** the #2286 requester (Minecraft lobbies) [P].
- **To OKG:** Yahaha's STRIDEN (an FPS) migrated to OKG on Alibaba [S]. It is unclear whether they came from Agones. https://www.alibabacloud.com/blog/from-minutes-to-seconds-yahahas-cloud-native-ue5-game-practice-powered-by-openkruisegame_602355
- No migration away from Agones by a Western studio was found.

---

## 2. Agones issues and discussions relevant to persistent, long-lived servers

| Topic | Finding | Source |
|---|---|---|
| Stable names | Ordinals refused (#2286). A standalone `GameServer` can take a fixed `metadata.name`, which gives a stable name with no ordinal and no controller recreating it [P/S]. | #2286; zenn.dev article [S] |
| Crash semantics | A game container crashing while Ready, Allocated or Reserved moves the GameServer to **Unhealthy**. With the `SidecarContainers` gate (Beta, on by default) the pod has `restartPolicy: Never` and "a terminated game server container can never be restarted". In a Fleet, an Unhealthy GameServer is **deleted and replaced** with a new random name; a standalone one stays Unhealthy until deleted. **There is no in-place restart of a persistent process**: every crash is a new pod, a new name and possibly a new port. | https://agones.dev/site/docs/guides/health-checking/ [P] |
| Rolling updates with Allocated servers | Fleet updates never delete Allocated servers. Only Ready and Reserved servers are replaced. Long-lived Allocated servers get user-defined labels or annotations on template change and must watch `WatchGameServer()` and shut themselves down. A replicas-only change ignores maxSurge and maxUnavailable. | https://agones.dev/site/docs/guides/fleet-updates/ [P] |
| Update stall | agones-discuss, 4 Apr 2026, Agones 1.39 on GKE: 8 always-Allocated servers. Even with `maxSurge: 99%` the new GameServerSet stayed at desired 0 until the old Allocated servers shut down, leaving half capacity. Workaround: temporarily double replicas. Mark Mandel's reply only pointed to a Slack thread. `RollingUpdateFix`, stable in 1.59, may address this; not verified. | https://groups.google.com/g/agones-discuss/c/pRQkp4WSfTI [P] |
| Drain and eviction | `eviction.safe` has three modes. **Never** (the default) sets `safe-to-evict=false` and the PDB `agones-gameserver-safe-to-evict-false`. **OnUpgrade** must exit within 1 h of SIGTERM. **Always** must exit within 10 min. Node upgrades "may not honor PodDisruptionBudget for longer than an hour" (on GKE the PDB is then ignored or the upgrade fails). Cluster Autoscaler gives 10 minutes. Nothing about single-node clusters or `kubectl drain`. On a single node, a Never-PDB **blocks `kubectl drain` indefinitely** unless forced. That is generic Kubernetes PDB behaviour (my inference) and matters for k0s/k3s upgrades. | https://agones.dev/site/docs/advanced/controlling-disruption/ [P]; design issue #368 [P] |
| Port allocation | Dynamic ports come from one range per cluster (default 7000–8000), with multiple named ranges stable in 1.59. Static policy means one GameServer per port per node. Mixing other hostPorts inside the dynamic range is unsupported. On a shortage the pod is held Pending. 625 regions on a few pods is far from exhaustion; two ports per pod is fine. | pkg.go.dev portallocator [P]; https://www.mintlify.com/googleforgames/agones/advanced/port-allocation [S] |
| TCPUDP static bug | #2314: `protocol: TCPUDP` with Static ports was rejected. Fixed in PR #3375 (closed Nov 2023). Shows the static-port paths are less exercised. | https://github.com/googleforgames/agones/issues/2314 [P] |
| Address reporting | `GameServer.status.address` comes from the Node: ExternalDNS (feature-gated), then ExternalIP, then InternalIP / InternalDNS. Bare-metal k3s/k0s nodes usually have **no ExternalIP**, so Agones reports the internal IP. Redpoint's k3s + Agones guide sets k3s flags (`--node-external-ip` and the interface) for this reason. Since 1.34 `status.addresses` lists all node addresses. | FAQ [P]; https://docs.redpoint.games/docs/dedis/creating_your_cluster [S]; https://www.alibabacloud.com/blog/agones-series-part-2-address-and-port-of-the-game-server_599427 [S] |
| minikube | Exposing the whole 7000–8000 range can stop minikube from starting; use 7000–7100. UDP through `minikube tunnel` may not work depending on the driver. | https://agones.dev/site/docs/installation/creating-cluster/minikube/ [P] |
| k3s | A single-node k3s + Agones tutorial exists (Linode/Akamai, Xonotic, Agones 1.47). The OVH docs note that Agones recommends separate node pools for the controllers and the game servers, which a single node ignores. A dev blog calls setup heavier than expected. | https://www.linode.com/docs/guides/set-up-a-xonotic-game-server-with-k3s-and-agones; https://blog.devgenius.io/what-i-learned-from-trying-agones-577f93193419 [S] |
| Counters and Lists | Beta and on by default in recent docs. Keys must be declared at GameServer creation (not dynamic); at most 1000 values per list. They replace Player Tracking (alpha, to be removed); 1.59 removed PlayerSelector from allocation. **No production write-up found.** Fixed keys suit "regions owned" as a List, though 625 ids split over N pods fits the 1000 cap. | https://agones.dev/site/docs/guides/counters-and-lists/ [P] |

Our own repo already ran an "Agones feasibility PASSED" check on k0s-in-docker (see `deploy/` and `docs/superpowers/specs/2026-09-22-local-deployment-environment-design.md`). That is internal evidence that it installs and runs there.

---

## 3. Other patterns: OpenKruise Game, Shulker, StatefulSet + per-pod Service, Thundernetes, managed hosts

### OpenKruise Game (OKG)

- **What it is** [P]. A CNCF OpenKruise sub-project (Alibaba-led). Its `GameServerSet` gives:
  - Ordinal, stable names (`gss-0`, `gss-1`, …), "fixed and unchanging" like a StatefulSet.
  - A headless Service for per-pod DNS (`{pod}.{gss}.{ns}.svc.cluster.local`).
  - `reserveGameServerIds`, including ranges since v1.0, so specific ids can be skipped or merged.
  - In-place updates (`podUpdatePolicy: InPlaceIfPossible`; an image change bumps the restart count, a resource change needs a manual pod delete).
  - Hot-update sidecars.
  - `opsState` / service qualities for probe-driven state.
  - Player-aware scale-down priority.
  - Annotation sync from GameServer to Pod.
  - Network status (`networkStatus.externalAddresses`) on the GameServer CR, also exposed to the pod via the Downward API (`game.kruise.io/network-status`).
  - Sources: https://openkruise.io/kruisegame/user-manuals/deploy-gameservers ; https://www.alibabacloud.com/blog/kruise-game-best-practices-for-traditional-pve-games_600822 (1 Feb 2024, ChrisLiu)
- **Network plugins** [P]. 19 in total, mostly tied to a cloud's load balancers (Alibaba, AWS NLB, Tencent, Volcengine, JD, Huawei). The cloud-agnostic ones:
  - **Kubernetes-HostPort**: a managed range, default 8000–9000, UDP and TCPUDP supported. Since v1.1 the same port can be reused on different nodes with least-used allocation. Needs a public host IP. Some CNIs lack hostPort support.
  - **Kubernetes-NodePort**: one NodePort Service per GameServer, in the 30000–32767 range.
  - **Kubernetes-Ingress**.
  - Source: https://openkruise.io/kruisegame/user-manuals/network
- **Releases** [P]. v0.9 (Aug 2024), v0.10 (Jan), v1.0.0 (21 Jul, 2025 per a late-2025 install blog), **v1.1.0 (25 Jun, presumably 2026, marked Latest)**. The install docs use the kruise chart 1.8.0 plus kruise-game 1.1.0. v1.0 added external certificates, which allow multiple controller replicas.
  - https://github.com/openkruise/kruise-game/releases
  - https://github.com/openkruise/kruise-game/blob/master/CHANGELOG.md
- **Adoption** [P, logos only]. About 1.1k stars and 36 adopter logos, almost all Chinese studios: Lilith, Hypergryph, Bilibili, Yostar, Kunlun, 360, Chillyroom, and others. Case studies [S]:
  - Guanying Interactive's 2D MMO engine "Thousand". Each zone is a "fat container" GameServer, with DNAT automation over shared EIPs and hot updates (29 Nov 2023, Alibaba Cloud Native): https://www.besthub.dev/articles/how-cloud-native-architecture-supercharged-a-2d-mmo-engine-on-kubernetes-ebb04bf1fb68
  - The PvE "区服" (zone-server) best-practice guide maps one zone to one GameServer, with its ordinal used as the key into config or object storage.
  - **OKG is the only orchestrator with documented MMO and zone-server use.** It is in Alibaba's ecosystem, its docs and community are partly Chinese, and Western adoption is thin.
- **Dependencies and local clusters** [P/S].
  - OKG needs OpenKruise core installed (the kruise-manager controller plus the **kruise-daemon** DaemonSet that talks to the container runtime). That is a second controller stack.
  - No k3s, k0s, minikube or kind guidance in the docs.
  - Kruise on k3s needs `daemon.socketLocation=/var/run/k3s`, plus a DNS-policy fix (openkruise/kruise#1447, k3s 1.23, fixed in charts#109): https://github.com/openkruise/kruise/issues/1447
  - k0s's containerd socket path (`/run/k0s/containerd.sock`) would likewise need setting. Not verified.

### Shulker (a Minecraft operator in Rust)

- About 414 stars, AGPL-3.0, 1,321 commits. It tags `agones` as a topic, but the README and landing page do not say how it uses Agones. **Not verified.** I recall that Shulker creates Agones GameServers/Fleets for proxies and servers, but could not confirm it from fetched pages [U].
- https://github.com/jeremylvln/Shulker

### StatefulSet + per-pod NodePort Service

- **The mechanics are well documented** [P]. The StatefulSet controller labels every pod with `statefulset.kubernetes.io/pod-name`, so one Service per pod (NodePort or LoadBalancer) can select exactly that pod.
  - https://kubernetes.io/docs/concepts/workloads/controllers/statefulset/
- Write-ups suggest `externalTrafficPolicy: Local` and automating the per-pod Services with metacontroller [S].
  - https://itnext.io/exposing-statefulsets-in-kubernetes-698730fb92a1
- The only drawback anyone cites is that Services must be created per replica. With a handful of host pods that is a trivial Helm loop or a small controller.
- Gamezop's write-up warns that a StatefulSet restart loses in-memory state [S].
  - https://medium.com/gamezop/deploying-stateful-phoenix-game-servers-on-kubernetes-28e18a103df3
  - For us this does not matter: state is in Postgres and on disk.

### Thundernetes (PlayFab / Microsoft)

- Beta, last module release 2022. A tracker shows activity into 2026 [U]. Not a serious option.

### Edgegap and GameFabric

- Edgegap: a managed edge platform with a "Private Fleet" for persistent servers [S].
- GameFabric: Agones-based, with Formations and Vessels (above).
- Neither is self-hosted on our k0s.

---

## 4. Per-pod UDP exposure pitfalls (QUIC)

- **Stale UDP conntrack entries** [P/S]. When a UDP backend pod is replaced behind a Service or NodePort, the old conntrack entry can keep black-holing packets until it times out. Kernel defaults: 30 s unreplied, 120 s for an established stream (USENIX SRE 2026 slides). The QUIC client keeps sending from the same 4-tuple, so the entry never ages out while the client retries.
  - kube-proxy added NodePort UDP stale-entry flushing in 2021 (Red Hat BZ 1923231).
  - A regression was reported on 1.22 (kubernetes#105657).
  - A general, reconciler-style `CleanStaleEntries` that scans the table appears in **v1.32** (the pkg/proxy/conntrack API). The SRE'26 slides say "Fixed in v1.32".
  - Implications: **use Kubernetes ≥ 1.32**, keep the pod IP stable, or rely on client reconnect with a new source port.
  - Sources: https://bugzilla.redhat.com/show_bug.cgi?id=1923231 ; https://github.com/kubernetes/kubernetes/issues/105657 ; https://pkg.go.dev/k8s.io/kubernetes@v1.32.1/pkg/proxy/conntrack ; https://www.usenix.org/system/files/sre26amer_slides_bejarano.pdf
- **Source IP / SNAT** [P]. NodePort traffic is SNATed under the default `externalTrafficPolicy: Cluster`. That breaks per-client rate limiting, address validation and logging, and some QUIC behaviour that depends on the peer address. Use `Local`. On a single node `Local` costs nothing, because the endpoint is always local.
  - https://kubernetes.io/docs/tutorials/services/source-ip/
- **hostPort** [S]. hostPort needs the CNI `portmap` plugin. On k3s, flannel ships it. For k0s's default kube-router, portmap chaining was not verified (only a 2018 gist and Cilium docs found). Agones depends on hostPort, so this is worth testing on k0s. Our earlier feasibility run suggests it works.
- **hostNetwork**. Avoids NAT and conntrack entirely and binds the real ports. There is no port isolation: every pod needs unique ports, which the server can take from its ordinal. This is the simplest choice on a single node. Not researched further.
- **MetalLB**. Gives one IP per Service in L2 mode on bare metal. Only useful with several public IPs; a single-node box with one IP gets nothing from it. No QUIC-specific issues found.
- **minikube**. UDP through `tunnel` and port mappings depends on the driver; the Agones minikube page warns about it. Expect local UDP tests to need `minikube ip` + NodePort, or the docker driver with explicit `--ports`.
- **QUIC connection migration**. Nothing found on how kube-proxy interacts with QUIC connection IDs. A gap.

---

## Synthesis for this use case

### For Agones

- Production-proven lifecycle tooling: SDK health checks, Ready and Allocated states, `eviction.safe`.
- Automatic per-pod port allocation and address reporting.
- Allocation API, Counters and Lists.
- Now a CNCF project, so long-term governance is better than OKG's single-vendor community.
- GameFabric shows persistent servers can sit on Agones, but only with a custom named-instance layer on top.
- Our k0s feasibility check already passed.

### Against Agones

- Its model is disposable, interchangeable sessions:
  - No ordinals (refused in #2286).
  - A crash means a new GameServer with a new name and port; in-place restart is impossible with sidecar mode on by default.
  - Fleet rolling updates skip Allocated servers; you must signal them yourself, and the reported stall needed a 2× replica surge.
  - `eviction.safe: Never` PDBs will block `kubectl drain` on a single node.
  - Without an ExternalIP on the node, the reported address is the internal IP.
- We already have the hard part: Postgres epoch fencing and rendezvous assignment. Agones would add a controller, webhooks, CRDs and an SDK sidecar mainly to manage port numbers we could fix ourselves.
- No public MMO or persistent-world deployment on Agones Fleets was found.

### OpenKruise Game

- Conceptually the closest fit: StatefulSet-like ordinals, in-place updates, a HostPort/NodePort plugin, network status written back to the CR, and documented zone-server (MMO "区服") use.
- Against it:
  - It requires OpenKruise core plus a privileged per-node daemon.
  - k3s/k0s need socket and DNS tweaks.
  - Docs and community are Alibaba- and China-centric.
  - Its best features (LB plugins, hot-update tooling) target Alibaba Cloud.
- It is a real contender only if in-place updates without pod recreation, or managed per-server networking, become requirements. For one node with a handful of pods, it adds more machinery than a StatefulSet.

### Gaps

- The Komodor/Hathora podcast (HTTP 403).
- How GameFabric Vessels map to Agones objects.
- Whether Shulker uses Agones.
- Whether 1.59's RollingUpdateFix resolves the update stall.
- hostPort support under k0s kube-router.
- Any QUIC-specific kube-proxy report.
- Any production report on Counters and Lists.
- The exact OKG v1.0 and v1.1 release years.
