# Brick world — prior art survey (2026-11-08)

Question: could the game be "made of bricks" (studded toy bricks) instead of plain cubes? What open
source games / engines / tools exist, and what do they teach about grid units, data representation,
rendering at scale and voxel→brick conversion. Survey only — no design decision yet.

## 1. The projects

### Open-source games / engines (bricks are the world)

| Project | What | Tech / license | Worth stealing |
|---|---|---|---|
| **Brickbuild** — [repo](https://github.com/rollerozxa/brickbuild), [ContentDB](https://content.luanti.org/packages/ROllerozxa/brickbuild) | Studded brick building game on Luanti (Minetest). 44 part colours, many shapes. Spiritual successor to OpenBlox. | Lua on Luanti; MIT code, CC-BY-SA-4.0 media | Closest thing to "a voxel engine re-skinned as bricks": bricks are grid nodes with stud meshes, and the player is ~2× normal size so nodes read as small bricks. That's cheap scale trickery we could copy. |
| **Studs** mod (erlehmann) — [ContentDB](https://content.luanti.org/packages/erlehmann/studs/) | Puts studs on the tops of existing full nodes / slabs / stairs. | Luanti mod (license unchecked) | Shows the *minimum* change: keep the voxel world and only add studs to exposed tops. |
| **OpenBlox** — [SourceForge](https://www.sourceforge.net/p/openblox/) | Engine for "Lego-like" building games, physics + Lua scripting. | Old/abandoned | Historical only. |
| **Land of Dran** — [client](https://github.com/SorpoBlock/Land-of-Dran-Client), [server](https://github.com/SorpoBlock/Land-of-Dran-Server), [thread](https://cone3d.gamedev.net/forums/topic/715337-land-of-dran-open-source-block-building-game) | Open Blockland-like game, engine-less C++, client/server, tools + combat. | C++ (license unchecked) | The open reference for a Blockland-style *networked* brick sandbox: brick placement over the network, free (non-grid) brick bodies. |
| **Brickcraft** (tetreum) — [index](https://awesome.ecosyste.ms/projects/github.com%2Ftetreum%2Fbrickcraft) | "Lego bricks + Minecraft building" in Unity, WIP. | Unity, AGPL-3.0 | Small; mainly a sanity check that the hybrid has been tried. |
| **lego-blocks-game** — [index](https://awesome.ecosyste.ms/projects/github.com%2Fnafisrayan%2Flego-blocks-game); **Tower of Bricks** — [itch](https://shrikegames.itch.io/tower-of-bricks-built-to-scale-gmtk2024) | Toy web / jam games. | R3F / Godot | Not much. |
| **Darkflame Universe** — [org](https://www.github.com/DarkflameUniverse) | Server emulator for the brick MMO *LEGO Universe*. | C++, AGPL-3.0; client assets are proprietary | Server architecture of an actual brick MMO (zones/world servers). Its brick building was LDD-style free placement of models, not a brick terrain. |

### Commercial but documented (most relevant engineering)

- **Brickadia** (Unreal, proprietary, but a public save format plus open tooling). The best technical writing on a
  multiplayer world made entirely of bricks:
  - [Optimizing Bricks for Alpha 5, Part 1](https://brickadia.com/blog/optimizing-bricks-for-alpha-5-part-1/):
    the brick grid is split into cube chunks, and each chunk gets its own PhysX actor with its own acceleration
    structure. A single global BVH caused a full rebuild on every placement, so they **halved the chunk edge**,
    which gave ~8× cheaper inserts and deletes. Face-coverage between neighbours was cached as link lists
    (8 M links / ~1 GB); they dropped the cache and re-search neighbours on edit. Replication reaches 100 k bricks/s
    with delta compression (a shared brick type is sent once, then skip bits).
  - [Part 2](https://brickadia.com/blog/optimizing-bricks-for-alpha-5-part-2/): "brick clusters", one merged
    mesh per cluster that splits adaptively in dense areas, rebuilt async on workers. Texture arrays bring it to one
    draw per cluster (previously ~4×). Off-the-shelf mesh LOD failed (vertex clustering looked bad; quadric
    simplification removed little). Their own LOD swaps in simplified brick meshes at distance and then runs a custom
    redundant-geometry removal pass: **−82 % vertices, ~2× FPS** on Brickadia City (54 M+ verts).
  - Open tooling: [brs](https://github.com/brickadia/brs) (official Rust save lib), `brickadia-rs`, `brs-js`,
    [Omegga](https://awesome.ecosyste.ms/projects/github.com%2Fbrickadia-community%2Fomegga) (TS server wrapper).
    [heightmap2brs](https://github.com/Meshiest/heightmap2brs) (Rust, MIT) turns a **heightmap into bricks** with
    `--greedy` merging, `--terrain` (flat areas → boxes, slopes → micro wedges), `--wedge` and `--rampify`. This is
    the closest existing "worldgen terrain as bricks" code.
- **Blockland** (Torque, proprietary) is the ancestor of the genre. **LEGO Worlds** is a procedural voxel world of
  bricks, but no technical write-ups were found.

### Brick CAD / data ecosystem (LDraw)

- **LDraw**: the open standard and parts library for virtual LEGO CAD. The parts library is a free-to-use part set
  under a CC license (Debian ships it in non-free). Units: **1 stud = 20 LDU, plate = 8 LDU, brick = 24 LDU**.
  Parts come in hi/lo-res stud primitives, and the [LDraw LOD discussion](https://forums.ldraw.org/thread-8470.html)
  is useful. A plain stud is 32 polys; in a 2×4 brick, **400 of 414 polys are studs**.
- Rust: [`weldr`](https://github.com/djeedai/weldr) (LDraw parser plus a glTF export CLI) and
  [`ldraw.rs`](https://github.com/segfault87/ldraw.rs) (MIT; parser, IR for modern pipelines, GL/WebGL renderer,
  wasm). Either can import real parts as assets (`.ldr` → glTF → our model path).
- Editors: [LeoCAD](https://github.com/leozide/leocad) (GPLv2, Qt/C++), LDView, LPub3D, Bricksmith (whose author
  wrote [instancing for Bricksmith](https://hacksoflife.blogspot.com/2013/01/instancing-for-bricksmith.html)).

### Voxel → brick conversion ("legolization")

- **Animal Logic, LEGOscape** (SIGGRAPH 2014, [pdf](https://animallogic.com/wp-content/uploads/2023/07/Assembling-Environments-with-LEGOscape.pdf)).
  This is the most directly applicable idea:
  - The integer coordinate unit is the **1×1 plate**. A plate is ~2× as wide as tall, so each plate-voxel is
    supersampled **4×4×2 = 32 sub-voxels = one `u32`**. Intersection becomes `a & b`, sub-voxel difference
    `popcount(a ^ b)`, and "breaches terrain" `a & !t`. 90/180/270° rotations are bit rotations, thanks to a
    two-level spiral bit order.
  - Filling uses *patterns* (one brick type plus shift/stagger/spacing). A better-fitting brick evicts the bricks it
    intersects. Rendering bakes bricks into an instanced particle system.
  - Related: Animal Logic's LEGO Movie talks
    ([Millions of Bricks](https://animallogic.com/wp-content/uploads/2023/06/Millions-of-Bricks-Rigging-Animation-Render.pdf),
    [Bricks, Bricks and More Bricks](https://animallogic.com/wp-content/uploads/2023/06/The-LEGO-Movie-Bricks-Bricks-and-More-Bricks.pdf)):
    instancing at every level (brick → model → city block) removed the need for LOD in their offline renderer.
- **Legolization** (Luo et al., SIGGRAPH Asia 2015) is the reference algorithm for stable brick layouts: a
  force-based stability analysis that iteratively re-lays weak regions. Lighter alternatives use a connectivity-only
  stability test (all bricks connected = OK; see the
  [Eurographics STAG papers](https://diglib.eg.org:443/bitstream/handle/10.2312/stag20221256/057-067.pdf)), random
  1×1 merging that maximises connections (Testuz et al.), or genetic / split-and-merge methods.
  [BrickAnything (2026)](https://arxiv.org/pdf/2605.26182) still uses Legolization as its baseline.
- Code: [`brickalize`](https://pypi.org/project/brickalize/) (Python, GPLv3; voxel → largest bricks, hollowing,
  supports) and [block2brick](https://sourceforge.net/projects/block2brick) (GPLv3; Minecraft blocks → bricks via a
  dictionary).

## 2. Lessons mapped onto this engine

**Units, the core decision.** Real proportions are stud pitch 8 mm, plate 3.2 mm, brick 9.6 mm: a plate is
5:2 (w:h) and a brick 5:6. Our block is 16³ voxels (`game::block::BLOCK_VOXELS`), which doesn't divide into 5:2.
Options, in order of disruption:
1. **Stylised, cubic-ish** (Brickbuild / Studs mod / Minecraft packs): keep the 16³ block, call one block
   2×2 studs × one brick-ish height, and draw studs on exposed tops. Plate = 8 voxels = our existing **half block**
   (`HALF_BLOCK_VOXELS`), so a plate is 1×1 stud wide by half a block tall (2:1 rather than real 5:2). That's the
   LEGOscape approximation, and it fits the current half-block / chisel system with **zero sim changes**.
2. **True-ratio plates on the voxel grid**: stud = 5 voxels, plate = 2 voxels. That needs a block edge that's a
   multiple of 5 (e.g. 20 voxels = 4 studs), which collides with 16-based packing (`ChiselData` 4-bit coords, the
   32³ render brick baked into shaders). Expensive and not obviously worth it.
3. **Free / non-grid bricks** (Blockland, Land of Dran, Brickadia micro-bricks): a separate entity-like brick
   layer. Brickadia's chunked per-cell physics actors are the lesson if we ever go there.

**Data.** `ChiselData` is already "a canonical list of axis-aligned cuboids on the 16³ grid via a fixed greedy merge",
which is a brick list. Under option 1 the main additions would be:
- a *brick decomposition* rule that's canonical and deterministic. The greedy merge would need to prefer real brick
  footprints (1×1, 1×2, 2×2, 2×4 …) and **stagger seams** between layers so it reads as built rather than extruded;
  the legolization papers' "maximise connections" heuristic fits. It's a pure function of the voxels, so hashes
  stay stable.
- LEGOscape's bitmask trick only if we need sub-plate shapes (slopes, wedges). heightmap2brs `--terrain`
  (flat → boxes, slopes → wedges) is the terrain analogue.

**Rendering, where the cost is.** Studs are ~97 % of a brick's polygons. Prior art agrees:
- merged per-cluster meshes (our chunk meshes already do this), with studs only on **exposed top faces**;
- studs as geometry near the camera, then normal-mapped or textured discs at distance (LDraw LOD thread, the old
  Roblox stud texture). Since our camera is a top-down commander view, the far ring can be texture-only;
- Brickadia: simplified brick meshes at distance plus a redundant-geometry pass beat generic simplifiers;
- a natural fit here: studs as an **instanced draw generated from exposed-top-face records** or procedurally in the
  voxel shader (parallax / analytic cylinder), keeping the voxel mesher unchanged. Needs a prototype.

**Seams / bevels.** The "made of bricks" look comes mostly from the dark seam lines between bricks and slightly
bevelled edges, more than from studs. Per-brick seams require the decomposition above, because the mesher must
know brick boundaries rather than just voxel material.

**Gameplay hooks (DF direction).** Legolization's stability analysis lines up with DF-style cave-ins and support,
and the "connectivity = stable" test is cheap and deterministic. Creature models could be voxelised and
brick-decomposed from the existing `tools/creatures` sources.

**Legal.** Don't use "LEGO" in names or put a logo on studs. Stud-and-tube patents are long expired, and the EU
rejected a 3D trademark on the 2×4 brick shape as functional (Lego Juris v OHIM, 2010). Generic studded bricks are
fine; LDraw part *geometry* is CC-licensed and attributable if imported.

## 3. Suggested next steps (if pursued)
1. Visual spike: studs on exposed block tops plus seam lines in the voxel shader, half block = plate. Judge the
   look from the commander camera before touching sim data.
2. Prototype a canonical brick-decomposition merge for `ChiselData` / whole blocks (staggered, real footprints).
3. Read Brickbuild's node/stud meshes and heightmap2brs's greedy/terrain modes in detail.
