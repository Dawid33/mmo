---
name: blockbench-model-loading
description: Native .bbmodel loading + animation playback BUILT 2026-10-20 (crates/bbmodel + client renderer/model.rs, just model-view); user plans an external service that generates bbmodels directly
metadata:
  type: project
---

BUILT 2026-10-20 (develop f5599e3, 088e568): `crates/bbmodel` (engine-free parser + mesher:
4.x/5.0 groups, cubes incl. box UV/inflate/stretch/ZYX rotation, mesh elements, alpha classes)
+ client `renderer/model.rs` (Bevy AssetLoader for `.bbmodel`, `BbModel(handle)` component,
`spawn_bbmodels`, hot reload) + `just model-view <path> [--shot png]`. Verified: 10 parser tests
on MIT hephaestus-engine fixtures, 2 headless client tests, dragon fixture rendered on Intel GPU.
ANIMATION PLAYBACK BUILT 2026-10-20 (cc38183): bbmodel::animation + bbmodel::molang, client BbAnimationPlayer (play/crossfade/stop/finished), model_view --anim/--time. NOT built: locators, effect tracks, model→ChiselData.

Dawid is evaluating a service that generates .bbmodel files directly (2026-10-20); a research doc
`docs/superpowers/specs/2026-10-20-ai-blockbench-generation-research.md` appeared from elsewhere
(not mine, uncommitted). Kodari API key added to .env (KODARI_API_KEY).
**How to apply:** new models go in assets/models/*.bbmodel and load via the asset server; check
them with `just model-view`. Units are mechanized (Planetary-Annihilation-style bots); a Kodari Studio prompt for a PA bot (with idle/walk/fire/fall/land/standup) was written 2026-10-20 — the kod- API key only reaches text models, Studio generation is web-UI only.
