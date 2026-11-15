# UDP / QUIC network monitoring for the game in k8s — research

**Date:** 2026-11-06
**Status:** RESEARCH — nothing built.
**Context:** the observability stack on `orion` (`deploy/modules/monitoring`, handoff
`docs/superpowers/plans/2026-11-06-k3s-observability-stack-handoff.md`): kube-prometheus-stack
(Prometheus + node-exporter + kube-state-metrics + kubelet/cAdvisor scrapes, every
ServiceMonitor/PodMonitor in the cluster picked up), Thanos → S3, Loki, Tempo, Pyroscope and an
Alloy DaemonSet (pod logs → Loki, OTLP 4317/4318 → Tempo). The game will run as a **StatefulSet
with per-pod NodePort UDP** (`externalTrafficPolicy: Local`), `hostNetwork` as the fallback
(`2026-09-20-multi-instance-k8s-postgres-research.md` §0.7b′); §0.9(g) already plans a game
`/metrics` endpoint (`metrics` + `metrics-exporter-prometheus`).

## 1. What we want to know

1. **Does traffic reach the pod?** Players' datagrams arriving vs. none (firewall, NodePort,
   the ~120 s stale-conntrack black hole after a pod restart, §0.7b′).
2. **Is the kernel dropping it?** Socket receive buffer overflow, NIC/softnet drops, no listener.
3. **What does each player's link look like?** RTT, loss, congestion, MTU — the things that
   decide whether rollback netcode feels good.
4. **How much do we send, and to whom?** Bytes/datagrams per pod, per packet type, snapshot and
   LOD bursts; later pod↔pod region links.
5. **Is the server's own send path keeping up?** Writer stalls, `SEND_DEADLINE` timeouts
   (today's `[server]` `timeouts=` counter — the dropped-packet gap in `netcode_web`/`lib.rs`).

## 2. The constraint that decides the design

**Our UDP is QUIC (quinn 0.11, port 6466) and WebTransport over HTTP/3 (wtransport 0.7, 6467):
encrypted, including packet numbers and ACKs.** A packet sniffer or eBPF flow meter sees only the
5-tuple, sizes and counts — never RTT, loss, retransmits or which game packet it was. So:

- **Link quality (3) and send-path health (5) can only come from the QUIC endpoint itself** —
  i.e. instrumenting the server. No external tool can substitute.
- External tools are good for (1), (2) and the volume side of (4).
- pcap-based exporters (packetbeat-style, Pixie's protocol tracing) add cost without insight here.

A second, easy-to-miss constraint: **UDP MIB counters (`/proc/net/snmp`, `/proc/net/netstat`)
are per network namespace.** node-exporter runs with `hostNetwork`, so it sees the *host's*
counters. A game pod on the pod network has its own netns: a receive-buffer overflow on the
game's socket is counted there and is **invisible to node-exporter**. cAdvisor's
`container_network_*` come from the pod's interface stats, not the UDP MIB, so they don't show it
either. (Under the `hostNetwork` fallback the opposite holds: node-exporter sees it, but cAdvisor
can no longer split traffic per pod.)

## 3. Options, by layer

### L1 — Already running, zero code (node-exporter + cAdvisor)

All default-enabled in node-exporter (verified against the upstream README / 1.9 source):

| Metric | Answers |
|---|---|
| `node_netstat_Udp_InDatagrams` / `OutDatagrams` (+ `Udp6_`) | host-netns UDP volume |
| `node_netstat_Udp_RcvbufErrors` / `SndbufErrors` / `InErrors` | host-netns socket buffer drops |
| `node_netstat_Udp_NoPorts` | datagrams to a port nobody listens on (pod down, wrong NodePort) |
| `node_udp_queues{queue="rx"\|"tx"}` (`udp_queues` collector) | bytes sitting in UDP socket queues — rising rx = reader not keeping up |
| `node_network_receive_drop_total`, `node_softnet_dropped_total`, `node_softnet_times_squeezed_total` | NIC / backlog drops before any socket |
| `node_nf_conntrack_entries` / `_limit` (conntrack collector) | conntrack table pressure (NodePort DNATs every UDP flow) |
| cAdvisor `container_network_{receive,transmit}_{packets,bytes,packets_dropped}_total{pod=…}` | per-pod volume and interface drops (pod-network mode only) |

Cost: dashboards + alert rules only (both can live in the monitoring module as code). This is the
first thing to do once a game pod exists; it already answers "is anything arriving at the pod".

### L2 — Instrument the game server (the core; planned anyway in §0.9(g))

**quinn already measures everything we want, per connection**: `Connection::stats()` →
`ConnectionStats { udp_tx, udp_rx, frame_tx, frame_rx, path }`, where `PathStats` has `rtt`,
`min_rtt`, `cwnd`, `congestion_events`, `lost_packets`, `lost_bytes`, `sent_packets`,
`black_holes_detected`, `current_mtu`, PLPMTUD probe counts (quinn-proto 0.11.12;
`#[non_exhaustive]`), and `UdpStats` has datagram/byte/IO-call counts. WebTransport connections
reach the same struct: wtransport 0.7 has `Connection::rtt()` without features and
`Connection::quic_connection()` behind its `quinn` crate feature.

Proposal:

- **Exporter:** `metrics` + `metrics-exporter-prometheus` serving `/metrics` on a TCP port (as
  §0.9(g) decided); a **PodMonitor** (no Service needed per pod) picked up automatically —
  `podMonitorSelectorNilUsesHelmValues: false` is already set. Alternative: OTLP metrics push to
  Alloy (already listening on 4317/4318) — only worth it if we want exemplars linking to Tempo
  traces later; scraping is simpler and is what the KEDA/HPA plans assume.
- **Sampling:** a 1 s (or `[server]`-window) task walks the open connections and records:
  - histograms: `game_conn_rtt_seconds`, `game_conn_min_rtt_seconds` (one observation per
    connection per sample — distribution across players, no per-player label);
  - counters, as deltas since the last sample: `game_quic_lost_packets_total`,
    `game_quic_sent_packets_total`, `game_quic_congestion_events_total`,
    `game_quic_black_holes_total`, `game_udp_{rx,tx}_{datagrams,bytes}_total`;
  - gauges: `game_connections{transport="quic"|"webtransport"}`, `game_quic_cwnd_bytes`
    (histogram if wanted), `game_quic_mtu_bytes` (min).
  - Labels: `transport` only (+ pod/namespace from the PodMonitor). **Never a player or
    connection id label** — cardinality; per-player detail goes to logs (below).
- **Application-level counters next to it** (what QUIC can't know): bytes and packets sent per
  `ServerPacket` kind (snapshot, LOD, events, hashes), the existing `[server]` line's writer send
  time / `timeouts` / channel backlogs, `[region …]` tick budget. These turn the current text lines
  into series.
- **Pod-netns UDP MIB:** the server reads its own `/proc/net/snmp` `Udp:` line each sample and
  exports `game_netns_udp_{in_datagrams,rcvbuf_errors,sndbuf_errors,in_errors}_total`. It is in
  the pod's netns, so this closes the blind spot from §2 with ~20 lines of code and no privileges.
- **Per-player detail → Loki, not Prometheus:** on disconnect (and every N minutes for long
  sessions) log one structured line `[conn] player=… transport=… rtt_ms= min_rtt_ms= lost=
  sent= congestion= mtu= duration_s=`. Alloy already ships pod logs to Loki; LogQL can then
  answer "which players had bad links" without cardinality cost. Fits the `[perf]` volume
  contract (per connection, not per packet).
- **Inbound loss:** quinn's `lost_packets` is loss of what *we* sent. Client→server loss is only
  visible from the client. If needed later, the client can report its own `PathStats` summary in
  an occasional `ClientPacket` (native: quinn; browser: `WebTransport.getStats()` where the
  browser supports it) and the server folds it into the same histograms with
  `direction="client"`. Not needed for a first version.

### L3 — eBPF flow metrics, no game code (optional)

- **Grafana Beyla inside Alloy** (`beyla.ebpf` component, `metrics.features = ["network"]`):
  `beyla_network_flow_bytes_total` between endpoints, UDP included, decorated with k8s
  src/dst owner (namespace, StatefulSet, pod) and CIDR groups; Alloy exports scrape targets
  for `prometheus.scrape` → our Prometheus. Upstream OpenTelemetry eBPF Instrumentation (OBI)
  is the same engine (metric prefix `obi_`) but has no Alloy integration.
  - Cost: the Alloy pod needs `hostPID: true` and caps `BPF`, `NET_ADMIN`, `NET_RAW`,
    `PERFMON`, `SYS_PTRACE` (or privileged). That widens today's log-shipping DaemonSet —
    prefer a **second Alloy release** just for Beyla. `source = "socket_filter"` avoids
    clashing with other TC eBPF programs.
  - Value for us: per-pod bytes by peer *without* touching the game — mostly interesting once
    pods talk to each other (region links, §0.7) or a front door exists. On day one cAdvisor +
    L2 cover the same ground.
- **ebpf_exporter (Cloudflare) `udp-drops` example:** UDP drop counts **with a port label**
  from a UDP-specific tracepoint — the one thing the MIB can't give (which port/pod's socket
  overflowed), and it works across namespaces. Cheaper than tracing `kfree_skb` for every
  dropped packet. Worth it only if L1/L2 show buffer drops we can't attribute.
- **Microsoft Retina:** CNI-agnostic eBPF, Prometheus metrics, drop *reasons* (iptables,
  conntrack, …). Heavier; flannel/k3s compatibility not confirmed by anything I found. Keep as a
  fallback if conntrack/NodePort drops become a real investigation.
- Not a fit: Cilium Hubble (k3s here runs flannel), Pixie (heavy, protocol parsing useless on
  QUIC), Kepler (power).

### L4 — Synthetic probe from outside (black-box)

`blackbox_exporter`'s probes (http, tcp, icmp, dns, grpc) cannot open a QUIC connection to a
custom-protocol endpoint; HTTP/3 support exists only in forks/community images I couldn't verify,
and our 6466 endpoint speaks raw QUIC with game frames, not HTTP/3. Proposal: a tiny Rust
**`game-probe`** (quinn client, reuses `game::encode_packet`): connect, send `Hello` with a probe
token (or just complete the handshake and close), measure handshake and first-reply time, expose
`probe_success`, `probe_quic_handshake_seconds` on `/metrics`. Run it:

- in-cluster (Deployment + ServiceMonitor) against each pod's NodePort on the node IP — catches
  pod-down / stale-conntrack black holes (the ~120 s after a restart) exactly as a player hits
  them;
- ideally also from outside the box (another host or a cheap cron somewhere) pushing to
  Prometheus via remote-write or simply scraped through a tunnel — catches the host firewall
  (`deploy/firewall.nft`) and upstream routing. The external vantage is the one that matters
  for "can players reach us".

Same probe can dial 6467 (WebTransport) with wtransport.

## 4. Recommendation

1. **Now, with the first game pod (no game code):** L1 dashboard + alert rules in the
   monitoring module — `rate(node_netstat_Udp_RcvbufErrors[5m]) > 0`, `NoPorts` rising,
   softnet/NIC drops, conntrack > 80 % of limit, per-pod cAdvisor rx/tx and drops.
2. **With R5's `/metrics` endpoint (one change):** L2 — quinn/wtransport `ConnectionStats`
   aggregated into histograms/counters, per-`ServerPacket`-kind bytes, the `[server]` counters
   as series, the pod's own `/proc/net/snmp` UDP line, and one `[conn]` log line per
   disconnect for per-player detail in Loki. This is the part that actually tells us how play
   feels; nothing else can see inside QUIC.
3. **Small follow-up:** L4 `game-probe` for reachability alerts, from inside and outside.
4. **Only on demand:** L3 — Beyla network flows when pod↔pod links exist; `ebpf_exporter`
   `udp-drops` or Retina if unexplained drops appear.

Grafana dashboard sketch ("Game network"): connections by transport; RTT p50/p95/p99
(`histogram_quantile` over `game_conn_rtt_seconds_bucket`); loss % =
`rate(lost_packets) / rate(sent_packets)`; congestion events/s; tx/rx bytes and datagrams per pod
(L2 and cAdvisor side by side — a gap means the kernel or network ate them); bytes by packet kind;
send timeouts; pod-netns and host UDP buffer errors; probe success + handshake time. Keep
dashboards as code in the monitoring module (already a stated follow-up).

## 5. Open questions

- Sampling interval vs. cost: walking every connection's `stats()` once a second is cheap at
  tens of players; re-check at hundreds.
- Do we want exemplars (metric → trace) — then OTLP push via Alloy instead of scraping.
- Where the external probe runs (second host? GitHub Actions cron? a friend's box?).
- Whether the hostNetwork fallback gets used — it swaps which of node-exporter / cAdvisor sees
  the game's UDP (§2).

## Sources

- quinn `ConnectionStats` / quinn-proto `PathStats` (0.11.12): https://docs.rs/quinn/latest/quinn/struct.ConnectionStats.html, https://docs.rs/quinn-proto/latest/quinn_proto/struct.PathStats.html
- wtransport 0.7.2 `Connection` (`rtt`, `quic_connection` behind `quinn`): https://docs.rs/wtransport/latest/wtransport/struct.Connection.html
- node_exporter collectors (udp_queues, netstat, softnet default-on): https://github.com/prometheus/node_exporter ; netstat default fields: https://sources.debian.org/src/prometheus-node-exporter/1.9.1-1/collector/netstat_linux.go
- Alloy `beyla.ebpf`: https://grafana.com/docs/alloy/latest/reference/components/beyla/beyla.ebpf/ ; Beyla network metrics: https://grafana.com/docs/beyla/next/network/config/ ; Beyla vs OBI: https://grafana.com/docs/beyla/latest/obi.md
- ebpf_exporter udp-drops / kfree_skb discussion: https://lists.openwall.net/netdev/2023/07/14/260 , https://www.github.com/cloudflare/ebpf_exporter/releases
- Retina: https://techcommunity.microsoft.com/blog/linuxandopensourceblog/ebpf-powered-observability-beyond-azure-a-multi-cloud-perspective-with-retina/4403361
- HTTP/3 probing gap in blackbox_exporter: https://engineered.at/articles/from-custom-to-open-scalable-network-probing-and-http-3-readiness-with-prometheus
