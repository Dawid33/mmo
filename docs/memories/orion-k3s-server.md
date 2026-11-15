---
name: orion-k3s-server
description: "Hetzner 5.9.7.138 (kube context `orion`) runs k3s; observability stack APPLIED 2026-11-06 via deploy/envs/orion (OpenTofu, S3 state); Caddy off for good"
metadata:
  node_type: memory
  type: project
  originSessionId: de2edf95-6020-4a03-aadb-4acf2ec495f5
  modified: 2026-11-06T16:47:55.282Z
---

The Hetzner box **5.9.7.138** (`hetzner-ryzen5-3600-1`: 12 CPU, 64 GiB, ~1.9 TB) runs **k3s
v1.37.2** since 2026-11-06. Kube context **`orion`** (API over the operator VPN since 2026-11-15, `10.99.0.1:6443`; fallback ssh tunnel
`just tunnel`). Built in: Traefik (ServiceLB 80/443), `local-path`, metrics-server.
**Caddy is off and stays off**: the old compose web deploy and the WebTransport cert from Caddy's
directory no longer work — the game cert must come from cert-manager (not done yet).

**Observability stack is live** (2026-11-06, develop 43039c6 + b9a675a), OpenTofu roots
`deploy/envs/orion/{bootstrap,platform,monitoring}`, state in
`s3://mmo-orion-tfstate-029905505680/orion/<root>.tfstate` (eu-central-1), `just orion-*` recipes.
Grafana https://grafana.factoriosurvivalworld.com (user `admin`, password `just
orion-grafana-password`); the DNS record belongs to survival-world's infra, not this repo.
Remaining check: Thanos S3 upload after ≥2 h + a query past Prometheus' 2d retention.

**Why:** everything (observability, then CNPG / login / panel / worlds) runs on this one server;
see [[multi-instance-k8s-postgres-research]].
**How to apply:** always `--context orion`; plan before every apply and show the first plan.
Gotchas found: kps subcharts ignore `fullnameOverride` (Grafana svc is
`kube-prometheus-stack-grafana`); Grafana needs ≥1 GiB limit on first start; IAM tag values
reject parentheses; `just` recipes run in sh (no `${@:2}`). Related: [[local-deployment-env]].
