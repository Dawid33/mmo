---
name: bevy-no-png-feature
description: "Bevy is built without the `png` feature — asset_server.load of a .png never arrives and a StandardMaterial waiting on it is silently never drawn"
metadata:
  node_type: memory
  type: project
  originSessionId: 8a2e9b19-e138-4cb9-82fc-50c24ae70b29
  modified: 2026-11-06T09:42:41.995Z
---

The root Cargo.toml's Bevy feature list has no `png` (it has ktx2/zstd only). `asset_server.load("blocks/x.png")` therefore never yields an Image, and a `StandardMaterial` whose `base_color_texture` is that handle is never drawn. There is no error; the mesh is just invisible.

**Why:** found 2026-11-06 when item piles spawned at the right place but rendered nothing (fixed in 5a97691). Block textures and bbmodel textures are decoded in Rust, so nothing else had exercised the asset-server PNG path.

**How to apply:** get textures through `renderer::load_block_texture` (the file natively, the embedded copy on wasm) and add them with `Assets<Image>::add`, as `item_piles::texture_for` and `item_icons` do. Don't add a new asset-server PNG load. If piles or other meshes "don't render", check this first.
