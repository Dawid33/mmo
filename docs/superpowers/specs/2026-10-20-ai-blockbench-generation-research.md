# AI-generated Blockbench models for the RTS — research (2026-10-20)

Status: **research only, nothing built or changed.** Prices, terms and API catalogues
were checked on 2026-10-20 and change often; re-check before purchase.

Related docs:

- `docs/superpowers/specs/2026-09-22-blockbench-model-loading-investigation.md` —
  authoritative `.bbmodel` format and loader recommendation.
- `docs/superpowers/plans/2026-09-22-blockbench-model-loading.md` — planned
  `crates/bbmodel` implementation.
- `docs/superpowers/specs/2026-10-20-rts-transition-plan.md` — RTS direction and
  current (stale) GLB pipeline note at lines 108–109.
- `tools/assets/generate.py` — current Leonardo → TRELLIS.2 → GLB script, whose
  docstring wrongly says the client loads the GLB as-is.

## TL;DR

- **Existing access cannot produce `.bbmodel`.** Leonardo's 3D tab (Rodin V2) and
  every model in the 3D AI Studio API produce textured triangle/quad meshes
  (GLB; Tripo P2 quad can be FBX). The 3D AI Studio Convert tool supports
  GLB/glTF/OBJ/STL/PLY/FBX/USDZ/3MF — **not `.bbmodel` or `.vox`**.
- **The client cannot load the GLBs either.** Bevy is built without
  `bevy_gltf`, `bevy_scene`, `bevy_animation` and `png`; `assets/models/` is
  empty and `crates/bbmodel` does not exist.
- **Four services generate native `.bbmodel` directly:** Codella, Brikk, Kodari
  and Spaleforce. Codella is the best first test (accepts a reference image,
  4.10 free-format cubes, €7.99 one-time private plan). Brikk is the best
  volume/low-poly candidate and advertises API access on the $49.99 Studio plan.
  Kodari is for rigged/animated heroes and is expensive per iteration.
  Spaleforce gates `.bbmodel` downloads behind a €50/mo Ultimate plan.
- **Use the free headless Blockbench MCP server as a vendor-independent
  validation/render gate.** It reads and writes `.bbmodel` without Blockbench,
  validates geometry and animations, and renders contact sheets.
- **Recommended immediate bake-off:** Codella Starter (€7.99, private) on
  `soldier` and `barracks`, using the existing Leonardo concept prompts; validate
  the results with headless MCP before buying Brikk or Kodari.
- **Keep Leonardo and 3D AI Studio for concepts and maquettes.** Do not ship
  their GLBs unless the project deliberately switches to a glTF mesh pipeline.

## 1. What compatibility means for this project

From the existing `.bbmodel` investigation:

- 1 Blockbench pixel = 1 world unit = 1 voxel; 16 px = 1 block = 1 m. The loader
  applies no scaling.
- Planned loader scope: format 3.x–5.x, cubes, groups, `box`/per-face UV,
  embedded PNG, numeric linear/step/Catmull-Rom/pre-post keyframes; **mesh
  elements are skipped**; Molang evaluates to 0 with a warning.
- Convention: +Y up, north = **−Z**, east = +X; Euler ZYX.
- Pre-5.0 animations need `position.x`, `rotation.x` and `rotation.y` negated.
  Codella's published example is `format_version: 4.10`.
- Practical cube budgets (sosadly MCP density gates): props 30–60, mobs
  100–180, heroes 180–300+.

An AI-generated `.bbmodel` therefore has to pass more than "it opens":

| Check | Why it matters |
| --- | --- |
| `meta.format_version` is 3.x–5.x | 4.x animation axes need flipping |
| Every element is a cube (`type` absent or `cube`), no `mesh` | loader v1 skips meshes |
| No Molang expressions | loader v1 evaluates them as 0 |
| Cube count inside the RTS budget | performance and readability |
| Front is −Z after pipeline normalization | units face the right way |
| Texture is embedded PNG and small (≤256/512) | nearest-filtered RTS look |
| Pivots sit on joints; groups are nested, not flattened | animation/procedural use |
| Team-color areas are separable | per-team tint without repainting |
| Off-grid coordinates are accepted as render-only | Brikk uses fractional units |
| Animations are numeric, not Molang-heavy | only relevant once loader v2 lands |

Repo reality check:

- `assets/models/` has no files.
- `crates/bbmodel` has not been started.
- `Cargo.toml` Bevy features omit `bevy_gltf`, `bevy_scene`, `bevy_animation`
  and `png`.
- The RTS transition plan still says the art pipeline is
  "Leonardo.ai concept picture → 3D AI Studio TRELLIS.2 image-to-3D → GLB".

## 2. What existing access actually produces

### Leonardo

- Leonardo's 3D generation tab is Rodin V2-powered and exports `.glb`.
- Quality: quad meshes 4k–50k; triangle meshes 2k–500k. It is a proper 3D
  mesh pipeline, not a cube/Blockbench format.
- The current `tools/assets/generate.py` only uses Leonardo for a 2D concept
  picture; the 3D step is 3D AI Studio TRELLIS.2.
- **Best use here:** concept art, orthographic/turnaround references (including
  the 3D Reference View Creator blueprint), texture/color studies, prompt
  iteration. Not a production `.bbmodel` source.

### 3D AI Studio API

The public API docs list:

| Provider | API output | Notes |
| --- | --- | --- |
| Hunyuan 3D Pro / Rapid | GLB; converter can make OBJ/STL/FBX | Pro base 60 credits (+20 PBR, +20 multiview); Rapid base 35 (+20 PBR) |
| Hitem3D 3.0 | GLB, up to 5M faces | 2048³ reconstruction; high-detail, wrong style |
| TRELLIS.2 | GLB with optional PBR | 15–55 credits depending resolution/texture/thumbnail |
| Tripo 3.0 / 3.1 / 3.2 | GLB; quad and smart low-poly options | face limits and surcharges per option |
| Tripo P1 | GLB | base 60 (text) / 80 (image), plus texture options |
| Tripo P2 | GLB, or **FBX** when `quad=true` | base 200 + texture (220 with standard texture) |

Hard facts:

- **All 3D generation endpoints output GLB.** The docs say so explicitly:
  "All 3D generation endpoints output GLB files."
- Tripo P2 quad output is FBX because glTF cannot represent quads.
- The Convert tool's target formats are
  `glb`, `obj`, `obj_zip`, `stl`, `ply`, `fbx`, `fbx_zip`, `usdz`, `3mf`.
  There is no `.bbmodel` or `.vox` target.
- TRELLIS.2's "resolution" (512/1024/1536) is an internal **voxel grid
  resolution**. The output is a remeshed, smooth GLB with UVs/materials, not a
  cube model. Internal voxels do not survive as cubes.
- The API results expire after 24 hours; download immediately.
- The API docs do not list Meshy or Rodin endpoints even though the 3D AI
  Studio web app advertises "Includes Meshy, Tripo, Rodin & more". Treat
  web-app-only models as unavailable through the API until documented.
- The FAQ says generated assets can be used commercially and the user retains
  rights; API assets are private.

**Best use here:** 3D maquettes for proportion/silhouette checks, texture and
color reference, and rendered turnaround views. Not a production model source.

### Meshy's own documentation is the clearest warning

Meshy markets a "Minecraft model maker", but its own page says:

> "Java block and item models are built from cuboid elements only, and Bedrock
> geometry is cube-based in practice too, so a 3D mesh cannot be saved directly
> as a Minecraft model file."

It recommends exporting OBJ/GLB and running it through a voxelizer such as
ObjToSchematic for **worlds**, not for entity/block models. This applies equally
to TRELLIS.2, Hunyuan, Hitem3D, Rodin and the rest: they are the wrong
representation for this loader.

### Could a GLB be converted to `.bbmodel`?

No off-the-shelf service found. Two routes exist, both with real costs:

1. **Import the mesh into Blockbench** as a `mesh` element, then retopologise by
   hand into cubes. The planned loader skips meshes; this is manual work.
2. **Voxelize to a grid and merge cubes.** This means custom code
   (voxelize → greedy box merge → atlas resampling → `.bbmodel` writer), loses
   thin geometry, creates stair-stepped surfaces, and can explode the cube
   count. It is only plausible for chunky props, not heroes.
3. **VoxAI** is the closest off-the-shelf voxel-first route, but it is not
   native `.bbmodel` and its voxel output needs re-voxelization/merging.

Do not build the pipeline around GLB→`.bbmodel` unless the art direction
changes to smooth low-poly. If it does, enabling `bevy_gltf`/`bevy_scene` is
less work than writing a voxelizer.

## 3. Direct `.bbmodel` generators

| Tool | Output | Rig/animation | Price (checked 2026-10-20) | Licensing | RTS fit |
| --- | --- | --- | --- | --- | --- |
| **Codella** | `.bbmodel` 4.10 free format; cubes/groups/pivots/per-face UV; embedded atlas | Keyframes, linear/Catmull-Rom/step | Free 5 daily credits/public projects; Starter €7.99 one-time/100 credits/private; Pro €19.99/250; Team €39.99/500 | You own generated content, commercial use allowed; may use interactions to train models; free projects are public | **Best first test**; accepts a reference image; no Bedrock export |
| **Brikk** | `.bbmodel`, Java, Bedrock, GLB; embedded atlas; hitbox | Keyframe animations | $19.99/mo Pro (150 tokens ≈ 15 models); $49.99/mo Studio (450 tokens ≈ 45 models) + advertised API; generation 10 tokens, retexture 3, animation 5; free monthly tokens paused, paid/top-up tokens work ($1 = 10 tokens) | You retain ownership and may use commercially, but grant Brikk a broad perpetual showcase/distribution licence | **Best low-poly volume candidate**; gallery models observed 6–91 cubes; front is +Z, needs 180° normalization; API advertised but no public docs |
| **Kodari** | Rigged/animated `.bbmodel`; cubes and planes | Bone hierarchy + keyframed clips; 10–15 min typical generation; paid plan required | $9.99/600 credits … $99.99/6,000; public examples used 251 and 1,445 credits (~$4–$24) | Generated code/creations belong to you; public sessions are showcased; private sessions paid | **Only direct animated-hero option**, but expensive and loader animation support is not ready |
| **Spaleforce** | `.bbmodel` + pixel texture; V2 all plans, V3 Ultimate | Animated characters Ultimate-only | Free 5/day but no `.bbmodel` download; Pro €20/mo does **not** include `.bbmodel` download; Ultimate €50/mo does | Free is personal-use with credit; Pro/Ultimate full commercial rights | **Skip**: the file we need is paywalled at €50/mo; simple props focus |

### Codella — best first test

- Official page: text or attached image → `.bbmodel`; groups with joint origins,
  mirrored limbs, one texture atlas, keyframe animations, export packs for
  ItemsAdder/Oraxen/ModelEngine.
- Published example: `meta.format_version: "4.10"`, `model_format: "free"`,
  12 cubes for a sword. This is squarely in loader scope, including the pre-5.0
  animation-axis rule.
- Reference-image input is the differentiator: "Attach a picture and the
  generator reads its proportions, symmetry, parts and materials before
  building." That makes Leonardo concept art directly useful.
- Caveats: no public API; free projects are public; Bedrock export is not
  offered (open in Blockbench to convert); vendor-curated quality evidence;
  generated models still need validation for mesh elements, cube count,
  front orientation and team-color separation.

### Brikk — best volume/API candidate

- Exports `.bbmodel` with geometry, embedded texture, hitbox and animations.
  Also exports Java, Bedrock and GLB.
- Public gallery observed on 2026-10-20: 22 cards with cube counts from 6 to 91
  (for example `dire_wolf`, 39 cubes including its hitbox). The gallery is
  curated, but it is real, inspectable evidence that Brikk targets low cube
  counts.
- The gallery data shows fractional cube coordinates (e.g. 2.42, 13.58) and
  per-face paint palettes. Fractional coordinates are fine for rendering; they
  are not usable as `ChiselData` without cleanup.
- Brikk's own ModelEngine guide warns: "Brikk builds every model with the front
  toward **+Z**." Blockbench/loader convention is north **−Z**, so a Brikk model
  needs a 180° root rotation or pipeline normalization.
- Pricing: Pro $19.99/mo for up to ~15 models; Studio $49.99/mo for ~45 models
  and "API access — generate models from scripts & agents". No public API docs
  or endpoint reference found; validate before automating.
- Licence caveat: you own the models and may use them commercially, but you
  grant Brikk a worldwide, perpetual, royalty-free licence to modify, display,
  publish and otherwise use your models for any purpose, including marketing
  and third-party showcases. That may be unacceptable for proprietary game art.
- Availability caveat: free monthly tokens are paused "while we work on
  capacity"; the public site runs on a `lootvote.com` subdomain. Keep exported
  `.bbmodel` sources so a vendor failure cannot strand the assets.

### Kodari — animated heroes later

- The only vendor explicitly advertising rigged, bone-driven, keyframed
  `.bbmodel` files. Typical generation 10–15 minutes, iterative prompts refine
  the same model.
- Public project pages show 251 and 1,445 credits used for two models. At the
  $99.99/6,000-credit rate that is roughly $4.18 and $24.06. Iteration is the
  expensive part.
- No public API found; Studio requires a paid plan.
- Terms: generated code/creations belong to you; public sessions grant showcase
  rights; private sessions are a paid feature.
- **Do not buy for the MVP unless the animation loader exists.** The planned
  loader v1 has no animation support, so paying for animated heroes now buys
  files the client cannot fully use.

### Spaleforce — skip for this project

- Free accounts can generate with the V3 3D generator but cannot download
  `.bbmodel`; Pro €20/mo also lacks `.bbmodel` download; only Ultimate €50/mo
  includes "Free .bbmodel download — all your 3D models downloadable without
  restriction", plus animated custom characters.
- Terms: free plan is personal use with a "Powered by Spaleforce" credit and no
  resale; Pro/Ultimate get commercial use.
- Good for simple props/items; not cost-effective when the only required output
  is the paywalled file.

## 4. Voxel-first and MCP alternatives

### VoxAI

- Text/image → voxel models; exports `.vox`, `.json`, `.obj`, `.glb`; paid plans
  start at $14/mo (800 credits). Paid plans say assets are private and owned by
  the user. No public API found.
- Its Blockbench guide says to download `.json` and import it into Blockbench;
  another guide says to change the cube size and re-voxelize for Minecraft.
  This is a voxel-first pipeline, not a native `.bbmodel` pipeline, and the
  per-voxel cube counts need care.
- **Fit:** possible fallback for chunky props or terrain decorations, not the
  primary unit/building source.

### jasonjgardner's Blockbench MCP (GPL-3.0, 543 stars)

- Two modes: a desktop Blockbench plugin, and a **headless stdio MCP server**
  that reads/writes `.bbmodel` files directly without Blockbench.
- Headless tools include `bbmodel_info`, `bbmodel_outline`,
  `bbmodel_find_elements`, `bbmodel_get_node`, texture/animation listing,
  `bbmodel_create`, `bbmodel_edit`, `bbmodel_add_texture`, `bbmodel_validate`,
  `bbmodel_validate_animations`, `bbmodel_convert_legacy`, Java/Bedrock/modded
  exports, `bbmodel_render` and `bbmodel_contact_sheet`.
- Validation covers broken outliner, slivers, block-size limits, floating parts,
  intersecting parts and broken left/right symmetry; animation validation
  samples clips for sinking/detaching parts. Rendering needs Node 23.6+ and a
  GPU; validation works without them.
- Runs from GitHub with `bunx`/`npx`; each agent gets its own process, so
  several models can be checked in parallel. The README documents an OpenCode
  MCP configuration.
- Plugin policy note: the official Blockbench plugin repository does not allow
  AI features, so MCP plugins must be side-loaded. The desktop server has no
  authentication and exposes `risky_eval`; only enable it on a trusted network.
- **Fit:** recommended vendor-independent gate for any generated `.bbmodel`,
  and a possible long-term authoring route for exact, bespoke models.

Example OpenCode MCP entry (paths are examples):

```json
{
  "mcpServers": {
    "blockbench-headless": {
      "command": "npx",
      "args": [
        "-y",
        "github:jasonjgardner/blockbench-mcp-plugin",
        "--root",
        "/home/dawids/Projects/mmo/assets/models"
      ]
    }
  }
}
```

### sosadly's Blockbench MCP (MIT, 70 tools)

- Connects an agent to the live Blockbench desktop editor; desktop-only.
- Tools cover model creation, texturing, rigging, animation, screenshots and
  quality gates. `audit_complexity` grades against prop 30–60 / mob 100–180 /
  hero 180–300+ cube budgets and flags monolithic boxes, overlap, micro-detail
  density, bare faces and bone depth. `screenshot_views` lets the agent see its
  own work.
- **Fit:** excellent for hand-authored hero/prop models with human review; not
  headless, so it cannot gate CI by itself.

### Ashfox (MIT)

- "Assets as code": a DSL compiled into voxel-game assets, exporting GLB and
  Minecraft packs. It is deterministic and reviewable, but its primary outputs
  are not `.bbmodel`.
- **Fit:** interesting if the project wants source-controlled procedural
  models later; it does not solve the current loader path.

## 5. Recommended pipeline

### Now — MVP assets with a human in the loop

1. **Concept (Leonardo).** Keep the existing `tools/assets/generate.py`
   picture-only mode. Generate a clean 3/4 view on a flat key colour, plus
   front/side views with the 3D Reference View Creator blueprint. Ask for a
   chunky, cube-built silhouette, neutral shading, and flat light-grey
   team-colour areas.
2. **Optional maquette (3D AI Studio).** Use TRELLIS.2 for a textured GLB only
   as a proportion/silhouette reference, and the 3D AI Studio render tool for
   turnaround images. Do not put the GLB in `assets/models/`.
3. **Model (Codella first).** Feed the best concept image to Codella with a
   prompt that specifies cubes only, one atlas, static (no animation), front
   facing, and a cube budget. Use Codella Starter (€7.99 one-time) so the
   project is private. Test `soldier` and `barracks` first because they stress
   character and building construction differently.
4. **Validate (headless Blockbench MCP).** Run `bbmodel_info`, `bbmodel_validate`,
   texture listing and front/3⁄4/top renders. Reject any file with mesh elements,
   Molang, out-of-budget cubes, broken outliner, or the wrong facing. Fix
   orientation and team-colour mask in Blockbench/Aseprite if needed.
5. **Commit the `.bbmodel` source.** Treat it as the editable asset of record.
   Keep raw prompts and concept images in the git-ignored raw area.
6. **Iterate in Blockbench, not only by prompt.** AI gets the first 90%; pivots,
   silhouettes, team colour and readability still need a human pass.

### Next — if Codella is not enough

- **Brikk `$1` single-model top-up** is a cheap side-by-side test; Pro
  ($19.99/mo) if it wins on low-poly quality. Normalize the +Z front. Confirm
  the showcase licence is acceptable before using it for game art.
- **Brikk Studio ($49.99/mo)** only if the API is documented and works; no
  public API reference exists yet.
- **MCP-authored models** for exact one-off assets (core keep, team-colour
  masks, custom heroes) once an agent can reliably pass `bbmodel_validate`.

### Later — animated heroes

- Only after loader v2 supports numeric animation tracks, pre-5.0 axis flips and
  bone pivots. Then evaluate Kodari against MCP-authored animation. Until then,
  use static models with procedural render-only motion (bob, lean, leg swing)
  as the slime investigation already proposed.

### What not to do

- Do not feed GLB meshes into the native loader.
- Do not write a general GLB→voxel→`.bbmodel` converter for hero assets; the
  quality/effort trade-off is bad.
- Do not pay for Kodari animations before the loader can play them.
- Do not rely on Spaleforce's free tier for a downloadable `.bbmodel`.
- Do not treat `3D AI Studio`'s "voxel grid" as voxel output; it is internal.

## 6. Bake-off plan (concrete)

Use the existing asset names and Leonardo prompts, adding `.bbmodel`
constraints.

| Asset | Prompt constraints to add | Pass bar |
| --- | --- | --- |
| `soldier` | cube-built, 100–180 cubes, 2-block tall, front-readable, plain light-grey shield/plume for team colour, static, one 16×16-style atlas | opens in Blockbench 5.2.1; cubes only; no Molang; bounds sensible; front −Z; reads at RTS camera distance |
| `barracks` | cube-built, 60–90 cubes, ~4-block footprint, roofs/walls readable top-down, neutral wood/stone, static | same, plus no interior geometry, no floating parts, no slivers |

Record for each generated file:

- tool, prompt, cost, generation time;
- `meta.format_version`, `model_format`;
- element types and cube count;
- texture count/size, embedded or external;
- bounds, front direction, pivot hierarchy;
- headless MCP validation result and render paths;
- human score: silhouette, top-down readability, team-colour separability,
  amount of manual cleanup.

Only after both assets pass should Brikk or Kodari be purchased.

## 7. Cost and licensing summary

| Vendor | Entry cost | Commercial use | Key restriction |
| --- | --- | --- | --- |
| Leonardo | existing access | check plan terms | 3D output is GLB mesh, not `.bbmodel` |
| 3D AI Studio API | existing access; credits per generation | FAQ says commercial use, user retains rights | outputs GLB; no `.bbmodel`/`.vox`; results expire in 24h |
| Codella | free 5/day public; €7.99 Starter private | yes, "you own everything" | no API; AI-training clause; free projects public |
| Brikk | $1 top-up for one test; $19.99/mo Pro | yes, but broad perpetual showcase licence | +Z front; fractional coordinates; early-stage; API undocumented |
| Kodari | $9.99/600 credits; public examples 251–1,445 credits | yes, creations belong to user | no API; expensive iteration; animation loader not ready |
| Spaleforce | free generation, `.bbmodel` download only €50/mo Ultimate | free personal use; Pro/Ultimate commercial | download paywall; no API |
| VoxAI | $14/mo | paid plans: private, you own them | voxel output, not native `.bbmodel`; no API found |
| Headless Blockbench MCP | free (GPL-3.0) | tool use only | no auth/`risky_eval` on desktop mode; render needs GPU |

## 8. Risks and open questions

- **Vendor evidence is mostly curated galleries.** Brikk's public gallery is
  the strongest artifact because cube counts are visible; Codella and Kodari
  quality claims still need direct testing.
- **AI outputs will fail format/quality gates sometimes.** The validation
  pipeline is not optional.
- **Team-colour masks are not a first-class feature of any vendor.** Budget a
  post-process step (reserve an atlas colour key, or split a material/group).
- **Direct generators are early-stage and can change or disappear.** Commit
  `.bbmodel` sources and keep exports opening in stock Blockbench.
- **The current script and plan are misleading.** `generate.py` writes GLBs the
  client cannot load and its docstring claims otherwise; the RTS plan still
  describes the GLB pipeline. Both should be corrected or explicitly marked
  experimental.
- **If direct `.bbmodel` tools disappoint, the fallback decision is
  architectural:** either switch the RTS to a glTF mesh pipeline (enable
  `bevy_gltf`/`bevy_scene`/`bevy_animation`/`png`, lose the 1 px = 1 voxel
  mapping and Blockbench editability), or continue with native `.bbmodel` and
  use MCP/human authoring. Mixing both before the loader exists will slow the
  MVP.

## Sources

- Leonardo, "How to Turn an Image Into a 3D Model with AI":
  <https://leonardo.ai/news/how-to-turn-an-image-into-3d-model>
- 3D AI Studio API docs, 3D generation overview:
  <https://www.3daistudio.com/Platform/API/Documentation/3d-generation>
- 3D AI Studio API docs, TRELLIS.2:
  <https://www.3daistudio.com/Platform/API/Documentation/3d-generation/trellis2>
- 3D AI Studio API docs, Tripo P2:
  <https://www.3daistudio.com/Platform/API/Documentation/3d-generation/tripo-p2>
- 3D AI Studio API docs, Convert:
  <https://www.3daistudio.com/Platform/API/Documentation/tools/convert>
- 3D AI Studio API FAQ:
  <https://www.3daistudio.com/Platform/API/Documentation/faq>
- Meshy, Minecraft model maker (the "cannot be saved as a Minecraft model
  file" statement):
  <https://www.meshy.ai/use-cases/free-game-assets/minecraft>
- Codella, Blockbench model generator:
  <https://codella.ai/blockbench-model-generator>
- Codella, pricing: <https://codella.ai/pricing>
- Codella, terms: <https://codella.ai/terms>
- Brikk, home/pricing: <https://brikk.lootvote.com/>
- Brikk, ModelEngine guide (+Z front warning):
  <https://brikk.lootvote.com/guides/modelengine>
- Brikk, gallery (cube counts): <https://brikk.lootvote.com/gallery>
- Brikk, terms: <https://brikk.lootvote.com/terms>
- Kodari, Studio: <https://kodari.ai/studio>
- Kodari, credits: <https://kodari.ai/credits>
- Kodari, AI Blockbench model generator blog:
  <https://kodari.ai/blogs/ai-blockbench-model-generator>
- Kodari, terms: <https://kodari.ai/terms>
- Spaleforce, Blockbench generator:
  <https://spaleforce.com/blockbench-model-generator>
- Spaleforce, shop: <https://spaleforce.com/shop>
- Spaleforce, terms: <https://spaleforce.com/terms>
- VoxAI, pricing: <https://www.voxelai.ai/pricing>
- VoxAI, Blockbench guide:
  <https://www.voxelai.ai/docs/integrations/blockbench>
- jasonjgardner/blockbench-mcp-plugin (headless README):
  <https://github.com/jasonjgardner/blockbench-mcp-plugin>
- sosadly/blockbench-mcp:
  <https://github.com/sosadly/blockbench-mcp>
- Blockbench `.bbmodel` format wiki:
  <https://blockbench.net/wiki/docs/bbmodel>
- Repo docs:
  `docs/superpowers/specs/2026-09-22-blockbench-model-loading-investigation.md`,
  `docs/superpowers/plans/2026-09-22-blockbench-model-loading.md`,
  `docs/superpowers/specs/2026-10-20-rts-transition-plan.md`,
  `tools/assets/generate.py`, `Cargo.toml`.
