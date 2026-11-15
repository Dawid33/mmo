---
name: game-name-orion-web-deploy
description: "The game is called Orion (renamed from \"Labour of Love\" 2026-11-02); browser build deployed by `just deploy` to orion.factoriosurvivalworld.com on the survival-world Hetzner box"
metadata:
  node_type: memory
  type: project
  originSessionId: e4ef1596-141b-4cd8-9155-8e9994225101
  modified: 2026-11-02T17:19:54.352Z
---

The game's name is **Orion** (renamed from "Labour of Love" on 2026-11-02, everywhere incl. window/page titles and the menu).

The public browser build is deployed with `just deploy` to https://orion.factoriosurvivalworld.com/ (root@5.9.7.138, compose project `mmo` in /srv/mmo; see CLAUDE.md "Web deployment"). It piggybacks on the user's other project, survival-world (`~/Projects/mod/mods/survival-world`): its Caddy proxy (site block + Let's Encrypt cert the game server reads), its host nftables firewall (`deploy/firewall.nft`, needs UDP 6466/6467), and its OpenTofu `infra/` (Route 53 `aws_route53_record.orion`; state moved to s3://survival-world-tfstate-029905505680 on 2026-11-02 after the local state was found lost — resources re-imported).

**Why:** the deploy spans two repos; changes to proxy/firewall/DNS belong in survival-world, not here.
**How to apply:** survival-world's `sync.sh` no longer deploys its website unless `--website`; its tree often has the user's uncommitted work — apply live changes surgically rather than running its whole sync. The auto-mode classifier blocks firewall/DNS changes made by hand; tofu apply on user request went through. Distro binaryen (108) breaks wasm-bindgen output — web_release.sh pins binaryen under target/tools.
