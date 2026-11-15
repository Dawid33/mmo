---
name: local-deployment-env
description: "Local deployment env (OpenTofu + single-node k0s-in-docker + Agones + CloudNativePG) IMPLEMENTED + verified 2026-09-22; user wants OpenTofu, not Terraform"
metadata:
  node_type: memory
  type: project
  originSessionId: 4254817d-bd4a-4d8e-8b65-38658be9966c
  modified: 2026-09-22T18:26:39.981Z
---

2026-09-22: user wants deployments declared in **OpenTofu** (`tofu`, v1.12.6 installed; they
corrected "terraform" → "use opentofu"). Built and verified: `deploy/modules/{platform,world}` +
`deploy/envs/local/{platform,world}`; `just dev-cluster-up | deploy-local | deploy-local-plan |
kube | dev-cluster-down | dev-cluster-destroy`. Spec with findings:
`docs/superpowers/specs/2026-09-22-local-deployment-environment-design.md`. Uncommitted.

Verified: Agones 1.60 on k0s v1.35.8 in docker — two GameServers, Dynamic UDP port pairs,
UDP echo from host OK (the §10a feasibility test, passed); CNPG Postgres 18.4 healthy; re-plan =
No changes; Fleet with a pause stand-in + curl sidecar reached Ready, stayed healthy.

Bare k0s gotchas: no LoadBalancer (Agones allocator/ping hang helm --wait → ClusterIP / ping off),
no default StorageClass (OpenEBS LocalPV), disk-pressure taint at <15% free on this laptop
(absolute eviction thresholds), multi-container GameServer needs `spec.container`.

**Why:** production target is single-node k0s; deploy via OpenTofu.
**How to apply:** real Fleet needs server changes first (bind 0.0.0.0, learn Agones host ports,
container image, cert-hash over HTTP) — multi-instance stage 3. Related:
[[multi-instance-k8s-postgres-research]].
