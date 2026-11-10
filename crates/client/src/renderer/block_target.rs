//! Minecraft-style feedback on the block in front of a first-person body:
//!
//! - **Highlight**: a thin dark outline round the block the eye ray hits —
//!   the same walk as the sim's `Rollback::eye_hit` (`game::edit::raycast_blocks`,
//!   any non-air block within `game::edit::REACH`), over the home region's
//!   [`TileMirror`], from the render camera's eye every frame, so it follows
//!   the mouse look without waiting for a tick. Water is not outlined (it
//!   cannot be mined).
//! - **Cracks**: while the sim says the local player is mining a block
//!   (`HudItems::mining`, the home region's `game::MiningView`), a cube a
//!   hair larger than the block is drawn over it with one of
//!   [`CRACK_STAGES`] crack textures, picked from the progress — Minecraft's
//!   destroy stages. The textures are generated here
//!   ([`crack_stage_pixels`]), not loaded: every stage reveals more of the
//!   same deterministic crack network, so the damage grows instead of
//!   jumping.
//!
//! Render-only: nothing here touches the sim.

use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use game::rapier3d::math::Vector;
use game::{TileSource, BLOCK_VOXELS};

use super::bridge::LocalBody;
use super::commander::tiles::TileMirror;
use super::slint_overlay::HudItems;
use crate::session::{SessionResetExt, SessionScoped};

/// Destroy stages, as in Minecraft.
pub const CRACK_STAGES: usize = 10;
/// Crack texture edge, texels (one per voxel: a block is 16 voxels).
pub const CRACK_TEXELS: u32 = 16;
/// Crack texel opacity (dark cracks blended over the block).
pub const CRACK_ALPHA: f32 = 0.75;
/// Seed of the crack network (any value; fixed so every run looks alike).
const CRACK_SEED: u64 = 0x5EED_C4AC_0B10_C0DE;
/// The crack cube's edge over the block's, so it sits just outside the
/// block's faces instead of z-fighting them.
pub const CRACK_INFLATE: f32 = 1.004;
/// The outline box's edge over the block's.
pub const HIGHLIGHT_INFLATE: f32 = 1.004;
/// Outline colour: Minecraft's translucent black.
pub const HIGHLIGHT_COLOR: Color = Color::srgba(0.0, 0.0, 0.0, 0.45);
/// Outline width, logical pixels.
pub const HIGHLIGHT_LINE_PX: f32 = 2.0;

/// Gizmo group for the highlight, so its line width and depth bias are its own.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct BlockHighlightGizmos;

/// The block the first-person eye ray hits (home region, region-local), if
/// any — what a break would take.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct LookedAtBlock(pub Option<game::edit::BlockPos>);

/// The crack overlay entity and its stage materials.
#[derive(Resource)]
pub struct CrackAssets {
    mesh: Handle<Mesh>,
    stages: Vec<Handle<StandardMaterial>>,
}

/// The crack overlay currently drawn (spawned on first use).
#[derive(Resource, Default)]
pub struct CrackOverlay(Option<Entity>);

impl FromWorld for CrackAssets {
    fn from_world(world: &mut World) -> Self {
        let edge = BLOCK_VOXELS as f32 * CRACK_INFLATE;
        let mesh = world.resource_mut::<Assets<Mesh>>().add(Cuboid::new(edge, edge, edge));
        let images: Vec<Handle<Image>> = (0..CRACK_STAGES)
            .map(|stage| {
                let mut image = Image::new(
                    Extent3d { width: CRACK_TEXELS, height: CRACK_TEXELS, depth_or_array_layers: 1 },
                    TextureDimension::D2,
                    crack_stage_pixels(stage),
                    TextureFormat::Rgba8UnormSrgb,
                    RenderAssetUsages::RENDER_WORLD,
                );
                image.sampler = ImageSampler::nearest();
                world.resource_mut::<Assets<Image>>().add(image)
            })
            .collect();
        let mut mats = world.resource_mut::<Assets<StandardMaterial>>();
        let stages = images
            .into_iter()
            .map(|image| {
                mats.add(StandardMaterial {
                    base_color_texture: Some(image),
                    alpha_mode: AlphaMode::Blend,
                    unlit: true,
                    ..default()
                })
            })
            .collect();
        Self { mesh, stages }
    }
}

/// Tiny deterministic generator for the crack network.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// The crack network's texels in the order they crack: random walks out of
/// a few starting points near the centre, each step one texel, with forks,
/// interleaved so every branch grows together. Each texel appears once.
pub fn crack_order() -> Vec<(u32, u32)> {
    const STEPS: [(i32, i32); 8] = [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)];
    let n = CRACK_TEXELS as i32;
    let mut rng = SplitMix(CRACK_SEED);
    let mut seen = vec![false; (n * n) as usize];
    let mut order = Vec::new();
    // (position, heading index into STEPS, steps left).
    let mut walkers: Vec<((i32, i32), usize, u32)> = Vec::new();
    let c = n / 2;
    for i in 0..4 {
        let start = (c - 2 + rng.below(4) as i32, c - 2 + rng.below(4) as i32);
        walkers.push((start, i * 2 + rng.below(2) as usize, 6 + rng.below(5) as u32));
    }
    while !walkers.is_empty() {
        let mut next = Vec::new();
        for ((x, y), heading, left) in walkers {
            let idx = (y * n + x) as usize;
            if !seen[idx] {
                seen[idx] = true;
                order.push((x as u32, y as u32));
            }
            if left == 0 {
                continue;
            }
            // Wander: keep the heading, or turn one step either way.
            let turn = rng.below(4);
            let heading = match turn {
                0 => (heading + 1) % 8,
                1 => (heading + 7) % 8,
                _ => heading,
            };
            let (dx, dy) = STEPS[heading];
            let (nx, ny) = (x + dx, y + dy);
            if !(0..n).contains(&nx) || !(0..n).contains(&ny) {
                continue;
            }
            next.push(((nx, ny), heading, left - 1));
            // Fork now and then: a shorter branch off at an angle.
            if rng.below(5) == 0 {
                let side = if rng.below(2) == 0 { (heading + 2) % 8 } else { (heading + 6) % 8 };
                next.push(((nx, ny), side, (left / 2).max(1)));
            }
        }
        walkers = next;
    }
    order
}

/// RGBA8 (sRGB) pixels of destroy stage `stage` (0-based, `< CRACK_STAGES`):
/// the first `(stage + 1) / CRACK_STAGES` of [`crack_order`] dark, the rest
/// clear.
pub fn crack_stage_pixels(stage: usize) -> Vec<u8> {
    let order = crack_order();
    let shown = order.len() * (stage + 1) / CRACK_STAGES;
    let mut px = vec![0u8; (CRACK_TEXELS * CRACK_TEXELS * 4) as usize];
    let alpha = (CRACK_ALPHA * 255.0).round() as u8;
    for &(x, y) in &order[..shown] {
        let i = ((y * CRACK_TEXELS + x) * 4) as usize;
        px[i..i + 4].copy_from_slice(&[12, 10, 8, alpha]);
    }
    px
}

/// The destroy stage for mining progress `per_mille` (0..=1000).
pub fn crack_stage(per_mille: u32) -> usize {
    ((per_mille as usize * CRACK_STAGES) / 1000).min(CRACK_STAGES - 1)
}

/// World-space centre of region-local block `p` of `region`.
fn block_centre(region: game::RegionId, p: game::edit::BlockPos) -> Vec3 {
    let size = BLOCK_VOXELS as f32;
    let local = Vec3::new(p[0] as f32 + 0.5, p[1] as f32 + 0.5, p[2] as f32 + 0.5) * size;
    local + Vec3::from_array(region.world_offset())
}

/// Find the block under the first-person crosshair.
pub fn update_looked_at(
    eyes: Query<&GlobalTransform, (With<LocalBody>, With<Camera3d>)>,
    mirror: Res<TileMirror>,
    mut looked: ResMut<LookedAtBlock>,
) {
    let hit = (|| {
        let region = mirror.region()?;
        let eye = eyes.iter().next()?;
        let origin = eye.translation() - Vec3::from_array(region.world_offset());
        let dir = eye.forward();
        // Stud terrain: the brick under the crosshair, not a block (`update_looked_at_stud`).
        if mirror.terrain() == game::TerrainKind::Stud {
            return None;
        }
        let hit = game::edit::raycast_blocks(
            Vector::new(origin.x, origin.y, origin.z),
            Vector::new(dir.x, dir.y, dir.z),
            game::edit::REACH,
            |p| game::block_props().blocks_ray(mirror.block(p)),
        )?;
        Some(hit.block)
    })();
    looked.set_if_neq(LookedAtBlock(hit));
}

/// The brick the first-person eye ray hits on stud terrain, as its world-space
/// box (min, max) — in whichever loaded region (or stud halo) it lies.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq)]
pub struct LookedAtStud(pub Option<(Vec3, Vec3)>);

/// The world-space box of the brick a stud ray hit (the hit cell alone when the
/// chunk no longer holds a brick there).
pub fn stud_hit_box(hit: &game::StudWorldHit, chunk: Option<&game::StudChunk>) -> (Vec3, Vec3) {
    let (min, size) = match chunk.and_then(|c| c.brick_at(hit.cell)) {
        Some(b) => (b.min, b.size()),
        None => (hit.cell, [1, 1, 1]),
    };
    let cell = |c: [i32; 3]| Vec3::new((c[0] * game::STUD_UNITS as i32) as f32, game::plate_units(c[1]), (c[2] * game::STUD_UNITS as i32) as f32);
    let origin = super::stud::world::world_chunk_origin(hit.chunk);
    let lo = min.map(i32::from);
    let hi = [lo[0] + size[0] as i32, lo[1] + size[1] as i32, lo[2] + size[2] as i32];
    (origin + cell(lo), origin + cell(hi))
}

/// Find the brick under the first-person crosshair on stud terrain: the sim's
/// `WorldView::stud_raycast` walk over the client's stud mirror, within the
/// block reach.
pub fn update_looked_at_stud(
    eyes: Query<&GlobalTransform, (With<LocalBody>, With<Camera3d>)>,
    mirror: Res<TileMirror>,
    world: Option<Res<super::stud::world::StudWorld>>,
    chunks: Query<&super::stud::SimStudChunk>,
    mut looked: ResMut<LookedAtStud>,
) {
    let hit = (|| {
        let world = world.as_deref()?;
        let region = mirror.region().filter(|_| mirror.terrain() == game::TerrainKind::Stud)?;
        let eye = eyes.iter().next()?;
        let origin = eye.translation() - Vec3::from_array(region.world_offset());
        let dir = eye.forward();
        let chunk_of = |e: Entity| chunks.get(e).ok().map(|c| &c.chunk);
        let reach = game::edit::REACH * game::TerrainKind::Stud.tile_width_units();
        let hit = super::stud::world::stud_world_raycast(
            world,
            chunk_of,
            region,
            Vector::new(origin.x, origin.y, origin.z),
            Vector::new(dir.x, dir.y, dir.z),
            reach,
        )?;
        let chunk = match world.entity_at(hit.chunk) {
            Some(e) => chunk_of(e),
            None => world.halo_chunk(hit.chunk),
        };
        Some(stud_hit_box(&hit, chunk))
    })();
    looked.set_if_neq(LookedAtStud(hit));
}

/// Outline the looked-at brick (stud terrain).
pub fn draw_stud_highlight(looked: Res<LookedAtStud>, mut gizmos: Gizmos<BlockHighlightGizmos>) {
    let Some((lo, hi)) = looked.0 else { return };
    let tf = Transform::from_translation((lo + hi) / 2.0).with_scale((hi - lo) * HIGHLIGHT_INFLATE);
    gizmos.cube(tf, HIGHLIGHT_COLOR);
}

/// Outline the looked-at block.
pub fn draw_highlight(
    looked: Res<LookedAtBlock>,
    mirror: Res<TileMirror>,
    mut gizmos: Gizmos<BlockHighlightGizmos>,
) {
    let (Some(p), Some(region)) = (looked.0, mirror.region()) else { return };
    let edge = BLOCK_VOXELS as f32 * HIGHLIGHT_INFLATE;
    gizmos.cube(Transform::from_translation(block_centre(region, p)).with_scale(Vec3::splat(edge)), HIGHLIGHT_COLOR);
}

/// Where the crack cube goes and at which stage: the block being mined in
/// `region`, once it has taken damage. Nothing on stud terrain: the cube and
/// its texture are a voxel BLOCK's (a metre cube on the block grid), and the
/// sim has no survival mining on studs yet — should it report one before the
/// cracks are ported to bricks, a block-sized cube must not be drawn on them.
pub fn crack_target(
    mining: Option<game::MiningView>,
    region: Option<game::RegionId>,
    terrain: game::TerrainKind,
) -> Option<(Vec3, usize)> {
    if terrain == game::TerrainKind::Stud {
        return None;
    }
    let (m, region) = (mining?, region?);
    (m.progress_per_mille > 0).then(|| (block_centre(region, m.target), crack_stage(m.progress_per_mille)))
}

/// Show the crack stage of the block being mined, or hide the overlay.
pub fn update_cracks(
    mut commands: Commands,
    items: Option<Res<HudItems>>,
    mirror: Res<TileMirror>,
    assets: Res<CrackAssets>,
    mut overlay: ResMut<CrackOverlay>,
    mut shown: Query<(&mut Transform, &mut MeshMaterial3d<StandardMaterial>, &mut Visibility)>,
) {
    let want = crack_target(items.as_ref().and_then(|i| i.mining), mirror.region(), mirror.terrain());
    let existing = overlay.0.and_then(|e| shown.get_mut(e).ok());
    match (want, existing) {
        (Some((at, stage)), Some((mut tf, mut mat, mut vis))) => {
            if tf.translation != at {
                tf.translation = at;
            }
            if mat.0 != assets.stages[stage] {
                mat.0 = assets.stages[stage].clone();
            }
            vis.set_if_neq(Visibility::Visible);
        }
        (Some((at, stage)), None) => {
            let e = commands
                .spawn((
                    Mesh3d(assets.mesh.clone()),
                    MeshMaterial3d(assets.stages[stage].clone()),
                    Transform::from_translation(at),
                    Visibility::Visible,
                    NotShadowCaster,
                    NotShadowReceiver,
                    SessionScoped,
                ))
                .id();
            overlay.0 = Some(e);
        }
        (None, Some((_, _, mut vis))) => {
            vis.set_if_neq(Visibility::Hidden);
        }
        (None, None) => {}
    }
}

fn setup_highlight_gizmos(mut store: ResMut<GizmoConfigStore>) {
    let (config, _) = store.config_mut::<BlockHighlightGizmos>();
    config.line.width = HIGHLIGHT_LINE_PX;
    // Just in front of the block's own faces, which the box's edges lie on.
    config.depth_bias = -0.0005;
}

pub struct BlockTargetPlugin;

impl Plugin for BlockTargetPlugin {
    fn build(&self, app: &mut App) {
        app.init_gizmo_group::<BlockHighlightGizmos>()
            .init_resource::<TileMirror>()
            .init_resource::<CrackAssets>()
            .add_session_reset::<LookedAtBlock>()
            .add_session_reset::<LookedAtStud>()
            .add_session_reset::<CrackOverlay>()
            .add_systems(Startup, setup_highlight_gizmos)
            .add_systems(PostUpdate, (update_looked_at, draw_highlight).chain().after(bevy::transform::TransformSystems::Propagate))
            .add_systems(
                PostUpdate,
                (update_looked_at_stud, draw_stud_highlight).chain().after(bevy::transform::TransformSystems::Propagate),
            )
            .add_systems(Update, update_cracks);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_grow_the_same_cracks() {
        let order = crack_order();
        assert!(order.len() >= 40, "a visible network: {} texels", order.len());
        let mut prev = crack_stage_pixels(0);
        for stage in 1..CRACK_STAGES {
            let px = crack_stage_pixels(stage);
            // Every texel cracked at the previous stage stays cracked.
            for i in (3..px.len()).step_by(4) {
                assert!(px[i] >= prev[i], "stage {stage} un-cracked texel {}", i / 4);
            }
            assert_ne!(px, prev, "stage {stage} adds cracks");
            prev = px;
        }
    }

    #[test]
    fn cracks_are_drawn_on_voxel_blocks_only() {
        let region = Some(game::RegionCoords::new(0, 0));
        let mining = Some(game::MiningView { target: [3, 4, 5], progress_per_mille: 500 });
        let (at, stage) = crack_target(mining, region, game::TerrainKind::Voxel).expect("a damaged block");
        assert_eq!(at, Vec3::new(3.5, 4.5, 5.5) * BLOCK_VOXELS as f32);
        assert_eq!(stage, crack_stage(500));
        assert_eq!(crack_target(mining, region, game::TerrainKind::Stud), None, "no block-sized cube on bricks");
        let untouched = Some(game::MiningView { target: [3, 4, 5], progress_per_mille: 0 });
        assert_eq!(crack_target(untouched, region, game::TerrainKind::Voxel), None);
        assert_eq!(crack_target(None, region, game::TerrainKind::Voxel), None);
        assert_eq!(crack_target(mining, None, game::TerrainKind::Voxel), None);
    }

    #[test]
    fn the_stud_highlight_is_the_whole_hit_brick() {
        use game::{plate_units, StudBrick, StudChunk, WorldChunkPos, CHUNK_UNITS, STUD_CHUNK_HEIGHT_UNITS, STUD_UNITS};
        let chunk = StudChunk::new([StudBrick::filler([4, 3, 6], [2, 3, 4], 4)]).unwrap();
        let hit = game::StudWorldHit { chunk: WorldChunkPos::new(-1, 2, 3), cell: [5, 4, 8], normal: [0, 1, 0], t: 1.0 };
        let o = Vec3::new(-(CHUNK_UNITS as f32), 2.0 * STUD_CHUNK_HEIGHT_UNITS as f32, 3.0 * CHUNK_UNITS as f32);
        let s = STUD_UNITS as f32;
        let (lo, hi) = stud_hit_box(&hit, Some(&chunk));
        assert_eq!(lo, o + Vec3::new(4.0 * s, plate_units(3), 6.0 * s));
        assert_eq!(hi, o + Vec3::new(6.0 * s, plate_units(6), 10.0 * s));
        // No chunk content at hand: the hit cell.
        let (lo, hi) = stud_hit_box(&hit, None);
        assert!((hi - lo).abs_diff_eq(Vec3::new(s, plate_units(1), s), 1e-3));
    }

    #[test]
    fn stage_from_progress() {
        assert_eq!(crack_stage(0), 0);
        assert_eq!(crack_stage(99), 0);
        assert_eq!(crack_stage(100), 1);
        assert_eq!(crack_stage(999), 9);
        assert_eq!(crack_stage(1000), 9);
    }
}
