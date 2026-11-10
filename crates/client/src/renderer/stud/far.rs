//! Far terrain of a stud world: one mesh per LOD key from its
//! [`StudLodTile`] (`game::stud_lod`) — the recipe sampled on the key's
//! cell-corner lattice.
//!
//! The tile is drawn as a **smooth height field**: one vertex per lattice
//! point (its top, the LDraw colour of its material), two triangles per cell,
//! normals from the height differences. Tiles of one factor share their border
//! samples, so they meet exactly; where a tile meets a coarser one, or the near
//! bricks, the surfaces agree at the coarse lattice points and may part by a
//! cell's relief between them — a **skirt** hangs one cell edge down from each
//! border to close that. Not stepped prisms: at these cell sizes (2 m and up)
//! a prism per cell is three quads of unshared vertices where the smooth sheet
//! is one vertex, and it is the shared normals that carry the relief's shading.
//!
//! Water is the recipe's: a flat blended sheet over every cell with a wet
//! corner, at the highest of its corners' levels (the ground rises through it
//! at the shore), in the voxel water pass's material.
//!
//! Meshes are built in a compute job ([`StudLodMeshJob`]) and hang under the
//! key's root, which `lod_rings` owns (subscription, re-receipt, retirement and
//! the hand-over to the near window are the voxel rings').

use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, Task};
use game::lod::LodKey;
use game::movement::UNITS_PER_METER;
use game::{plate_units, StudLodSample, StudLodTile};
use serde::{Deserialize, Serialize};

use super::material::{StudMaterials, StudTerrainMaterial};
use super::mesh::{stud_assets, StudMeshData};
use crate::renderer::lod_rings::LodWaterSurface;
use crate::renderer::water::material::{WaterMaterial, WaterMaterialHandle};
use crate::renderer::water::mesh::WaterMeshData;
use crate::renderer::water::water_alpha;
use crate::session::SessionScoped;

/// Finished far tiles shown per frame at most (each is two mesh uploads).
pub const STUD_LOD_UPLOADS_PER_FRAME: usize = 4;

/// Mesh one far tile: `cell` is the cell edge in sim units (the key's factor).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StudLodMeshJob {
    pub tile: StudLodTile,
    pub cell: f32,
}

/// A far tile's vertex streams, in the key root's frame (sim units).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StudLodMeshOutput {
    pub terrain: StudMeshData,
    pub water: WaterMeshData,
}

impl crate::renderer::compute::ComputeJob for StudLodMeshJob {
    type Output = StudLodMeshOutput;
    const KIND: crate::renderer::compute::JobKind = crate::renderer::compute::JobKind::StudLodMesh;

    fn run(self) -> StudLodMeshOutput {
        // A tile that does not decode draws nothing (wire data is untrusted).
        match self.tile.decode() {
            Some(samples) => mesh_tile(self.tile.side, &samples, self.cell),
            None => StudLodMeshOutput::default(),
        }
    }
}

/// How far a tile's border skirt hangs down, sim units: one cell edge, so it
/// closes any gap to a neighbouring surface whose slope is up to 45 degrees.
pub fn skirt_depth(cell: f32) -> f32 {
    cell
}

/// The terrain sheet, its skirts and the water of a tile of `side` cells
/// (`samples`: `(side + 1)²`, row major). See the module doc.
pub fn mesh_tile(side: u16, samples: &[StudLodSample], cell: f32) -> StudLodMeshOutput {
    let n = usize::from(side) + 1;
    assert_eq!(samples.len(), n * n, "a tile's lattice");
    if side == 0 {
        return StudLodMeshOutput::default();
    }
    let assets = stud_assets();
    let at = |x: usize, z: usize| &samples[z * n + x];
    let height = |x: usize, z: usize| plate_units(i32::from(at(x, z).top));
    let pos = |x: usize, z: usize| [x as f32 * cell, height(x, z), z as f32 * cell];
    let color = |x: usize, z: usize| {
        let c = assets.brick_colors(game::stud_material_color(at(x, z).material)).0;
        // Far terrain is opaque whatever the colour.
        [c[0], c[1], c[2], 1.0]
    };

    let mut t = StudMeshData::default();
    for z in 0..n {
        for x in 0..n {
            // Central differences (one-sided on the border).
            let (x0, x1) = (x.saturating_sub(1), (x + 1).min(n - 1));
            let (z0, z1) = (z.saturating_sub(1), (z + 1).min(n - 1));
            let dx = (height(x1, z) - height(x0, z)) / ((x1 - x0) as f32 * cell);
            let dz = (height(x, z1) - height(x, z0)) / ((z1 - z0) as f32 * cell);
            t.positions.push(pos(x, z));
            t.normals.push(Vec3::new(-dx, 1.0, -dz).normalize().to_array());
            t.colors.push(color(x, z));
        }
    }
    for z in 0..n - 1 {
        for x in 0..n - 1 {
            let i = |x: usize, z: usize| (z * n + x) as u32;
            // Counter-clockwise seen from above.
            t.indices.extend([i(x, z), i(x, z + 1), i(x + 1, z + 1), i(x, z), i(x + 1, z + 1), i(x + 1, z)]);
        }
    }

    // Skirts: each border walked so that `step x down` points out of the tile.
    let depth = skirt_depth(cell);
    let last = n - 1;
    let borders: [(Box<dyn Fn(usize) -> (usize, usize)>, [f32; 3]); 4] = [
        (Box::new(|i| (i, 0)), [0.0, 0.0, -1.0]),
        (Box::new(move |i| (last - i, last)), [0.0, 0.0, 1.0]),
        (Box::new(move |i| (0, last - i)), [-1.0, 0.0, 0.0]),
        (Box::new(move |i| (last, i)), [1.0, 0.0, 0.0]),
    ];
    for (point, normal) in &borders {
        let base = t.positions.len() as u32;
        for i in 0..n {
            let (x, z) = point(i);
            let p = pos(x, z);
            for y in [p[1], p[1] - depth] {
                t.positions.push([p[0], y, p[2]]);
                t.normals.push(*normal);
                t.colors.push(color(x, z));
            }
        }
        for i in 0..(n - 1) as u32 {
            let (top, bottom, next_top, next_bottom) = (base + 2 * i, base + 2 * i + 1, base + 2 * i + 2, base + 2 * i + 3);
            t.indices.extend([top, next_top, bottom, next_top, next_bottom, bottom]);
        }
    }

    StudLodMeshOutput { terrain: t, water: water_sheet(side, samples, cell) }
}

/// The water of a tile: per cell with a wet corner, the highest of its corners'
/// levels and the alpha of its mean depth there; cells of one level and alpha are
/// merged into rectangles (runs along x, extended along z).
fn water_sheet(side: u16, samples: &[StudLodSample], cell: f32) -> WaterMeshData {
    let (n, cells) = (usize::from(side) + 1, usize::from(side));
    let key_of = |x: usize, z: usize| -> Option<(i16, u32)> {
        let corners = [(x, z), (x + 1, z), (x, z + 1), (x + 1, z + 1)].map(|(cx, cz)| samples[cz * n + cx]);
        let level = corners.iter().filter(|c| c.has_water()).map(|c| c.water).max()?;
        let depth_plates: i32 = corners.iter().map(|c| (i32::from(level) - i32::from(c.top)).max(0)).sum();
        let depth_m = plate_units(1) * depth_plates as f32 / 4.0 / UNITS_PER_METER;
        Some((level, water_alpha(depth_m).to_bits()))
    };
    let keys: Vec<Option<(i16, u32)>> = (0..cells * cells).map(|i| key_of(i % cells, i / cells)).collect();
    let mut used = vec![false; cells * cells];
    let mut data = WaterMeshData::default();
    for z in 0..cells {
        let mut x = 0;
        while x < cells {
            let i = z * cells + x;
            let Some(k) = keys[i].filter(|_| !used[i]) else {
                x += 1;
                continue;
            };
            let mut w = 1;
            while x + w < cells && !used[i + w] && keys[i + w] == Some(k) {
                w += 1;
            }
            let mut h = 1;
            while z + h < cells {
                let row = (z + h) * cells + x;
                if (row..row + w).any(|j| used[j] || keys[j] != Some(k)) {
                    break;
                }
                h += 1;
            }
            for dz in 0..h {
                let row = (z + dz) * cells + x;
                used[row..row + w].fill(true);
            }
            data.quad(
                [x as f32 * cell, plate_units(i32::from(k.0)), z as f32 * cell],
                [0.0, 0.0, h as f32 * cell],
                [w as f32 * cell, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                f32::from_bits(k.1),
            );
            x += w;
        }
    }
    data
}

/// The terrain mesh child of a stud LOD root.
#[derive(Component)]
pub struct StudLodTerrain;

/// A stud LOD root (the key's far tile).
#[derive(Component, Clone, Copy, Debug)]
pub struct StudLodRoot(pub LodKey);

/// A finished job's meshes (`None` = nothing to draw).
pub struct StudLodMeshes {
    pub terrain: Option<Mesh>,
    pub water: Option<Mesh>,
}

/// Far tile mesh jobs in flight, by the root they belong to. Dropped with the
/// session (which cancels them where the executor can).
#[derive(Resource, Default)]
pub struct StudLodTasks(Vec<(Entity, Task<StudLodMeshes>)>);

impl StudLodTasks {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn to_meshes(out: StudLodMeshOutput) -> StudLodMeshes {
    StudLodMeshes {
        terrain: (!out.terrain.is_empty()).then(|| super::systems::to_mesh(out.terrain)),
        water: crate::renderer::water::tile_mesh(out.water),
    }
}

/// Spawn `key`'s root at its world offset and start its mesh job; the meshes
/// land as children ([`apply_stud_lod_meshes`]). A tile sized for another key
/// would mesh past (or short of) its root: its root stays empty.
pub fn spawn_stud_lod_root(commands: &mut Commands, tasks: &mut StudLodTasks, key: LodKey, tile: StudLodTile) -> Entity {
    let offset = key.world_offset();
    let root = commands
        .spawn((
            Transform::from_translation(Vec3::from_array(offset)),
            Visibility::default(),
            Name::new(format!("stud lod {:?} x{}", key.coords, key.factor)),
            StudLodRoot(key),
            SessionScoped,
        ))
        .id();
    if tile.side != StudLodTile::side_for(&key) {
        warn!("lod: dropping stud tile of side {} for {key:?} (expected {})", tile.side, StudLodTile::side_for(&key));
        return root;
    }
    let job = StudLodMeshJob { tile, cell: key.factor as f32 };
    tasks.0.push((root, crate::renderer::compute::spawn(job, to_meshes)));
    root
}

/// What [`apply_stud_lod_meshes`] draws with; absent in headless apps (the
/// children are then spawned without meshes' materials).
#[derive(bevy::ecs::system::SystemParam)]
pub struct StudLodAssets<'w> {
    meshes: Option<ResMut<'w, Assets<Mesh>>>,
    stud_materials: Option<ResMut<'w, Assets<StudTerrainMaterial>>>,
    stud: Option<ResMut<'w, StudMaterials>>,
    water_materials: Option<ResMut<'w, Assets<WaterMaterial>>>,
    water: Option<ResMut<'w, WaterMaterialHandle>>,
    fortress: Option<Res<'w, crate::renderer::fortress_view::FortressViewUniform>>,
}

/// Land finished far tile jobs, at most [`STUD_LOD_UPLOADS_PER_FRAME`] a frame:
/// the terrain child draws with the stud material of everything outside the home
/// rect (`StudMaterials::outside`: the near bricks' shading and boundary line,
/// never the cut — a far tile lies where it does not apply), the water child with the
/// water material. Neither casts shadows. A job whose root is gone is dropped.
pub fn apply_stud_lod_meshes(
    mut commands: Commands,
    mut tasks: ResMut<StudLodTasks>,
    roots: Query<(), With<StudLodRoot>>,
    mut assets: StudLodAssets,
) {
    if tasks.0.is_empty() {
        return;
    }
    let mut landed = 0;
    let mut i = 0;
    while i < tasks.0.len() {
        if !roots.contains(tasks.0[i].0) {
            // Retired before its mesh was built (dropping the task cancels it).
            drop(tasks.0.swap_remove(i));
            continue;
        }
        if landed >= STUD_LOD_UPLOADS_PER_FRAME {
            break;
        }
        let Some(out) = block_on(future::poll_once(&mut tasks.0[i].1)) else {
            i += 1;
            continue;
        };
        let (root, _) = tasks.0.swap_remove(i);
        landed += 1;
        let Some(meshes) = assets.meshes.as_mut() else { continue };
        if let (Some(mesh), Some(materials), Some(stud)) = (out.terrain, assets.stud_materials.as_mut(), assets.stud.as_mut()) {
            commands.spawn((
                StudLodTerrain,
                Mesh3d(meshes.add(mesh)),
                // A far tile never lies in the home region's rect: the uncut material.
                MeshMaterial3d(stud.outside(materials)),
                Transform::IDENTITY,
                Visibility::default(),
                NotShadowCaster,
                SessionScoped,
                ChildOf(root),
            ));
        }
        if let (Some(mesh), Some(materials), Some(handle)) = (out.water, assets.water_materials.as_mut(), assets.water.as_mut()) {
            let material = handle.get_or_add(materials, assets.fortress.as_deref().copied().unwrap_or_default());
            commands.spawn((
                LodWaterSurface,
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material),
                Transform::IDENTITY,
                Visibility::default(),
                NotShadowCaster,
                SessionScoped,
                ChildOf(root),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::compute::{decode_output, encode_job, run_encoded, ComputeJob, JobKind};
    use game::{MAT_GRASS, MAT_SAND, MAT_STONE, STUD_LOD_NO_WATER};

    const CELL: f32 = 32.0;

    fn dry(top: i16, material: u16) -> StudLodSample {
        StudLodSample { top, material, water: STUD_LOD_NO_WATER }
    }

    fn lattice(side: u16, f: impl Fn(usize, usize) -> StudLodSample) -> Vec<StudLodSample> {
        let n = usize::from(side) + 1;
        (0..n * n).map(|i| f(i % n, i / n)).collect()
    }

    fn tri_normal(d: &StudMeshData, t: usize) -> Vec3 {
        let p = |k: usize| Vec3::from_array(d.positions[d.indices[3 * t + k] as usize]);
        (p(1) - p(0)).cross(p(2) - p(0))
    }

    /// Vertices and triangles of the sheet alone (the skirts follow it).
    fn sheet(side: u16) -> (usize, usize) {
        let n = usize::from(side) + 1;
        (n * n, 2 * usize::from(side) * usize::from(side))
    }

    /// Every triangle faces the way its vertices' normals say (so back-face
    /// culling keeps it): the sheet up, each skirt out of the tile.
    fn assert_wound_outward(d: &StudMeshData) {
        for t in 0..d.indices.len() / 3 {
            let face = tri_normal(d, t);
            for k in 0..3 {
                let n = Vec3::from_array(d.normals[d.indices[3 * t + k] as usize]);
                assert!(face.dot(n) > 0.0, "triangle {t} faces {face:?} against its normal {n:?}");
            }
        }
    }

    #[test]
    fn a_flat_tile_is_a_level_sheet_with_skirts() {
        let side = 4u16;
        let out = mesh_tile(side, &lattice(side, |_, _| dry(10, MAT_GRASS)), CELL);
        assert!(out.water.is_empty());
        let d = &out.terrain;
        let (verts, tris) = sheet(side);
        let n = usize::from(side) + 1;
        assert_eq!(d.positions.len(), verts + 4 * 2 * n, "one vertex per lattice point, two per border point per skirt");
        assert_eq!(d.indices.len() / 3, tris + 4 * 2 * usize::from(side));
        let y = plate_units(10);
        let grass = stud_assets().brick_colors(game::stud_material_color(MAT_GRASS)).0;
        for i in 0..verts {
            assert_eq!(d.positions[i], [(i % n) as f32 * CELL, y, (i / n) as f32 * CELL]);
            assert_eq!(d.normals[i], [0.0, 1.0, 0.0]);
            assert_eq!(d.colors[i], [grass[0], grass[1], grass[2], 1.0]);
        }
        // The sheet spans the whole key and nothing more.
        let span = f32::from(side) * CELL;
        assert!(d.positions.iter().all(|p| (0.0..=span).contains(&p[0]) && (0.0..=span).contains(&p[2])));
        // Skirts hang one cell down from the border, facing out.
        let low = d.positions[verts..].iter().map(|p| p[1]).fold(f32::MAX, f32::min);
        assert_eq!(low, y - skirt_depth(CELL));
        assert!(d.positions[verts..].iter().all(|p| p[0] == 0.0 || p[0] == span || p[2] == 0.0 || p[2] == span));
        assert_wound_outward(d);
    }

    /// A step between two columns of samples: the cells across it slope by the
    /// step (no hole, no wall of its own), the vertices beside it lean away from
    /// the high side, and each side keeps its own material's colour.
    #[test]
    fn a_step_is_a_slope_between_its_lattice_columns() {
        let side = 4u16;
        let n = usize::from(side) + 1;
        let out = mesh_tile(side, &lattice(side, |x, _| if x < 2 { dry(0, MAT_SAND) } else { dry(15, MAT_STONE) }), CELL);
        let d = &out.terrain;
        let rise = plate_units(15);
        let (verts, tris) = sheet(side);
        for t in 0..tris {
            let ys: Vec<f32> = (0..3).map(|k| d.positions[d.indices[3 * t + k] as usize][1]).collect();
            let xs: Vec<f32> = (0..3).map(|k| d.positions[d.indices[3 * t + k] as usize][0]).collect();
            let across = xs.iter().any(|&x| x == CELL) && xs.iter().any(|&x| x == 2.0 * CELL);
            let span = ys.iter().cloned().fold(f32::MIN, f32::max) - ys.iter().cloned().fold(f32::MAX, f32::min);
            assert_eq!(span, if across { rise } else { 0.0 }, "triangle {t}");
            assert!(tri_normal(d, t).y > 0.0, "the sheet faces up");
        }
        let sand = stud_assets().brick_colors(game::stud_material_color(MAT_SAND)).0;
        let stone = stud_assets().brick_colors(game::stud_material_color(MAT_STONE)).0;
        assert_ne!(sand, stone);
        for i in 0..verts {
            let x = i % n;
            let normal = Vec3::from_array(d.normals[i]);
            assert!((normal.length() - 1.0).abs() < 1e-5);
            match x {
                // Beside the step: leaning towards -x (downhill), still mostly up or level.
                1 | 2 => assert!(normal.x < -0.1 && normal.y > 0.0 && normal.z == 0.0, "vertex {i}: {normal:?}"),
                _ => assert_eq!(normal, Vec3::Y, "vertex {i}"),
            }
            assert_eq!(&d.colors[i][..3], if x < 2 { &sand[..3] } else { &stone[..3] });
        }
        // The skirts follow the stepped border: their tops are the border samples.
        let tops: Vec<[f32; 3]> = d.positions[verts..].iter().step_by(2).copied().collect();
        assert!(tops.iter().all(|p| p[1] == if p[0] < 2.0 * CELL { 0.0 } else { rise }));
        assert_wound_outward(d);
    }

    /// Water: a flat sheet over every cell with a wet corner at the water's
    /// level, merged where level and depth agree; dry cells get none.
    #[test]
    fn water_is_a_flat_sheet_over_the_wet_cells() {
        let side = 4u16;
        // A basin in x < 2 (lattice columns 0 and 1 wet), deep and level; land beyond.
        let samples = lattice(side, |x, _| {
            if x < 2 { StudLodSample::of_column(-60, MAT_SAND, Some(5)) } else { dry(20, MAT_GRASS) }
        });
        let out = mesh_tile(side, &samples, CELL);
        let w = &out.water;
        let level = plate_units(5);
        assert!(w.positions.iter().all(|p| p[1] == level), "one level");
        assert!(w.normals.iter().all(|&n| n == [0.0, 1.0, 0.0]));
        // Cells x = 0 (all four corners wet) and x = 1 (the shore: two wet corners).
        let area: f32 = (0..w.indices.len() / 3)
            .map(|t| {
                let p = |k: usize| Vec3::from_array(w.positions[w.indices[3 * t + k] as usize]);
                let n = (p(1) - p(0)).cross(p(2) - p(0));
                assert!(n.y > 0.0, "the water faces up");
                n.length() / 2.0
            })
            .sum();
        assert_eq!(area, 2.0 * 4.0 * CELL * CELL);
        assert!(w.positions.iter().all(|p| p[0] <= 2.0 * CELL), "nothing over the dry cells");
        // Deep water merges along z: the full-depth column is one quad, the shore another.
        assert_eq!(w.positions.len(), 2 * 4);
        let alphas: std::collections::BTreeSet<u32> = w.colors.iter().map(|c| c[3].to_bits()).collect();
        assert_eq!(alphas.len(), 2, "the shore is shallower on average");
        // The ground under the water is still drawn (the sea floor shows through).
        assert_eq!(out.terrain.positions[0][1], plate_units(-60));
        // A dry tile has no water mesh at all.
        assert!(mesh_tile(side, &lattice(side, |_, _| dry(0, MAT_GRASS)), CELL).water.is_empty());
    }

    #[test]
    fn the_job_round_trips_and_a_malformed_tile_draws_nothing() {
        let side = 3u16;
        let samples = lattice(side, |x, z| StudLodSample::of_column((x * 7 + z) as i32, MAT_GRASS, (x == 0).then_some(40)));
        let job = || StudLodMeshJob { tile: StudLodTile::encode(side, &samples), cell: CELL };
        let direct = job().run();
        assert_eq!(direct, mesh_tile(side, &samples, CELL));
        assert!(!direct.terrain.is_empty() && !direct.water.is_empty());
        assert_eq!(JobKind::from_u8(JobKind::StudLodMesh as u8), Some(JobKind::StudLodMesh));
        let through_worker = decode_output::<StudLodMeshJob>(&run_encoded(StudLodMeshJob::KIND, &encode_job(&job())));
        assert_eq!(through_worker, direct);

        let mut bad = job();
        bad.tile.heights.pop();
        assert_eq!(bad.run(), StudLodMeshOutput::default());
        assert_eq!(mesh_tile(0, &[dry(0, MAT_GRASS)], CELL), StudLodMeshOutput::default());
    }
}
