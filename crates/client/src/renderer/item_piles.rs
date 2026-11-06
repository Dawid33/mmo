//! Dropped items (`game::item_piles`) drawn as little cubes.
//!
//! The sim keeps piles as cell-keyed [`PileView`]s and emits
//! `GameDataUpdateKind::SetItemPiles` both ways; the bridge forwards them
//! here as [`ItemPileMsg`]s (a region's snapshot piles too, and a region's
//! removal). One root entity per `(RegionId, BlockPos)` sits at the floor
//! centre of the cell in world space (region offset applied here, at the
//! render boundary) with `cubes_for(count)` cube children per stack, spread
//! over a fixed offset table. The bob and spin are client-only and never
//! touch the sim. Roots are hidden by the fortress cut like entities above
//! the working level (`commander::cut_entities::hidden_by_cut`).
//!
//! Textures: a block item draws its block's side texture on a shared cube
//! mesh; a non-block item draws a flat double-sided quad. There is no
//! `assets/items/` directory yet: `stick` reuses `oak_branch.png`
//! ([`extra_item_texture`]). Materials are one per `ItemId`, nearest-sampled; their
//! textures are decoded here into `Image` assets (`texture_for`), never
//! loaded through the asset server (no `png` feature in this Bevy build).

use std::collections::BTreeMap;

use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::prelude::*;
use game::edit::BlockPos;
use game::{BlockRegistry, ItemId, PileView, RegionId, BLOCK_VOXELS};

use super::commander::cut_entities::{home_rect, hidden_by_cut};
use super::commander::level::{camera_cut, WorkingLevel};
use super::commander::tiles::TileMirror;
use crate::session::{SessionResetExt, SessionScoped};

/// Edge of one item cube, metres.
pub const ITEM_CUBE_SIZE_M: f32 = 0.25;
/// Bob amplitude, metres (period π s: `sin(2t)`).
pub const ITEM_BOB_M: f32 = 0.05;
/// Spin, radians per second.
pub const ITEM_SPIN_RAD_PER_S: f32 = 1.0;
/// Items one drawn cube stands for.
pub const ITEMS_PER_CUBE: u32 = 16;
/// Most cubes drawn for one stack.
pub const CUBES_PER_STACK_MAX: u32 = 4;
/// Spacing of the offset table, metres.
const SPREAD_M: f32 = 0.3;

/// How many cubes a stack of `count` items is drawn with.
pub fn cubes_for(count: u32) -> u32 {
    count.div_ceil(ITEMS_PER_CUBE).min(CUBES_PER_STACK_MAX)
}

/// The fixed little offset table (x, z in cube spreads from the cell centre),
/// indexed by a pile's running cube number.
const OFFSETS: [[f32; 2]; 9] =
    [[0.0, 0.0], [1.0, 0.0], [-1.0, 0.0], [0.0, 1.0], [0.0, -1.0], [1.0, 1.0], [-1.0, -1.0], [1.0, -1.0], [-1.0, 1.0]];

/// Texture for a non-block item, relative to the assets root.
pub fn extra_item_texture(item: ItemId) -> String {
    match game::item_name(item) {
        "stick" => "blocks/oak_branch.png".to_string(),
        other => format!("items/{other}.png"),
    }
}

/// What the bridge tells the piles.
#[derive(Message, Debug, Clone)]
pub enum ItemPileMsg {
    /// Per cell: the new pile (`None` = gone), applied in order.
    Set { region: RegionId, changes: Vec<(BlockPos, Option<PileView>)> },
    /// The region was removed (or replaced): drop all its piles.
    RemoveRegion(RegionId),
}

/// The piles drawn now.
#[derive(Resource, Default)]
pub struct ItemPileRoots(pub BTreeMap<(RegionId, BlockPos), Entity>);

/// A pile's root.
#[derive(Component, Debug, Clone, Copy)]
pub struct ItemPileRoot {
    pub region: RegionId,
    pub cell: BlockPos,
}

/// One drawn cube (child of a root): its rest position and animation phase.
#[derive(Component, Debug, Clone, Copy)]
pub struct ItemCube {
    pub base: Vec3,
    pub phase: f32,
}

#[derive(Resource)]
struct ItemPileAssets {
    registry: BlockRegistry,
    cube: Handle<Mesh>,
    quad: Handle<Mesh>,
    materials: BTreeMap<ItemId, (Handle<StandardMaterial>, bool)>,
}

impl FromWorld for ItemPileAssets {
    fn from_world(world: &mut World) -> Self {
        let size = ITEM_CUBE_SIZE_M * game::movement::UNITS_PER_METER;
        let cube = world.resource_mut::<Assets<Mesh>>().add(Cuboid::new(size, size, size));
        let quad = world.resource_mut::<Assets<Mesh>>().add(Rectangle::new(size, size));
        let registry = BlockRegistry::from_ron(&crate::blocks::manifest_src()).expect("invalid block manifest");
        Self { registry, cube, quad, materials: BTreeMap::new() }
    }
}

/// The texture for `item` as an `Image` asset, decoded here from the block
/// PNG (`super::load_block_texture`: the file natively, the embedded copy on
/// wasm). Never through the asset server: Bevy is built without its `png`
/// feature, so a `load("blocks/x.png")` never arrives and a material waiting
/// on it is never drawn.
fn texture_for(registry: &BlockRegistry, images: &mut Assets<Image>, item: ItemId) -> Option<Handle<Image>> {
    let rgba = super::item_icons::texture_path(registry, item).and_then(|p| super::load_block_texture(&p))?;
    let (w, h) = rgba.dimensions();
    let mut image = Image::new(
        Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        TextureDimension::D2,
        rgba.into_raw(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::nearest();
    Some(images.add(image))
}

/// The material for `item` (made once) and whether it is a flat quad.
fn material_for(
    world_assets: &mut ItemPileAssets,
    images: &mut Assets<Image>,
    mats: &mut Assets<StandardMaterial>,
    item: ItemId,
) -> (Handle<StandardMaterial>, bool) {
    if let Some(m) = world_assets.materials.get(&item) {
        return m.clone();
    }
    let flat = item.as_block().is_none();
    let texture = texture_for(&world_assets.registry, images, item);
    let material = mats.add(StandardMaterial {
        base_color_texture: texture,
        perceptual_roughness: 1.0,
        reflectance: 0.0,
        double_sided: true,
        cull_mode: None,
        alpha_mode: if flat { AlphaMode::Mask(0.5) } else { AlphaMode::Opaque },
        ..default()
    });
    world_assets.materials.insert(item, (material.clone(), flat));
    (material, flat)
}

/// World position of the floor centre of `cell` in `region`.
pub fn pile_world_position(region: RegionId, cell: BlockPos) -> Vec3 {
    let o = region.world_offset();
    let b = BLOCK_VOXELS as f32;
    Vec3::new(o[0] + (cell[0] as f32 + 0.5) * b, o[1] + cell[1] as f32 * b, o[2] + (cell[2] as f32 + 0.5) * b)
}

fn spawn_pile(
    commands: &mut Commands,
    assets: &mut ItemPileAssets,
    images: &mut Assets<Image>,
    mats: &mut Assets<StandardMaterial>,
    region: RegionId,
    cell: BlockPos,
    view: &PileView,
) -> Entity {
    let unit = game::movement::UNITS_PER_METER;
    let half = ITEM_CUBE_SIZE_M * unit / 2.0;
    let mut n = 0usize;
    let mut children = Vec::new();
    for stack in &view.stacks {
        let (material, flat) = material_for(assets, images, mats, stack.item);
        for _ in 0..cubes_for(stack.count) {
            let [ox, oz] = OFFSETS[n % OFFSETS.len()];
            let base = Vec3::new(ox * SPREAD_M * unit, half, oz * SPREAD_M * unit);
            let mesh = if flat { assets.quad.clone() } else { assets.cube.clone() };
            children.push(
                commands
                    .spawn((
                        ItemCube { base, phase: n as f32 * 1.3 },
                        Mesh3d(mesh),
                        MeshMaterial3d(material.clone()),
                        Transform::from_translation(base),
                    ))
                    .id(),
            );
            n += 1;
        }
    }
    commands
        .spawn((
            ItemPileRoot { region, cell },
            SessionScoped,
            Transform::from_translation(pile_world_position(region, cell)),
            Visibility::default(),
        ))
        .add_children(&children)
        .id()
}

fn apply_pile_msgs(
    mut commands: Commands,
    mut msgs: MessageReader<ItemPileMsg>,
    mut roots: ResMut<ItemPileRoots>,
    mut assets: ResMut<ItemPileAssets>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    for msg in msgs.read() {
        match msg {
            ItemPileMsg::Set { region, changes } => {
                for (cell, view) in changes {
                    if let Some(old) = roots.0.remove(&(*region, *cell)) {
                        commands.entity(old).despawn();
                    }
                    if let Some(view) = view.as_ref().filter(|v| !v.stacks.is_empty()) {
                        let e = spawn_pile(&mut commands, &mut assets, &mut images, &mut mats, *region, *cell, view);
                        roots.0.insert((*region, *cell), e);
                    }
                }
            }
            ItemPileMsg::RemoveRegion(region) => {
                let keys: Vec<_> = roots.0.range((*region, [i32::MIN; 3])..=(*region, [i32::MAX; 3])).map(|(k, _)| *k).collect();
                for k in keys {
                    if let Some(e) = roots.0.remove(&k) {
                        commands.entity(e).despawn();
                    }
                }
            }
        }
    }
}

/// Bob and spin, client-only.
fn animate_cubes(time: Res<Time>, mut cubes: Query<(&ItemCube, &mut Transform)>) {
    let t = time.elapsed_secs();
    let bob = ITEM_BOB_M * game::movement::UNITS_PER_METER;
    for (cube, mut tf) in &mut cubes {
        tf.translation = cube.base + Vec3::Y * ((2.0 * t + cube.phase).sin() * bob);
        tf.rotation = Quat::from_rotation_y(t * ITEM_SPIN_RAD_PER_S + cube.phase);
    }
}

/// Piles at or above the working level inside the home region are hidden.
fn hide_cut_piles(
    level: Res<WorkingLevel>,
    tiles: Res<TileMirror>,
    mut roots: Query<(&ItemPileRoot, &mut Visibility)>,
) {
    let cut = camera_cut(&level, &tiles);
    let home = tiles.region().map(home_rect);
    for (root, mut vis) in &mut roots {
        let hide = hidden_by_cut(pile_world_position(root.region, root.cell), cut, home);
        vis.set_if_neq(if hide { Visibility::Hidden } else { Visibility::Inherited });
    }
}

pub struct ItemPilesPlugin;

impl Plugin for ItemPilesPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ItemPileMsg>()
            .add_session_message_clear::<ItemPileMsg>()
            .add_session_reset::<ItemPileRoots>()
            .init_resource::<ItemPileAssets>()
            .init_resource::<WorkingLevel>()
            .init_resource::<TileMirror>()
            .add_systems(Update, (apply_pile_msgs, animate_cubes, hide_cut_piles).chain());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::ItemStack;

    #[test]
    fn cubes_for_counts() {
        assert_eq!(cubes_for(1), 1);
        assert_eq!(cubes_for(16), 1);
        assert_eq!(cubes_for(17), 2);
        assert_eq!(cubes_for(64), 4);
        assert_eq!(cubes_for(1000), 4);
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin { file_path: "../../assets".into(), ..default() }, bevy::transform::TransformPlugin))
            .init_asset::<Mesh>()
            .init_asset::<Image>()
            .init_asset::<StandardMaterial>()
            .add_plugins(ItemPilesPlugin);
        app
    }

    fn dirt() -> ItemId {
        ItemId::block(game::BlockId(1))
    }

    fn view(count: u32) -> PileView {
        PileView { stacks: vec![ItemStack { item: dirt(), count }] }
    }

    fn roots(app: &mut App) -> Vec<(Entity, usize, Vec3)> {
        let mut q = app.world_mut().query::<(Entity, &ItemPileRoot, &Transform, Option<&Children>)>();
        q.iter(app.world()).map(|(e, _, t, c)| (e, c.map_or(0, |c| c.len()), t.translation)).collect()
    }

    /// Live report 2026-11-05: piles were never drawn. Bevy is built without
    /// its `png` feature, so a texture loaded through the asset server never
    /// arrives and its `StandardMaterial` is never drawn. Every cube's texture
    /// must already be an `Image` asset when the pile spawns.
    #[test]
    fn cube_textures_are_ready_at_spawn() {
        let mut app = app();
        let stick = ItemId::extra_named("stick").expect("stick item");
        let changes = vec![
            ([0, 0, 0], Some(view(5))),
            ([2, 0, 0], Some(PileView { stacks: vec![ItemStack { item: stick, count: 3 }] })),
        ];
        app.world_mut().write_message(ItemPileMsg::Set { region: RegionId::new(0, 0), changes });
        app.update();
        app.update();
        let mut q = app.world_mut().query::<&MeshMaterial3d<StandardMaterial>>();
        let handles: Vec<_> = q.iter(app.world()).map(|m| m.0.clone()).collect();
        assert!(!handles.is_empty(), "piles spawned cubes");
        let mats = app.world().resource::<Assets<StandardMaterial>>();
        let images = app.world().resource::<Assets<Image>>();
        for h in handles {
            let tex = mats.get(&h).and_then(|m| m.base_color_texture.clone()).expect("cube material has a texture");
            assert!(images.get(&tex).is_some(), "cube texture must be an Image asset at spawn, not a pending load");
        }
    }

    #[test]
    fn spawns_and_despawns() {
        let mut app = app();
        let region = RegionId::new(1, -1);
        let cell = [3, 5, 7];
        app.world_mut().write_message(ItemPileMsg::Set { region, changes: vec![(cell, Some(view(40)))] });
        app.update();
        app.update();
        let r = roots(&mut app);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].1, 3, "40 items = 3 cubes");
        assert_eq!(r[0].2, pile_world_position(region, cell));
        let o = region.world_offset();
        let b = BLOCK_VOXELS as f32;
        assert_eq!(r[0].2, Vec3::new(o[0] + 3.5 * b, 5.0 * b, o[2] + 7.5 * b));

        // A change replaces the pile.
        app.world_mut().write_message(ItemPileMsg::Set { region, changes: vec![(cell, Some(view(1)))] });
        app.update();
        app.update();
        let r = roots(&mut app);
        assert_eq!((r.len(), r[0].1), (1, 1));

        app.world_mut().write_message(ItemPileMsg::Set { region, changes: vec![(cell, None)] });
        app.update();
        app.update();
        assert!(roots(&mut app).is_empty());
    }

    #[test]
    fn region_removal_drops_its_piles_only() {
        let mut app = app();
        let (a, b) = (RegionId::new(0, 0), RegionId::new(1, 0));
        app.world_mut().write_message(ItemPileMsg::Set { region: a, changes: vec![([0, 0, 0], Some(view(1))), ([1, 0, 0], Some(view(1)))] });
        app.world_mut().write_message(ItemPileMsg::Set { region: b, changes: vec![([0, 0, 0], Some(view(1)))] });
        app.update();
        app.world_mut().write_message(ItemPileMsg::RemoveRegion(a));
        app.update();
        app.update();
        assert_eq!(roots(&mut app).len(), 1);
    }
}
