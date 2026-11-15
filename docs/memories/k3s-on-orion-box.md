---
name: k3s-on-orion-box
description: "2026-11-06 k3s v1.37.2 installed on 5.9.7.138 (Traefik on 80/443); all docker containers (Caddy, mmo-server, survival-world stack) stopped, compose deploy deprecated"
metadata:
  node_type: memory
  type: project
  originSessionId: 3feb1d7a-9170-4c40-b77d-08765eb1d066
  modified: 2026-11-06T15:18:23.238Z
---

On 2026-11-06 k3s v1.37.2+k3s1 was installed on the Hetzner box root@5.9.7.138 (hostname + node `hetzner-ryzen5-3600-1`, renamed from `survival-world` same day; AMD Ryzen 5 3600): `--node-external-ip 5.9.7.138 --flannel-iface enp4s0`, bundled Traefik (LoadBalancer via ServiceLB on 80/443), local-path storage, metrics-server. Kubeconfig `/etc/rancher/k3s/k3s.yaml` (mode 600); API 6443 closed from outside by the host firewall (use an SSH tunnel). Host firewall `hostfw` (source: ~/Projects/mod/mods/survival-world/deploy/firewall.nft) now also accepts `cni0`/`flannel*`.

Every docker container there (survival-world Caddy proxy + stack, mmo-web Caddy, mmo-server) was stopped with restart=no, not removed; the user called Caddy/compose deprecated: "we will deploy on this k3s now".

**Why:** the user decided the box moves to k8s-native deploys (see [[multi-instance-k8s-postgres-research]]).
**How to apply:** `just deploy` / deploy/web/compose.yml in the mmo repo is stale until ported to k3s; the server's WebTransport cert came from Caddy's data dir (cert-manager or Traefik ACME needed now). Note: Agones 1.62 lists K8s 1.34–1.36 only; this is 1.37.
