//! Coarse twins: distance LOD inside the simulated neighbourhood.
//!
//! Every simulated region draws its sim chunks at full detail — chiselled
//! branches, half blocks, cutout leaves — out to the far edge of the simulated
//! neighbourhood (~400–700 m from the camera). Measured 2026-11-05 on the Iris
//! Xe, a 3×3 at 256 m regions, horizon view: 22.8M near quads, a
//! geometry-bound 23.5 ms main pass and a 15.6 ms far shadow cascade.
//!
//! So each sim chunk with quads also gets a COARSE TWIN: its blocks
//! downsampled to block-scale cells (`game::lod::block_scale_cells`, the
//! server's own factor-16 LOD rule: one cell per block, chisel detail dropped)
//! and meshed by the LOD mesher (`lod::lod_quads`: leaves as solid foliage, a
//! one-cell skirt). The twin is a child of the chunk entity at the LOD cell
//! placement (scale 16, one cell down for the skirt bias), with its own
//! `ChunkQuads` and `voxel_pull::DetailBand::Coarse`; the chunk turns
//! `DetailBand::Fine` once its twin is meshed. The GPU cull compute then draws
//! the chunk within `voxel_pull::NEAR_DETAIL_DISTANCE_M` of the main camera
//! (horizontal distance to the chunk's xz rect, which the twin shares) and
//! the twin beyond it — in every view from the same eye, so shadows match.
//!
//! A twin is rebuilt whenever its chunk's merged mesh is (`rebuild_chunk_meshes`
//! marks [`CoarseDirty`]): joins and edits alike. GPU-driven path only; the
//! CPU-culled backends skip twins (`prepare.rs`) and `VOXEL_NEAR_DETAIL_M=0`
//! builds none.
//!
//! A twin is padded with its six neighbours' boundary blocks (read from
//! `bricks::SimChunkIndex` when the build starts), so it shows no walls at
//! chunk borders and the z-level cut's cap works through it. A neighbour that
//! lands or changes later re-meshes its border bricks, which rebuilds both
//! chunks and so both twins. Known trade-off: the any-solid block rule rounds
//! half blocks up, so the coarse surface can sit up to half a block higher
//! where the switch happens.

use std::sync::Arc;

use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoAutoAabb;
use bevy::math::Vec3A;
use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, Task};

use super::super::block_textures::BlockTextureLayers;
use super::super::bricks::SimChunk;
use super::super::voxel_pull::{near_detail_distance_m, ChunkQuads, DetailBand};
use super::records::{self, QuadRecord};

/// Cell size of a coarse twin in voxels: one block (`game::lod` factor 16).
pub const COARSE_FACTOR: u32 = 16;

/// Coarse twin builds started per frame (each is a ~4k-block downsample and
/// a 32³ greedy mesh on the compute pool).
pub const COARSE_SPAWNS_PER_FRAME: usize = 64;

/// On a sim chunk whose coarse twin is out of date.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct CoarseDirty;

/// On a sim chunk: its coarse twin entity (a child).
#[derive(Component, Clone, Copy, Debug)]
pub struct CoarseTwin(pub Entity);

/// On a sim chunk: the twin build in flight.
#[derive(Component)]
pub struct CoarseTask(Task<Vec<QuadRecord>>);

/// On a sim chunk whose own quads and whose twin both exist but one of them
/// only just arrived: both draw at every distance for this many more frames,
/// THEN the chunk turns [`DetailBand::Fine`] and the twin
/// [`DetailBand::Coarse`] ([`finish_band_handovers`]). Quads are extracted
/// and uploaded on the render sub-app a frame or two after they land; handing
/// the band over on the landing frame left the chunk drawing nothing
/// meanwhile — patches of sky flashing through the terrain as full detail
/// arrived around a walking player (seen in play, 2026-11-19).
#[derive(Component, Clone, Copy, Debug)]
pub struct BandHandover(pub u32);

/// Frames a [`BandHandover`] lasts.
pub const BAND_HANDOVER_FRAMES: u32 = 6;

/// Count the handovers down and switch the bands at the end.
pub fn finish_band_handovers(
    mut commands: Commands,
    mut chunks: Query<(Entity, &mut BandHandover, Option<&CoarseTwin>, Has<ChunkQuads>)>,
) {
    for (chunk, mut left, twin, has_quads) in &mut chunks {
        if left.0 > 0 {
            left.0 -= 1;
            continue;
        }
        let mut ec = commands.entity(chunk);
        ec.remove::<BandHandover>();
        if let (Some(&CoarseTwin(t)), true) = (twin, has_quads) {
            ec.insert(DetailBand::Fine);
            commands.entity(t).insert(DetailBand::Coarse);
        }
    }
}

/// A chunk landed without bricks (`bricks::FineDeferred`) whose twin — all it
/// draws — is not built yet: it counts as unmeshed wherever a region's
/// terrain is waited for (the loading cover, the LOD hand-over).
pub type AwaitingTwinFilter = (With<super::super::bricks::FineDeferred>, Or<(With<CoarseDirty>, With<CoarseTask>)>);

/// Whether coarse twins are built at all (`VOXEL_NEAR_DETAIL_M` > 0).
pub fn coarse_twins_enabled() -> bool {
    near_detail_distance_m() > 0.0
}

/// Face directions of a chunk's neighbours, in [`CoarseMeshJob::slabs`] order.
pub const FACE_DIRS: [[i32; 3]; 6] = [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]];

/// Which of a neighbour's boundary blocks (the layer touching our face) are
/// solid, `CHUNK_BLOCKS`² of them indexed `u + CHUNK_BLOCKS * v` over the two
/// other axes in x, y, z order. `None`: no neighbour chunk there.
pub type Slab = Option<Vec<bool>>;

/// Downsample one sim chunk to block cells and mesh it as LOD records
/// (a compute job, `renderer::compute`). `slabs` pad it with its six
/// neighbours' boundary blocks (see [`coarse_records`]).
#[derive(serde::Serialize, serde::Deserialize)]
pub struct CoarseMeshJob {
    pub chunk: game::Chunk,
    pub slabs: [Slab; 6],
    pub layers: BlockTextureLayers,
}

impl super::super::compute::ComputeJob for CoarseMeshJob {
    type Output = Vec<QuadRecord>;
    const KIND: super::super::compute::JobKind = super::super::compute::JobKind::CoarseMesh;

    fn run(self) -> Vec<QuadRecord> {
        coarse_records(self.chunk, &self.slabs, &self.layers)
    }
}

/// Stand-in block of a solid neighbour cell. Greedy meshing never merges it
/// with a real block, so its own quads can be dropped whole.
const PAD: game::BlockId = game::BlockId(u16::MAX);

/// The coarse twin's records: the chunk at chunk-local coords (0, 0, 0), so
/// its blocks fill cells 0..CHUNK_BLOCKS of one 32³ grid, padded with its
/// neighbours' boundary blocks so no face is emitted against solid ground —
/// a twin then has no walls at chunk borders, and the z-level cut's cap (back
/// faces below the plane) shows through it exactly as through a full mesh.
/// The +x/+y/+z neighbours sit INSIDE the grid at cell `CHUNK_BLOCKS` (their
/// quads are dropped), the −x/−y/−z ones in the mesher's padding shell. A
/// missing chunk below is implied ground (solid, as the near mesher assumes);
/// missing elsewhere is air.
pub fn coarse_records(chunk: game::Chunk, slabs: &[Slab; 6], layers: &BlockTextureLayers) -> Vec<QuadRecord> {
    const N: usize = game::CHUNK_BLOCKS;
    const G: usize = game::BRICK_VOXELS;
    let cells = game::lod::block_scale_cells(vec![(game::ChunkCoords::new(0, 0, 0), chunk)]);
    let Some(cell) = cells.into_iter().find(|c| c.group == [0, 0, 0]) else {
        return Vec::new();
    };
    let mut grid = cell.grid;
    // Liquid is empty to the terrain mesh (the water pass draws it).
    let props = game::block_props();
    for v in grid.iter_mut().filter(|v| props.is_liquid(v.block)) {
        *v = game::Voxel::default();
    }
    let mut shells: std::collections::BTreeMap<[i32; 3], Vec<game::Voxel>> = Default::default();
    for (dir, slab) in FACE_DIRS.iter().zip(slabs) {
        let axis = dir.iter().position(|&d| d != 0).unwrap();
        let (a, b) = match axis {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        let solid = |u: usize, v: usize| match slab {
            Some(s) => s[u + N * v],
            None => *dir == [0, -1, 0],
        };
        let positive = dir[axis] > 0;
        let shell = (!positive).then(|| shells.entry(*dir).or_insert_with(|| vec![game::Voxel::default(); G * G * G]));
        let mut shell = shell;
        for v in 0..N {
            for u in 0..N {
                if !solid(u, v) {
                    continue;
                }
                let mut c = [0usize; 3];
                c[a] = u;
                c[b] = v;
                c[axis] = if positive { N } else { G - 1 };
                let i = c[0] + G * c[1] + G * G * c[2];
                match shell.as_deref_mut() {
                    Some(s) => s[i] = game::Voxel::new(PAD),
                    None => grid[i] = game::Voxel::new(PAD),
                }
            }
        }
    }
    let neighbors = shells.iter().map(|(&k, v)| (k, v.as_slice())).collect();
    let mut quads = super::lod::lod_quads(&grid, &neighbors);
    quads.retain(|q| q.block != PAD);
    records::quads_to_records(&quads, layers)
}

/// Solidity of `neighbour`'s boundary layer facing a chunk on its `dir` side
/// (see [`Slab`]): only opaque WHOLE blocks count — not chiselled ones (half
/// blocks) and not cutout ones (leaves). The neighbour may draw at full
/// detail while this twin draws coarse, and the twin's any-solid cells round
/// a half block up: if it trusted a half-block neighbour it would skip the
/// riser and leave a half-block slit to see through (in the cut view, the
/// cap colour of the back faces behind).
pub fn boundary_slab(neighbour: &game::Chunk, dir: [i32; 3]) -> Vec<bool> {
    const N: usize = game::CHUNK_BLOCKS;
    let axis = dir.iter().position(|&d| d != 0).unwrap();
    // The neighbour at +dir touches us with its low layer, at -dir its high one.
    let layer = if dir[axis] > 0 { 0 } else { N - 1 };
    let mut out = vec![false; N * N];
    for v in 0..N {
        for u in 0..N {
            let c = match axis {
                0 => [layer, u, v],
                1 => [u, layer, v],
                _ => [u, v, layer],
            };
            let id = neighbour.block_at(c[0], c[1], c[2]).id;
            out[u + N * v] = game::block_props().blocks_ray(id)
                && !game::block_props().is_cutout(id)
                && !neighbour.chisel.contains_key(&game::BlockIndex::from_xyz(c[0], c[1], c[2]));
        }
    }
    out
}

/// World sim-chunk key of a chunk entity at `translation` (its global
/// origin), as `bricks::SimChunkIndex` keys it.
pub fn chunk_key(translation: Vec3) -> [i32; 3] {
    let per = game::BRICKS_PER_CHUNK_EDGE as i32;
    let k = translation / game::BRICK_VOXELS as f32;
    [k.x.round() as i32, k.y.round() as i32, k.z.round() as i32].map(|v| v.div_euclid(per))
}

/// The twin's transform under its chunk: LOD cell placement at factor 16
/// (`lod_rings::spawn_lod_root`: scale = factor, one cell down for the
/// mesher's +1 skirt bias).
pub fn twin_transform() -> Transform {
    Transform {
        translation: Vec3::new(0.0, -(COARSE_FACTOR as f32), 0.0),
        scale: Vec3::splat(COARSE_FACTOR as f32),
        ..default()
    }
}

/// The twin's bounds in cells: the chunk's cube (x/z exactly, so the cull's
/// band test sees the same rect as the chunk's) plus the skirt's cell of y.
pub fn twin_aabb() -> Aabb {
    let half = game::CHUNK_BLOCKS as f32 / 2.0;
    Aabb { center: Vec3A::new(half, half + 0.5, half), half_extents: Vec3A::new(half, half + 0.5, half) }
}

/// Start twin builds for dirty chunks, nearest to the camera first: a chunk
/// that landed without bricks (`bricks::FineDeferred`) draws nothing until
/// its twin is built, and the near ones are the ones in view.
pub fn spawn_coarse_tasks(
    mut commands: Commands,
    dirty: Query<(Entity, &SimChunk, &Transform, &ChildOf, Option<&CoarseTwin>), (With<CoarseDirty>, Without<CoarseTask>)>,
    // A chunk may ask for its twin on the frame it lands, before its
    // `GlobalTransform` is propagated: its origin is root + own translation.
    roots: Query<&Transform, Without<SimChunk>>,
    all: Query<&SimChunk>,
    index: Option<Res<super::super::bricks::SimChunkIndex>>,
    layers: Res<BlockTextureLayers>,
    camera: Query<&GlobalTransform, With<Camera3d>>,
) {
    if dirty.is_empty() {
        return;
    }
    let eye = camera.iter().next().map(|t| t.translation());
    let origin = |tf: &Transform, parent: &ChildOf| roots.get(parent.parent()).map_or(Vec3::ZERO, |r| r.translation) + tf.translation;
    let mut order: Vec<(f32, Entity)> = dirty
        .iter()
        .map(|(e, _, tf, parent, _)| (eye.map_or(0.0, |eye| eye.distance_squared(origin(tf, parent))), e))
        .collect();
    if order.len() > COARSE_SPAWNS_PER_FRAME {
        order.select_nth_unstable_by(COARSE_SPAWNS_PER_FRAME, |a, b| a.0.total_cmp(&b.0));
        order.truncate(COARSE_SPAWNS_PER_FRAME);
    }
    order.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, e) in order {
        let Ok((_, sim, tf, parent, twin)) = dirty.get(e) else { continue };
        // All air (most of the sky's chunks): no twin, and no job to find out.
        if sim.0.chisel.is_empty() && sim.0.blocks.uniform().is_some_and(|b| b.id == game::BlockId::AIR) {
            commands.entity(e).remove::<CoarseDirty>();
            drop_twin(&mut commands, e, twin);
            continue;
        }
        let key = chunk_key(origin(tf, parent));
        let slabs: [Slab; 6] = FACE_DIRS.map(|d| {
            let n = [key[0] + d[0], key[1] + d[1], key[2] + d[2]];
            let neighbour = index.as_deref().and_then(|i| i.0.get(&n)).and_then(|&ne| all.get(ne).ok())?;
            Some(boundary_slab(&neighbour.0, d))
        });
        let job = CoarseMeshJob { chunk: sim.0.clone(), slabs, layers: layers.clone() };
        let task = super::super::compute::spawn(job, |records| records);
        commands.entity(e).remove::<CoarseDirty>().insert(CoarseTask(task));
    }
}

/// Land finished twin builds: create / update / drop the twin and set the
/// bands. A twin stands on its own — it is what a chunk without bricks
/// (`bricks::FineDeferred`) draws, and what a chunk still meshing its bricks
/// keeps drawing meanwhile: with the chunk's own quads it is
/// [`DetailBand::Coarse`] and the chunk [`DetailBand::Fine`]; without, it is
/// drawn at every distance. A twin that is new beside existing quads takes
/// its band through a [`BandHandover`].
pub fn apply_coarse_tasks(
    mut commands: Commands,
    mut tasks: Query<(Entity, &mut CoarseTask, Option<&CoarseTwin>, Has<ChunkQuads>, Has<BandHandover>)>,
) {
    for (chunk, mut task, twin, has_quads, handing_over) in &mut tasks {
        let Some(records) = block_on(future::poll_once(&mut task.0)) else {
            continue;
        };
        commands.entity(chunk).remove::<CoarseTask>();
        if records.is_empty() {
            drop_twin(&mut commands, chunk, twin);
            continue;
        }
        let quads = ChunkQuads(Arc::new(records));
        match twin {
            // Rebuilt: it keeps the band it has (a handover in progress, or
            // the one `rebuild_chunk_meshes` / a finished handover gave it).
            Some(&CoarseTwin(t)) => {
                commands.entity(t).insert(quads);
                if !has_quads && !handing_over {
                    commands.entity(t).insert(DetailBand::Always);
                }
            }
            None => {
                let t = commands
                    .spawn((twin_transform(), twin_aabb(), NoAutoAabb, quads, DetailBand::Always, ChildOf(chunk)))
                    .id();
                let mut ec = commands.entity(chunk);
                ec.insert(CoarseTwin(t));
                if has_quads {
                    ec.insert((DetailBand::Always, BandHandover(BAND_HANDOVER_FRAMES)));
                }
            }
        }
    }
}


fn drop_twin(commands: &mut Commands, chunk: Entity, twin: Option<&CoarseTwin>) {
    if let Some(&CoarseTwin(t)) = twin {
        commands.entity(t).try_despawn();
    }
    if let Ok(mut ec) = commands.get_entity(chunk) {
        ec.remove::<(CoarseTwin, BandHandover)>().insert(DetailBand::Always);
    }
}
