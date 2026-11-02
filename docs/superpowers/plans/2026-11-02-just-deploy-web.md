# `just deploy`: the web build on 5.9.7.138

Goal: one command builds the release wasm client and a matching release server, copies both to
`root@5.9.7.138`, and restarts the game, so anyone with the URL can play in a browser.

Status: IMPLEMENTED 2026-11-02 with D1 = `orion.factoriosurvivalworld.com` and D2 = (a), the Let's Encrypt cert. The deployed shape differs from the sketch below in one way: the bundle is served by an `mmo-web` Caddy file server on the survival-world network (the proxy reverse-proxies to it), so the proxy needs no new volume or restart — only a Caddyfile reload. See CLAUDE.md "Web deployment" for the as-built summary.

## What the survey found

**The box** (`survival-world`, Ubuntu 26.04, 12 cores, 62 GB RAM, 1.7 TB free, Docker 29.9, ufw inactive)
already runs the `survival-world` compose stack from `/srv/survival-world/docker-compose.yml`, fronted
by `caddy:2.10-alpine` (`survival-world-proxy-1`) on **80/tcp, 443/tcp AND 443/udp** (Caddy's HTTP/3),
config in `/srv/survival-world/proxy/Caddyfile`. Domains: `factoriosurvivalworld.com` + `grafana.` /
`prometheus.` / `share.` subdomains; DNS is Route 53, managed in that project's `infra/main.tf`.
Taken UDP ports: 443, 34197–34199. 6466/6467 are free.

**Blockers in this repo:**

1. **WebTransport needs a secure context.** `http://5.9.7.138/` is not one, so the page must be served
   over HTTPS from a real hostname → a subdomain (proposed `mmo.factoriosurvivalworld.com`) on the
   existing Caddy. That means three one-time edits in the *survival-world* project, not this repo:
   a Route 53 A record, a Caddyfile site block, a read-only volume on the `proxy` service.
2. **The server binds `127.0.0.1` only** — QUIC (`crates/server/src/lib.rs:221`) and the WebTransport
   ingress (`WEBTRANSPORT_PORT` 6467). Needs a bind-address knob.
3. **The browser dials `https://127.0.0.1:{port}/`** (`crates/client/src/netcode_web.rs:236`). Needs
   the page's own hostname.
4. **Certificates.** The ingress mints a self-signed cert each start (`Identity::self_signed(["localhost",
   "127.0.0.1"])`, ≤ 14 days valid, required for `serverCertificateHashes`) and writes its hash to a
   path baked in at compile time (`env!("CARGO_MANIFEST_DIR")/../../assets/…`), which does not exist on
   the box. See decision D2.
5. **`index.html` speaks wasm-server-runner's layout**: it imports `./api/wasm.js`, fetches
   `/api/wasm.wasm`, and polls `/api/version` (the runner's hot-reload). A static bundle must provide
   those three paths (we can: `api/version` = the git rev, so a redeploy even reloads open tabs).
6. **`just web-release` is broken**: it builds `--profile wasm-release`, which `Cargo.toml` no longer
   defines; `wasm-opt` (binaryen) is not installed locally (`wasm-bindgen` is).
7. **Asset weight**: `assets/` is 103 MB, 97 MB of it `models/creatures/*.bbmodel` (JSON, compresses
   well). Served statically, gzip/zstd by Caddy; rsync makes redeploys incremental.

Nothing else blocks: the server reads only compiled-in assets (`blocks.ron` falls back to
`include_str!`), forges `worlds/default.world` on first start if missing, autosaves every 60 s and saves
on SIGTERM. The wasm build always skips the menu and connects straight to the server.

## Decisions (recommendation first)

- **D1 — hostname**: `mmo.factoriosurvivalworld.com` on the existing Caddy (needs edits to the
  survival-world stack + `infra/main.tf`). Alternative: a separate domain — same work, more DNS.
  There is no domain-less option for browsers short of every player using `ssh -L` to localhost.
- **D2 — the WebTransport certificate**:
  - **(a, recommended) Use the Let's Encrypt cert Caddy already gets for the subdomain.** Mount Caddy's
    `/srv/survival-world/caddy/data` read-only into the game container; new server env
    `VOXEL_WT_CERT` / `VOXEL_WT_KEY` → `Identity::load_pemfiles`. The browser then connects normally
    (no hash pinning), which also works in browsers whose `serverCertificateHashes` support lags.
    Cost: Caddy renews every ~60 days and the server must pick the new files up — reload on mtime
    change (wtransport `Endpoint::reload_config`) or simply a weekly restart; a dependency on Caddy's
    storage layout (`certificates/acme-v02.api.letsencrypt.org-directory/<host>/<host>.{crt,key}`).
  - (b) Keep self-signed + hash: make the hash-file path an env var (`VOXEL_CERT_HASH_FILE`) pointing
    into the web root, add the box's hostname/IP to the SANs, and rotate before day 14 (in-process
    `reload_config` + rewrite the file, or a restart cron). No Caddy coupling, but a restart every
    < 14 days or rotation code, and hash-pinning only.
- **D3 — how the server runs**: build the server binary locally (`--release`, glibc 2.39 here; the box
  has 26.04's newer glibc), scp/rsync it, run it in a stock `debian:trixie-slim` container with the
  binary bind-mounted — no image registry, no ECR. Its own compose project in `/srv/mmo/` so it never
  touches the survival-world stack's lifecycle. (ECR only if we later want reproducible images.)

## The recipe

```
just deploy            # = deploy-build + deploy-push
just deploy-build      # local only: target/deploy/{web,bin}
just deploy-push       # rsync + restart; no build
just deploy-logs       # ssh … docker compose logs -f --tail 200
just deploy-host=root@… deploy   # override; default root@5.9.7.138
```

Variables: `deploy_host := "root@5.9.7.138"`, `deploy_dir := "/srv/mmo"`. Both builds use the play
dials (`VOXEL_REGION_BLOCKS={{play_blocks}}`, chunk 16) — server and client must match, there is no
handshake.

**deploy-build** (`scripts/deploy_build.sh`):
1. `cargo build --release --bin server` → `target/deploy/bin/server`.
2. `cargo build --release -p client --target wasm32-unknown-unknown` (fix `web-release` the same way:
   drop the dead `wasm-release` profile or define it — e.g. `inherits = "release"`, `opt-level = "s"`,
   `lto = "thin"`, `codegen-units = 1`).
3. `wasm-bindgen --target web --out-name wasm --out-dir target/deploy/web/api …/client.wasm`, then
   `wasm-opt -O` if installed (warn and skip otherwise; add to the recipe's "Needs:" line).
4. Copy `crates/client/index.html` and `assets/` (minus `webtransport-cert-hash.json`) into
   `target/deploy/web/`; write `api/version` = `git rev-parse --short HEAD` (+`-dirty`).
5. (D2b only) nothing to write — the server writes the hash file at start.

**deploy-push** (`scripts/deploy_push.sh`):
1. `rsync -az --delete target/deploy/web/ host:/srv/mmo/releases/<rev>/` (hard-link against the
   previous release with `--link-dest` so the 100 MB of assets is not re-sent), then atomically
   `ln -sfn releases/<rev> /srv/mmo/web.next && mv -T /srv/mmo/web.next /srv/mmo/web`; keep the last 3
   releases.
2. `rsync target/deploy/bin/server host:/srv/mmo/bin/server.new && mv` (the running container keeps
   its open inode).
3. First run only: upload `deploy/web/compose.yml` (checked in here) to `/srv/mmo/compose.yml`.
4. `ssh host 'docker compose -f /srv/mmo/compose.yml up -d --force-recreate'`, then wait for
   `WebTransport endpoint listening` in the logs (timeout 60 s; a fresh box forges the world first).

`deploy/web/compose.yml` (sketch):

```yaml
name: mmo
services:
  server:
    image: debian:trixie-slim
    command: /srv/mmo/bin/server
    working_dir: /srv/mmo
    restart: unless-stopped
    stop_grace_period: 90s          # orderly exit parks + saves every region
    ports: ["6466:6466/udp", "6467:6467/udp"]
    environment:
      VOXEL_BIND_ADDR: 0.0.0.0
      VOXEL_WORLD_FILE: /srv/mmo/worlds/default.world
      VOXEL_WT_CERT: /caddy/certificates/acme-v02.api.letsencrypt.org-directory/mmo.factoriosurvivalworld.com/mmo.factoriosurvivalworld.com.crt
      VOXEL_WT_KEY:  /caddy/certificates/acme-v02.api.letsencrypt.org-directory/mmo.factoriosurvivalworld.com/mmo.factoriosurvivalworld.com.key
      RUST_LOG: info
    volumes:
      - /srv/mmo/bin:/srv/mmo/bin:ro
      - /srv/mmo/worlds:/srv/mmo/worlds
      - /srv/survival-world/caddy/data/caddy:/caddy:ro
```

**Survival-world stack, one time** — repo `~/Projects/mod/mods/survival-world` (deployed by its
`deploy/sync.sh`, as `ubuntu@`; it has uncommitted edits to exactly these files as of 2026-11-02, so
make these by hand or after those land):

- `deploy/firewall.nft`: add `6466, 6467` to `udp dport { 443, 34197, 34198, 34199 }` — the host
  firewall (`table inet hostfw`, installed by `deploy/setup-host.sh`, live on the box) drops every
  other UDP port, ufw being inactive notwithstanding. Docker's published ports do NOT bypass it (its
  prerouting hook runs at priority −150, before Docker's DNAT).
- `deploy/proxy/Caddyfile`: the site block below (sync.sh already reloads Caddy).
- `deploy/docker-compose.yml` `proxy.volumes`: `- /srv/mmo:/srv/mmo:ro`.
- `infra/main.tf`: `aws_route53_record "mmo"` like `share` (`mmo.${var.mail_domain}`, A → `var.server_ip`).
- The game container mounts `/srv/survival-world/caddy/data/caddy` (the proxy's `./caddy/data:/data`)
  read-only for the cert (D2a).


```caddy
mmo.factoriosurvivalworld.com {
	encode zstd gzip
	root * /srv/mmo/web
	file_server
	header /api/* Cache-Control "no-cache"
}
```
(`/srv/mmo` is mounted whole so the `web` → `releases/<rev>` symlink resolves.) Caddy serves `.wasm`
as `application/wasm` already.

## Code changes in this repo

1. `server`: `VOXEL_BIND_ADDR` (default `127.0.0.1`, so dev and single-player are unchanged) for both
   the QUIC and WebTransport binds; log the resolved addresses.
2. `server/webtransport.rs`: (D2a) `VOXEL_WT_CERT`/`VOXEL_WT_KEY` → load PEMs, skip the hash file;
   reload when the files change (or document the weekly restart). (D2b) `VOXEL_CERT_HASH_FILE`
   + SANs + rotation instead.
3. `client/netcode_web.rs`: connect to `location.hostname`; with D2a, when the page is `https:` and not
   localhost, connect without hashes and skip the hash-file fetch (port from a `?port=` param or a
   default of 6467); keep the dev path (`127.0.0.1` + hash file) for `just web`. Unit-test the
   decision as a pure function, like `query_world`.
4. `justfile`: `deploy`, `deploy-build`, `deploy-push`, `deploy-logs`, fix `web-release`; CLAUDE.md:
   a short "Web deployment" paragraph next to the OpenTofu one.

## Verification

- `just smoke`; `just test-one client "--lib netcode_web::"` type-checks only on native — check the
  wasm build compiles via `deploy-build` itself.
- Locally before the first push: run `target/deploy/bin/server` with `VOXEL_BIND_ADDR=0.0.0.0` and
  serve `target/deploy/web` with any static server on localhost (a secure context) — the dev-hash path
  must still connect.
- After the push: open `https://mmo.factoriosurvivalworld.com/?world=small` in Chromium, check the
  server log for `client_connected … transport=webtransport`, walk around, then `just deploy` again
  and confirm the world (and fortress) survived the restart.

## Risks / notes

- Besides the host nftables table above, a provider firewall (Hetzner Robot) would not be visible from
  the box; Factorio's UDP ports work, so probably none — confirm with a first connect.
- No auth: anyone with the URL joins one shared world, and every player is effectively admin.
- Release wasm at the `full` world scale is untested in a browser; `?world=small` is the safe link to
  hand out until it is.
- Caddy cert renewal + D2a: a stale cert breaks browser connects silently until reload — the reload
  must be real, not "remember to restart".
