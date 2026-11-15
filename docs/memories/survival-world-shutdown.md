---
name: survival-world-shutdown
description: 2026-11-02 Factorio servers + factoriosurvivalworld.com homepage stopped on 5.9.7.138; homepage to be rebuilt around this game (Orion)
metadata:
  node_type: memory
  type: project
  originSessionId: 219d410f-181b-4c0c-86bd-546afe204943
  modified: 2026-11-02T22:13:53.283Z
---

On 2026-11-02 the user had the Factorio side of `~/Projects/mod/mods/survival-world` brought down on 5.9.7.138: `docker compose stop updater factorio fortress deathworld web` in `/srv/survival-world` (games exited 0 = maps saved; containers kept, not removed). Still running there: `proxy` (Caddy — it terminates orion.factoriosurvivalworld.com AND supplies the Let's Encrypt cert our WebTransport ingress reads, so never stop it), pocketbase, picoshare, monitoring stack. factoriosurvivalworld.com now answers 502.

**Why:** the user wants a new homepage based on this game (Orion / the DF-style voxel game) in place of the Factorio site.

**How to apply:** a `deploy/sync.sh` or any `compose up -d` in the survival-world repo would restart the stopped services (the updater does `up -d` every 120 s if restarted). The new homepage needs a Caddy site change in `deploy/proxy/Caddyfile` there (the root domain currently routes to `web:3000`). Related: [[local-deployment-env]].
