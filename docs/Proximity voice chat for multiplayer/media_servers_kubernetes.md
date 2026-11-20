# Self-hostable voice/media servers for proximity voice, and running them on Kubernetes (as of 2026-11)

Scope note: about 24 tool calls were spent. Jitsi Videobridge, Nakama, Agora, coturn and the `livekit-helm` values file were NOT researched from primary sources in this pass — they appear only under Gaps. Anything marked "(older material)" could not be confirmed against a 2026 source.

## LiveKit (open-source SFU)

### Takeaway
LiveKit is the best-documented self-hostable option: Apache-2.0, actively released (v1.13.11, late October 2026), official Helm chart, a single-UDP-port mode, TCP and embedded-TURN fallbacks, a first-party Rust SDK and a browser SDK, plus server APIs to control who subscribes to whom. Its Kubernetes model is host networking with one pod per node, which fits a single-node k3s box well; nothing needed for proximity voice is Cloud-only (Cloud adds a global mesh, enhanced noise cancellation, observability and agent hosting).

### Cited Findings
**Version, licence, activity**
- Latest release is v1.13.11 (25 Oct, marked Latest): adds `rtc.rtx_budget`, upstream FlexFEC03, negotiated data channels in single-PC mode, rebuilt with Go 1.27.3 and a patched Alpine image. Preceding releases v1.13.10 (14 Oct), v1.13.9 (7 Oct), v1.13.8 (6 Oct), v1.13.7 (14 Sep), v1.13.6 (26 Aug) — roughly 2–4 releases a month. The page shows day and month only; the year is inferred as 2026 from the 2026 sequence below. — [GitHub releases](https://github.com/livekit/livekit/releases)
- 2026 sequence: v1.10.0 (23 Mar 2026), v1.11.0 (17 Apr 2026, data tracks on by default), v1.12.0 (16 May 2026, TURN auth change), v1.13.0/1.13.1 (8 Jun 2026, removes backwards compatibility for TURN authentication without TTL). — [pkg.go.dev v1.13.0](https://pkg.go.dev/github.com/livekit/livekit-server@v1.13.0), [releases.sh changelog](https://releases.sh/livekit/livekit-server/changelog)
- "LiveKit server is licensed under Apache License v2.0." — [README](https://raw.githubusercontent.com/livekit/livekit/master/README.md)
- Features listed in the README: distributed WebRTC SFU, JWT authentication, UDP/TCP/TURN connectivity, single binary / Docker / Kubernetes, speaker detection, selective subscription, moderation APIs, end-to-end encryption, data tracks, webhooks, distributed multi-region. SDKs listed include Browser (client-sdk-js), Rust, C++, Unity, Unity WebGL, Swift, Android, Flutter, React Native, ESP32, Python, Node.js. Unreal is not listed. — [README](https://raw.githubusercontent.com/livekit/livekit/master/README.md)

**Kubernetes**
- The Helm chart sets up a distributed deployment plus Service and Ingress; "Our Helm chart supports Google GKE, Amazon EKS, and Digital Ocean DOKS out of the box". — [Kubernetes docs](https://docs.livekit.io/home/self-hosting/kubernetes/)
- "LiveKit pods requires direct access to the network with host networking." and consequently "we'll be limited to one LiveKit pod per node"; other workloads can still run on those nodes. — [Kubernetes docs](https://docs.livekit.io/home/self-hosting/kubernetes/)
- "LiveKit does not support deployment to serverless and/or private clusters" (extra NAT layers). — [Kubernetes docs](https://docs.livekit.io/home/self-hosting/kubernetes/)
- "Termination of TLS/SSL is left as a responsibility of the Ingress"; the embedded TURN/TLS server still needs its own certificate, imported as a Kubernetes TLS Secret in the deployment's namespace. Needed: "SSL certificates for primary domain and TURN/TLS." — [Kubernetes docs](https://docs.livekit.io/home/self-hosting/kubernetes/)
- Graceful drain: old pods drain active sessions on upgrade; the chart sets `terminationGracePeriodSeconds` to 5 hours. ConfigMap changes do not restart pods. — [Kubernetes docs](https://docs.livekit.io/home/self-hosting/kubernetes/)
- Redis is a listed prerequisite for a multi-node deployment; the deployment page calls Redis recommended for production (`redis.address`). — [Kubernetes docs](https://docs.livekit.io/home/self-hosting/kubernetes/), [Deployment docs](https://docs.livekit.io/home/self-hosting/deployment/)
- The STUNner project's own LiveKit example states the recommended way to install LiveKit into Kubernetes is host-network deployment. — [STUNner LiveKit example (v0.18.1, older material)](https://github.com/l7mp/stunner/blob/v0.18.1/docs/examples/livekit/README.md)

**Ports and UDP handling**
- API/WebSocket 7880 (`port`, behind an SSL-terminating LB); ICE/UDP 50000–60000 (`rtc.port_range_start`/`rtc.port_range_end`, "Each participant uses two ports"); ICE/TCP 7881 (`rtc.tcp_port`, fallback when UDP fails); ICE/UDP mux 7882 (`rtc.udp_port`, optional single port for all UDP — "When this is set, rtc.port_range_start/end are not used"); TURN/TLS 5349 (`turn.tls_port`; "If not using LB, this port needs to be set to 443"); TURN/UDP 3478 (`turn.udp_port`, embedded TURN, also serves STUN). — [Ports and firewall](https://docs.livekit.io/home/self-hosting/ports-firewall/)
- `rtc.use_external_ip: true` is recommended where the host has a public IP the process cannot see; LiveKit discovers it by STUN and advertises it. — [Deployment docs](https://docs.livekit.io/home/self-hosting/deployment/)
- The domain certificate "must be signed by a trusted certificate authority; self-signed certs do not work here"; TURN needs a separate domain and certificate. — [Deployment docs](https://docs.livekit.io/home/self-hosting/deployment/)
- Recent hardening relevant to operators: v1.13.6 added limits on signalling message size, TURN allocations per participant, pending tracks; v1.13.7 added PROXY protocol on the TURN TCP listener; v1.13.10 makes TURN refuse TCP relays. — [GitHub releases](https://github.com/livekit/livekit/releases)

**Capacity / benchmarks**
- Official audio-only benchmark on a 16-core GCP `c2-standard-16`: 10 publishers, 3000 subscribers, 7.3 kBps in / 23 MBps out, 305 / 959,156 packets/s in/out, 80% CPU; assumes about 3 kbps average audio bitrate and roughly 10 active speakers. — [Benchmark docs](https://docs.livekit.io/home/self-hosting/benchmark/)
- Each room must fit on a single node; scaling is by adding nodes for more rooms. Load is generated with `lk load-test --audio-publishers 10 --subscribers 1000`. — [Benchmark docs](https://docs.livekit.io/home/self-hosting/benchmark/)
- Scalability is bounded by CPU and bandwidth; compute-optimized instances and host networking are recommended. — [Deployment docs](https://docs.livekit.io/home/self-hosting/deployment/)

**Subscription control, data, spatial audio, metrics**
- Selective subscription: connect with `autoSubscribe: false` (Rust: `options.auto_subscribe = false`) and call `publication.setSubscribed(true)` per track. Server-side: `UpdateSubscriptions` (`api.room.updateSubscriptions(room, identity, trackSids, subscribe)`; available in Go, Node, Python, Ruby, Kotlin and Rust server SDKs). — [Subscribing to tracks](https://docs.livekit.io/home/client/tracks/subscribe/)
- Subscribe/unsubscribe needs a server negotiation handshake; `setEnabled(false)` on a subscribed track stops media without renegotiation, so it "can be more efficient for frequent toggling". "A track must be subscribed to and enabled for data to be received." — [Subscribing to tracks](https://docs.livekit.io/home/client/tracks/subscribe/)
- v1.13.2 "enforces subscription permissions for data tracks" (confirms subscription permissions exist server-side) and added Prometheus metrics for join latency and peer-connection state. — [GitHub releases](https://github.com/livekit/livekit/releases)
- Prometheus metrics are served at `:6789/metrics` when `prometheus_port` is set (commented out in the sample config). — [Deployment docs](https://docs.livekit.io/home/self-hosting/deployment/)
- Spatial audio is client-side only: LiveKit's official example is a 2D world where the frontend pans/attenuates each remote track with WebAudio (`SpatialAudioController.tsx`); LiveKit itself only gates room access and forwards tracks. — [LiveKit blog tutorial](https://blog.livekit.io/tutorial-using-webrtc-react-webaudio-to-create-spatial-audio), [example repo mirror](https://git.tdem.in/livekit-examples/spatial-audio)

**Cloud vs self-hosted**
- Self-hosted is a single-home SFU (one server per room); Cloud is a global mesh SFU. The docs say the server is the same, and "everything around the server: agent tooling, hosting, inference, telephony, global scaling, and observability" is native to Cloud. Code can move between the two without rewrites. — [LiveKit Cloud docs](https://docs.livekit.io/intro/cloud/), [Cloud vs self-host blog](https://livekit.com/blog/livekit-cloud-vs-self-host)
- Enhanced noise cancellation (Krisp/BVC) is Cloud-only; self-hosted gets the client's WebRTC noise suppression, or ai-coustics with your own licence key. — [LiveKit community thread](https://community.livekit.io/t/noise-cancelling-features-with-self-hosted-agents/1227), [Cloud noise cancellation docs](https://docs.livekit.io/home/cloud/noise-cancellation)

### Inferences
- From the official benchmark (12.8 cores busy for ~960k outbound packets/s), LiveKit forwards on the order of 75k audio packets/s per core. A proximity listener hearing 5 simultaneous talkers at 50 packets/s each costs ~250 packets/s, so a rough ceiling is a few hundred concurrently-listening players per core — an estimate from one published data point, not a measured proximity benchmark. The benchmark's "3 kbps average" implies mostly-silent (DTX) streams; continuous talk at 24–32 kbps would be bandwidth-heavier but similar in packet count.
- Because a room lives on one node and subscription is per track, "one room per game region (or per host), all clients `autoSubscribe: false`, game server calls `UpdateSubscriptions` from its authoritative positions" is the natural mapping for many small overlapping proximity groups; overlapping groups are not a problem for an SFU because there is no group object, only per-listener track sets. Toggling `setEnabled` client-side is cheaper than (un)subscribing but trusts the client, so anti-eavesdropping needs the server-side call or subscription permissions.
- On a single-node k3s box: hostNetwork + `rtc.udp_port: 7882` + `rtc.tcp_port: 7881` + `use_external_ip: true` means exactly two extra firewall openings (one UDP, one TCP) plus the existing ingress for the WebSocket signalling; no Redis is needed until a second LiveKit node exists. The one-pod-per-node rule means horizontal scale = more nodes, not more replicas.
- The 5-hour termination grace period is sized for meetings; for a game it should be lowered, and a rolling upgrade on a single node necessarily drops voice (the new pod cannot bind the host ports until the old one exits).

### Gaps
- The `livekit-helm` chart version and values (e.g. whether it has a first-class single-node / k3s / Traefik path) were not read; the docs name only GKE, EKS and DOKS.
- JWT grant fields (`canSubscribe`, `canPublish`, `canPublishData`, `roomJoin`) and the client-side `setTrackSubscriptionPermissions` API were not confirmed from a fetched page in this pass — the subscribe page did not mention them. They exist in my prior knowledge of LiveKit but are unsourced here.
- No documented hard per-room participant limit was found for self-hosted; no official benchmark for many-publisher audio rooms (e.g. 200 publishers each heard by 5–10 listeners).
- Data channel details (reliable/lossy, size limits) and the newer "data tracks" feature were not researched beyond release-note mentions.
- Whether the browser SDK can be driven from a Rust wasm client without the JS SDK (the Rust SDK's wasm32 support status) was not checked.

## Other self-hostable servers (mediasoup, Janus, Galène, Pion/ion-sfu, Mumble, Rust-native, Jitsi, Nakama)

### Takeaway
After LiveKit, the credible self-hosted choices are mediasoup (a library with a first-party Rust crate — you write the signalling and the server), Janus (the only mainstream one with server-side mixing, AudioBridge, including basic stereo panning) and Galène (small, simple Go SFU). ion-sfu is dead, Mumble has no viable browser client, and the Rust-native servers (atm0s-media-server, str0m-based kits) are pre-1.0 or mid-refactor.

### Cited Findings
**mediasoup**
- mediasoup 3.20.0 was announced together with Rust crates (`mediasoup` 0.22.0, `mediasoup-sys` 0.12.0, `mediasoup-types` 0.4.0) and a new built-in SCTP stack; the announcement snippet carried no date. — [mediasoup forum](https://mediasoup.discourse.group/t/mediasoup-3-20-0-released-and-rust-crates-with-new-built-in-sctp-stack/6974)
- OpenVidu's July 2026 release notes say it moved to mediasoup v3.19.21, so the 3.19 line was current in mid-2026 (secondhand). — [OpenVidu 3.8.0](https://openvidu.io/blog/2026/07/09/release-380/)
- Licence is listed as ISC by a third-party index (not verified against the repo's LICENSE file). — [ecosyste.ms](https://opencollective.ecosyste.ms/projects/4141)
- `WebRtcServer` (since 3.10.0) lets a worker listen on a single port instead of a port per transport. — [mediasoup forum](https://mediasoup.discourse.group/c/announcements/9?page=1)

**Janus**
- Janus is GPLv3, with commercial licensing available from Meetecho; a directory page reports the last commit on 27 Jul 2026 and lists it as actively maintained. npm READMEs exist for janus-gateway 1.3.0 and 1.4.1. — [README via jsDelivr](https://cdn.jsdelivr.net/gh/meetecho/janus-gateway@master/README.md), [selfhostyourself](https://selfhostyourself.com/services/janus-gateway), [unpkg 1.4.1](https://unpkg.com/janus-gateway@1.4.1/README.md)
- AudioBridge is a mixing conference bridge: Opus only, each participant receives the room mix. `spatial_audio: true` makes the mix stereo and each participant gets a `spatial_position` from 0 (left) to 100 (right), settable at join or via `configure`. Mixer `sampling_rate` defaults to 16000 for API-created rooms. Opus `quality` 0–10 (default 4) trades CPU for quality; `expected_loss` enables FEC; per-participant RNNoise `denoise`; suspending a participant removes them from the mix and saves CPU. — [AudioBridge docs](https://janus.conf.meetecho.com/docs/audiobridge.html)
- AudioBridge `groups` affect only RTP forwarders, "not the room's main mix or how participants are mixed". — [AudioBridge docs](https://janus.conf.meetecho.com/docs/audiobridge.html)
- Janus rooms are pinned to specific instances rather than load-balanced. — [webrtc.ventures, Aug 2026](https://webrtc.ventures/2026/08/stunner-production-kubernetes/)

**Galène**
- Galène 1.1 was released 21 June 2026, 1.2 on 5 September 2026 and 1.2.1 on 8 September 2026 — actively maintained. — [Galène list: 1.2 announce](https://lists.galene.org/galene/87h5k1gnix.fsf@toke.dk/t/), [Galène list archive](https://lists.galene.org/galene/87fr6jhh70.wl-jch@irif.fr)

**Pion / ion-sfu**
- The `ionorg/ion` repo was archived in December 2023; `ion-avp` was archived in July 2021; the `cloudwebrtc/ion-sfu` Go package has had no release in over four years. A separate `pion/ion` repo describes itself as a work-in-progress ION SFU remake with pushes in January 2026. — [ionorg/ion releases](https://www.github.com/ionorg/ion/releases), [pion/ion-avp](https://github.com/pion/ion-avp), [ecosyste.ms ion-sfu versions](https://packages.ecosyste.ms/registries/proxy.golang.org/packages/github.com%2Fcloudwebrtc%2Fion-sfu/versions), [ecosyste.ms pion/ion](https://awesome.ecosyste.ms/api/v1/projects/github.com%2Fpion%2Fion)

**Mumble / Murmur**
- Mumble 1.6.870 (RC) was posted 1 March 2026 (new database backend, a shortcut to toggle positional audio); no final 1.6 announcement was found. Stable line is 1.5.x (1.5.735, 1.5.857). — [Mumble 1.6.870 RC](https://www.mumble.info/blog/mumble-1.6.870-rc/), [Mumble 1.5.735](https://www.mumble.info/blog/mumble-1.5.735)
- Positional audio is a client plugin model: since 1.4 there is a general plugin framework; 1.5.634 added a Positional Audio Viewer and raised the maximum distance thresholds. — [Mumble 1.5.634](https://www.mumble.info/blog/mumble-1.5.634), [Mumble 1.4.230](https://www.mumble.info/blog/mumble-1.4.230/)
- Browser: `mumble-web` uses WebSockets for control and WebRTC for voice, but needs a proxy or a server with WebRTC support, "which neither websockify nor Grumble provides"; Murmur itself has an open request for a WebSocket transport. The found Docker images are about five years old and the original README says its WebRTC library is probably broken. — [Netzbegruenung webrtc branch](https://git.netzbegruenung.de/NB-Public/mumble-web/src/branch/webrtc), [Mumble issue 2131](https://www.goodfirstissue.org/mumble-voip/mumble/issues/2131), [Docker Hub lukas1818](https://hub.docker.com/r/lukas1818/mumble-web-webrtc), [Rantanen/mumble-web](https://github.com/Rantanen/mumble-web)

**Rust-native**
- atm0s-media-server (8xFF): MIT, 332 stars, 313 commits, empty Releases section. The README says the project "is in a refactoring-era state around `sans-io-runtime`"; implemented integrations are the WebRTC SDK protobuf APIs, WHIP, WHEP, RTPengine-style APIs, recording, multi-tenancy; RTMP and SIP "are not current in-repo server modes". Runs as one binary in `standalone` or split console/gateway/connector/media modes with an SDN overlay. No Kubernetes or Helm references in the repo page. — [atm0s-media-server](https://github.com/8xFF/atm0s-media-server)
- An `8xff/atm0s-media-stack` Helm chart exists on the DigitalOcean Marketplace (Kubernetes >= 1.23, Helm 3); the catalog page was last generated April 2025 (older material). The core crate's last listed version is 0.1.0-alpha.2 (8 Feb 2025). — [DigitalOcean catalog](https://docs.digitalocean.com/products/marketplace/catalog/atm0s-media-stack), [lib.rs](https://lib.rs/crates/atm0s-media-server-core)
- str0m is a sans-I/O Rust WebRTC library written for server-SFU use (used that way at Lookback); docs.rs lists versions up to 0.23.0; its repo ships a chat example acting as an SFU. Small projects build on it (`oxpulse-sfu-kit` 0.11.5, `switchboard`). — [docs.rs str0m](https://docs.rs/str0m), [str0m 0.23.0](https://docs.rs/crate/str0m/0.23.0), [oxpulse-sfu-kit](https://docs.rs/oxpulse-sfu-kit)

### Inferences
- Suitability for many small overlapping proximity groups: SFUs with per-consumer control (LiveKit, mediasoup) fit directly — a "group" is just each listener's current set of subscribed producers. Janus AudioBridge does not: a room is one mix, `groups` do not partition the participant mix, so overlapping proximity sets would need one room per listener-neighbourhood or constant room hopping; its left/right-only `spatial_position` is also per-room, not per-listener, so it cannot express listener-relative direction.
- mediasoup is the natural fit if the voice server should be embedded in, or written beside, the Rust game server: the Rust crate makes the game server itself able to create producers/consumers from authoritative positions, and `WebRtcServer` gives the same single-port Kubernetes story as LiveKit's UDP mux. The cost is that signalling, auth, reconnection, TURN and a native Rust client stack (mediasoup has no first-party Rust *client*; a native client would need libwebrtc or str0m/webrtc-rs) are all yours to build.
- Mumble is ruled out by the browser requirement, not by quality: its native client/positional model is strong but the browser path depends on dormant third-party proxies.
- A str0m-based custom SFU inside the game server process is feasible for audio-only (Opus forwarding needs no simulcast/SVC logic) and would reuse the existing QUIC/WebTransport-style architecture thinking, but it is a build-not-buy decision with no off-the-shelf operations story.

### Gaps
- Jitsi Videobridge: not researched in this pass (no source fetched on current version, Kubernetes story or audio-only suitability). Only fact found: its media-session ownership is decided by Jicofo — [webrtc.ventures](https://webrtc.ventures/2026/08/stunner-production-kubernetes/). Prior knowledge (unsourced here): Apache-2.0, requires Prosody + Jicofo, heavyweight for a game.
- Nakama (or other game backends) with built-in voice: not researched; I found no evidence either way in this pass.
- mediasoup: exact latest version/date, the licence from the repo itself, and Kubernetes port guidance were not confirmed from primary pages.
- Janus: exact current 1.x version and date not confirmed; no published AudioBridge CPU-per-participant benchmark found.
- Galène: licence (MIT in prior knowledge, unsourced here), native-client story and Kubernetes guidance were not fetched.
- atm0s-media-server: last commit date not visible; whether its earlier "audio mix-minus"/spatial features survive the refactor is unknown; no Rust client SDK was verified.
- Cloudflare-style or other hosted SFUs are covered in the managed section below.

## SFU vs MCU/mixing for proximity voice

### Takeaway
For proximity voice with per-listener spatialization an SFU is the fit: the listener needs each talker as a separate stream to pan and attenuate it, and forwarding Opus costs the server no codec work. Mixing (Janus AudioBridge) saves client bandwidth and decode work but costs a decode + mix + encode per participant on the server and collapses direction to at most a room-wide left/right pan.

### Cited Findings
- Janus AudioBridge decodes and mixes Opus server-side, delivers one mix per participant, and exposes CPU-relevant knobs (`quality` complexity, `sampling_rate`, suspend-to-save-CPU), which shows the per-participant transcoding cost is real. — [AudioBridge docs](https://janus.conf.meetecho.com/docs/audiobridge.html)
- AudioBridge's spatial support is a stereo mix with a fixed `spatial_position` 0–100 per participant for the whole room. — [AudioBridge docs](https://janus.conf.meetecho.com/docs/audiobridge.html)
- LiveKit's SFU forwarded audio to 3000 subscribers at 80% of 16 cores, with outbound 23 MBps for ~10 active speakers at ~3 kbps average. — [LiveKit benchmark](https://docs.livekit.io/home/self-hosting/benchmark/)
- LiveKit's spatial-audio example does all spatialization in the client with WebAudio from separate per-participant tracks. — [LiveKit blog tutorial](https://blog.livekit.io/tutorial-using-webrtc-react-webaudio-to-create-spatial-audio)

### Inferences
- Bandwidth per listener with an SFU scales with audible talkers: N streams × (Opus 16–32 kbps + roughly 16–24 kbps of IP/UDP/SRTP/RTP header overhead at 50 packets/s). With 5 audible talkers that is on the order of 150–250 kbps down while all talk at once; DTX cuts silent talkers to near zero, which is why real averages (LiveKit's "3 kbps average") are far lower. Capping audible talkers (nearest N, or loudest N via audio-level/speaker detection) bounds the worst case. These figures are my arithmetic, not from a source.
- With mixing, listener bandwidth is one stream regardless of crowd size, but the server must produce a different mix per listener for true proximity (each listener has a different audible set and different gains), i.e. one encode per listener — the AudioBridge "one room, one mix-minus" model does not do that.
- Server-side culling (subscribe only to talkers within range) is needed in the SFU model for both bandwidth and privacy: a client that receives a distant talker's stream can play it regardless of client-side attenuation.

### Gaps
- No published head-to-head CPU numbers (SFU forward vs AudioBridge mix per participant) were found in this pass.
- Opus/DTX bitrate and overhead figures above are unsourced calculations; the report writer should source them from the codec researcher's notes if available.

## Kubernetes networking for WebRTC/UDP media

### Takeaway
On a single-node k3s box the simple, vendor-recommended answer is hostNetwork with a single muxed UDP port (plus a TCP fallback port), advertising the node's public IP; signalling goes through the existing ingress with a real certificate. STUNner (a Gateway-API TURN gateway) is worth it only when media pods must stay off the host network or a cloud load balancer must front many media pods — it adds a relay hop, shared-secret management and its own failure modes.

### Cited Findings
- LiveKit requires host networking on Kubernetes and binds its RTC ports on the node directly; one pod per node. — [LiveKit Kubernetes docs](https://docs.livekit.io/home/self-hosting/kubernetes/)
- A single UDP port (`rtc.udp_port` 7882) can carry all media instead of the 50000–60000 range; ICE/TCP 7881 and TURN/TLS (443 if no LB) are the fallbacks for UDP-blocked clients. — [LiveKit ports](https://docs.livekit.io/home/self-hosting/ports-firewall/)
- mediasoup's `WebRtcServer` gives the same single-port listening model. — [mediasoup forum](https://mediasoup.discourse.group/c/announcements/9?page=1)
- STUNner exposes "one HTTPS port for signaling and a *single* UDP port for *all* your media"; v1.0.0 was the first GA release and v1.1.0 removed legacy mode and made the Helm chart install the gateway operator; the v1.1.0 notes announce an upcoming commercial edition. — [l7mp/stunner](https://github.com/l7mp/stunner), [v1.0.0](https://newreleases.io/project/github/l7mp/stunner/release/v1.0.0), [v1.1.0](https://newreleases.io/project/github/l7mp/stunner/release/v1.1.0)
- STUNner in production (Aug 2026 write-up): it is a TURN relay, not a load balancer ("STUNner does not load balance media"); media pods stay on ClusterIP; each Gateway becomes a `LoadBalancer` Service (one cloud LB per Gateway); `externalTrafficPolicy: Local` is needed to preserve client IPs for STUN; use ephemeral TURN REST credentials (HMAC-SHA1 with a shared key held by both stunnerd and the media server — a mismatch gives unauthorized failures "often with no clear error"); restarting stunnerd drops in-memory allocations and so active calls; UDP+TCP on the same port needs `stunner.l7mp.io/enable-mixed-protocol-lb: "true"` plus a cloud-specific annotation, else only the first listener survives. — [webrtc.ventures](https://webrtc.ventures/2026/08/stunner-production-kubernetes/)
- k3s ServiceLB (Klipper) works by binding node host ports and forwarding to the Service; it conflicts with MetalLB if both are enabled. — [OneUptime k3s ServiceLB](https://oneuptime.com/blog/post/2026-02-02-k3s-service-load-balancer/view), [OneUptime MetalLB on k3s](https://oneuptime.com/blog/post/2026-02-20-metallb-k3s-servicelb-conflicts/markdown)
- A single-node LiveKit deployment guide (Docker Swarm, not k3s) notes the single-node case "works without any additional configuration" because ports are published on the only node and DNS points there. — [Co-op Cloud lasuite-meet](https://git.coopcloud.tech/coop-cloud/lasuite-meet/src/branch/main/docs/multinode.md)
- Signalling TLS must be a CA-signed certificate terminated at the ingress/LB; TURN/TLS needs its own domain and certificate. — [LiveKit deployment](https://docs.livekit.io/home/self-hosting/deployment/)

### Inferences
- Why not NodePort/LoadBalancer for media: ICE candidates carry the address and port the server believes it has, so a Service that remaps ports or SNATs breaks the candidate unless the server is told its external address/port explicitly; a large UDP range cannot practically be expressed as Service ports. A single muxed UDP port makes a `hostPort` or a ServiceLB UDP entry technically possible, but hostNetwork is what both LiveKit and STUNner's own LiveKit guide recommend, and it avoids kube-proxy/conntrack in the media path.
- For the first target (one k3s node with a public IP): open one UDP port (media mux) and one TCP port (ICE/TCP fallback) in the host firewall, keep WebSocket signalling on the existing HTTPS ingress, set the advertised IP (STUN discovery or static). TURN/TLS on 443 conflicts with the ingress on a single IP — it would need a second IP, SNI-based TCP passthrough at the ingress, or be skipped in favour of ICE/TCP on 7881 (which fails only for clients restricted to 443).
- Browser clients already tunnel game traffic over WebTransport (QUIC/UDP 443 or similar), so a player whose network blocks arbitrary UDP likely cannot play at all; this weakens the case for TURN/TLS in this specific game.
- Growth path: more nodes → more LiveKit pods (one per node) + Redis; each node needs a public IP reachable by clients, or STUNner/coturn in front. STUNner becomes attractive mainly on managed clouds where nodes have no public IPs.

### Gaps
- No primary source was fetched on ServiceLB's UDP behaviour or on Traefik UDP entrypoints; the claim that they "cannot proxy the UDP media range well" is consistent with the above but not directly sourced.
- coturn on Kubernetes (deployment pattern, hostNetwork, relay port range) was not researched.
- STUNner's current version number/date, licence and what the commercial edition gates were not confirmed.
- No measured latency/CPU overhead figure for the STUNner relay hop was found.

## Commercial / managed alternatives (comparison only)

### Takeaway
Managed voice is cheap or free at small scale (Vivox free to 5,000 PCU, Discord Social SDK free, Cloudflare Realtime SFU $0.05/GB after 1,000 GB), but none is self-hosted on the operator's Kubernetes; 4Players ODIN is the only game-voice product found that advertises an on-premise server, at quote-based pricing.

### Cited Findings
- Vivox (Unity): the first 5,000 PCU per month per organization are free on standard pricing; PCU of all projects in the organization are summed. — [Unity support](https://support.unity.com/hc/en-us/articles/31045802890260), [Unity Vivox page](https://activation.unity3d.com/vivox)
- A competitor blog claims exceeding 5,000 PCU bills $2,000 for the month — unverified, treat with caution. — [fiveminutes.io](https://fiveminutes.io/articles/fiveminute-chat-vs-vivox)
- Discord Social SDK is free; voice and text are available but rate-limited; launch support was C++, Unreal and Unity on Windows 11+ and macOS, with later coverage saying PC, console and mobile. No browser support was found. — [Discord Social SDK overview](https://docs.discord.com/developers/discord-social-sdk/overview.md), [TechCrunch, Mar 2025](https://techcrunch.com/2025/03/17/discord-launches-sdk-to-help-developers-enhance-social-experiences-in-their-games), [discord.com/social-sdk](https://discord.com/social-sdk)
- Cloudflare Realtime SFU (formerly Calls; not self-hosted): $0.05 per GB egress, first 1,000 GB/month free, shared with TURN; ingress free. — [Cloudflare Realtime SFU pricing](https://developers.cloudflare.com/realtime/sfu/pricing)
- 4Players ODIN: offers an on-premise/self-hosted server (downloadable server plus a licence from the dashboard), 1,000 concurrent users per self-hosted instance (more on request); self-host is listed under the Enterprise tier with custom pricing; one published managed tier is €49.90/month scaling to 500 concurrent users. Sources conflict on whether self-host is a standard option or enterprise-only. — [ODIN pricing](https://odin.4players.io/pricing/), [ODIN voice chat](https://odin.4players.io/voice-chat/), [ODIN plans FAQ](https://4players.io/odin/faq/general/what-plans-do-you-offer), [odin-server repo](https://github.com/4Players/odin-server), [ODIN SDK blog](https://4players.io/blog/odin-what-the-new-sdk-can-do-for-you)
- LiveKit Cloud has a free Build plan (no credit card) and runs in 19+ regions; code is portable to and from self-hosted. — [LiveKit README](https://raw.githubusercontent.com/livekit/livekit/master/README.md), [Cloud vs self-host](https://livekit.com/blog/livekit-cloud-vs-self-host)

### Inferences
- Cloudflare's 1,000 GB free egress is large for voice: at ~40 kbps per forwarded stream including overhead, 1,000 GB is on the order of 55,000 stream-hours per month (my arithmetic). It is the cheapest managed fallback, but its SFU is a low-level API with no rooms/auth layer.
- ODIN's self-hosted server is proprietary and licence-keyed, so it satisfies "runs on our Kubernetes" but not "open source"; LiveKit remains the only option found that is both open source and operationally complete.

### Gaps
- Agora and Dolby.io pricing were not researched.
- Discord Social SDK's current voice rate limits and any browser/wasm support were not confirmed.
- ODIN's server licence terms, container/Kubernetes packaging and browser SDK status were not read from primary docs.
- Vivox overage pricing above the free tier was not confirmed from Unity.
