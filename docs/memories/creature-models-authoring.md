---
name: creature-models-authoring
description: "Creature Blockbench models are authored by Claude (no external 3D/model service); assault_bot.bbmodel's service is unusable and not a precedent"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 10c398c9-5d68-4696-ab08-b4ad5335c308
  modified: 2026-10-29T13:18:51.471Z
---

2026-10-29 Dawid: goal = a Blockbench model + animations for every DF creature (roster in
docs/superpowers/specs/2026-10-29-df-creatures/), one by one. assault_bot.bbmodel was made with a
service that needed his manual intervention — that service can't be used, and assault_bot is not a
template or convention source. The TRELLIS GLB pipeline (`just assets`) gives static meshes, no rig.

**Why:** the pipeline has to run without a service and without his hands; he reviews at a gate.

**How to apply:** Claude writes the models (proposed: compact creature source → generator →
.bbmodel, template bones keep their group UUIDs because animators bind by uuid, procedural
palette textures); Leonardo only for reference pictures; Blockbench touch-ups optional, never
required. BUILT 2026-10-29 (uncommitted): spec docs/superpowers/specs/2026-10-29-creature-asset-pipeline.md, tools/creatures/ (build/brief/sheet/progress), quadruped template + dog pilot (draft), lint crates/bbmodel/tests/creatures.rs, `just asset-viewer`. /goal handoff: docs/superpowers/plans/2026-10-29-creature-models-handoff.md (6 templates + vermin + giant variants still to build; approval stays Dawid's).
Related: [[blockbench-model-loading]].
