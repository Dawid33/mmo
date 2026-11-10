//! Stud build mode: pick the stud under the cursor, show a ghost brick where a
//! click would place one, left click places, right click removes.
//!
//! The cursor ray crosses region edges (`world::stud_world_raycast`: every
//! loaded region's chunks, else the stud halo), and a placement is sent as the
//! world-addressed `Order::PlaceStudAt`, which the home region applies itself or
//! hands to the neighbour that owns the chunk — so placing against, or into, a
//! neighbouring region works. Removal (`Order::RemoveStud`) is region-local in
//! the sim: a right click removes only a brick of the home region.
//!
//! While the mode is on the Slint build palette is shown (`palette.rs`,
//! `palette_ui.rs`, `ui/stud-build.slint`): the part list, the rotation, the
//! natural materials and the plastic colours, and the current selection. The
//! keys keep working beside it: `N` toggles, `R` rotates, `,` / `.` cycle the
//! part, `H` cycles a short list of colours. (`B`, `[`/`]`, `C` are the
//! designation Build tool, the top-down zoom and the cut, so they are not used.)

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use game::movement::UNITS_PER_METER;
use game::stud_catalog::STUD_PARTS;
use game::rapier3d::math::Vector;
use game::{plate_units, Order, RegionId, StudBrick, StudChunk, StudChunkCoords, StudShape, StudWorldHit, WorldChunkPos, STUD_UNITS};

use super::mesh::{body_mesh, stud_assets, stud_mesh, StudAssets, StudMeshData, StudNeighbours};
use super::systems::to_mesh;
use super::world::{stud_world_raycast, world_chunk_origin, StudWorld};
use super::SimStudChunk;
use crate::renderer::commander::designate::cursor_ray;
use crate::renderer::commander::minimap::{self, MinimapPointer};
use crate::renderer::commander::tiles::TileMirror;
use crate::renderer::commander::RightPress;
use crate::renderer::commander::CommanderView;
use crate::renderer::cursor::CursorPolicy;
use crate::renderer::{GameEvents, LocalPlayer};
use crate::session::SessionScoped;

pub const BUILD_TOGGLE_KEY: KeyCode = KeyCode::KeyN;
pub const BUILD_ROTATE_KEY: KeyCode = KeyCode::KeyR;
pub const BUILD_PART_PREV_KEY: KeyCode = KeyCode::Comma;
pub const BUILD_PART_NEXT_KEY: KeyCode = KeyCode::Period;
pub const BUILD_COLOR_KEY: KeyCode = KeyCode::KeyH;
/// LDraw colour codes the colour key cycles through.
pub const BUILD_COLORS: [u16; 8] = [15, 0, 4, 1, 14, 2, 71, 19];
/// How far the build ray reaches, metres.
pub const STUD_BUILD_REACH_M: f32 = 150.0;
/// A right press that travels farther than this (px of raw mouse motion) is a camera drag, not a removal.
const REMOVE_CLICK_TRAVEL_PX: f32 = 4.0;
const GHOST_ALPHA: f32 = 0.55;
const GHOST_INVALID_RGB: [f32; 3] = [1.0, 0.1, 0.1];

/// Build-mode state. `part` indexes `STUD_PARTS` (all of them, in catalog
/// order); `material` is the stud material a placed brick gets — a natural
/// one, or the plastic of an LDraw colour (`game::stud_material`).
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct StudBuild {
    pub on: bool,
    pub part: u16,
    pub rot: u8,
    pub material: u16,
}

impl Default for StudBuild {
    fn default() -> Self {
        Self { on: false, part: 0, rot: 0, material: BUILD_COLORS[0] }
    }
}

impl StudBuild {
    /// The colour key: the next of [`BUILD_COLORS`] after the current
    /// material (the first, when the material is not one of them).
    fn cycle_color(&mut self) {
        let next = BUILD_COLORS.iter().position(|&c| c == self.material).map_or(0, |i| (i + 1) % BUILD_COLORS.len());
        self.material = BUILD_COLORS[next];
    }

    fn cycle_part(&mut self, by: i32) {
        let n = STUD_PARTS.len() as i32;
        self.part = (self.part as i32 + by).rem_euclid(n.max(1)) as u16;
    }
}

/// Run condition for the designation / selection / move tools: build mode owns the clicks while on.
pub fn build_off(build: Res<StudBuild>) -> bool {
    !build.on
}

/// The ghost brick entity.
#[derive(Component)]
pub struct StudGhost;

/// What the cursor currently targets (only while build mode is on).
#[derive(Resource, Default, Debug, Clone, PartialEq)]
pub struct StudTarget {
    /// The stud cell under the cursor, in whichever region it lies.
    pub hit: Option<StudWorldHit>,
    /// The brick a left click places and the world chunk it goes into.
    pub place: Option<(WorldChunkPos, StudBrick)>,
    /// Whether the placement has room (and lies where the home region can reach).
    pub valid: bool,
    /// The home-region brick a right click removes (`None` for a hit in another region).
    pub remove: Option<(StudChunkCoords, [u8; 3])>,
}

impl StudTarget {
    /// The order a left click sends.
    pub fn place_order(&self) -> Option<Order> {
        let (chunk, brick) = self.place.filter(|_| self.valid)?;
        Some(Order::PlaceStudAt { chunk, brick })
    }

    /// The order a right click sends.
    pub fn remove_order(&self) -> Option<Order> {
        self.remove.map(|(chunk, cell)| Order::RemoveStud { chunk, cell })
    }
}

/// What the build ray (`origin` / `dir` in `home`'s local sim units) targets:
/// the hit, the placement against it and whether it fits. The placement's
/// chunk may be a neighbour's when it lies in the home region's halo band —
/// as far as the home region's sim sees (`Order::PlaceStudAt`'s reach).
pub fn build_target<'a>(
    build: &StudBuild,
    world: &'a StudWorld,
    chunk_of: impl Fn(Entity) -> Option<&'a StudChunk> + Copy,
    home: RegionId,
    origin: Vector,
    dir: Vector,
) -> Option<StudTarget> {
    let hit = stud_world_raycast(world, chunk_of, home, origin, dir, STUD_BUILD_REACH_M * UNITS_PER_METER)?;
    let place = hit.placement(build.part, build.rot, build.material);
    let valid = place.is_some_and(|(key, b)| {
        let local = key.local_in(home);
        let n = game::REGION_CHUNKS as i32;
        let own = (0..n).contains(&local[0]) && (0..n).contains(&local[2]);
        if !own && !game::halo::in_halo_band(local[0], local[2]) {
            return false;
        }
        let existing = match world.entity_at(key) {
            Some(e) => chunk_of(e),
            None => world.halo_chunk(key),
        };
        match existing {
            Some(c) => c.clone().place(b).is_ok(),
            // Not materialised: buried ground is full, anything else is air.
            None => !world.recipe(key.x, key.z).is_some_and(|g| key.y < *g.surface_layers().start()),
        }
    });
    let (region, c) = hit.chunk.to_local();
    let remove = (region == home).then_some((StudChunkCoords { x: c.x, y: c.y, z: c.z }, hit.cell));
    Some(StudTarget { hit: Some(hit), place, valid, remove })
}

pub fn build_keys(keys: Res<ButtonInput<KeyCode>>, policy: Res<CursorPolicy>, mut build: ResMut<StudBuild>) {
    if policy.blocks_game() {
        return;
    }
    if keys.just_pressed(BUILD_TOGGLE_KEY) {
        build.on = !build.on;
    }
    if !build.on {
        return;
    }
    if keys.just_pressed(BUILD_ROTATE_KEY) {
        build.rot = (build.rot + 1) & 3;
    }
    if keys.just_pressed(BUILD_PART_NEXT_KEY) {
        build.cycle_part(1);
    }
    if keys.just_pressed(BUILD_PART_PREV_KEY) {
        build.cycle_part(-1);
    }
    if keys.just_pressed(BUILD_COLOR_KEY) {
        build.cycle_color();
    }
}

/// The ghost mesh: the whole part (studs included), translucent, tinted red when invalid.
fn ghost_mesh(build: &StudBuild, valid: bool, assets: &StudAssets) -> Mesh {
    let brick = StudBrick { min: [0; 3], shape: StudShape::Part { part: build.part, rot: build.rot }, material: build.material, data: 0 };
    let mut data = StudMeshData::default();
    if let Ok(chunk) = StudChunk::new([brick]) {
        let free = |_: [i32; 3]| false;
        let n = StudNeighbours { get: &free };
        for d in [body_mesh(&chunk, &n, assets), stud_mesh(&chunk, &n, assets)] {
            let base = data.positions.len() as u32;
            data.positions.extend(d.positions);
            data.normals.extend(d.normals);
            data.colors.extend(d.colors);
            data.indices.extend(d.indices.into_iter().map(|i| i + base));
        }
    }
    for c in &mut data.colors {
        if !valid {
            c[..3].copy_from_slice(&GHOST_INVALID_RGB);
        }
        c[3] = GHOST_ALPHA;
    }
    to_mesh(data)
}

type GhostQuery<'w, 's> =
    Query<'w, 's, (Entity, &'static mut Transform, &'static mut Visibility), (With<StudGhost>, Without<CommanderView>)>;

/// Where the cursor points: fills [`StudTarget`] (empty unless build mode is on).
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn build_pick(
    build: Res<StudBuild>,
    window: Query<&Window, With<PrimaryWindow>>,
    view: Query<(&Camera, &Transform), With<CommanderView>>,
    policy: Res<CursorPolicy>,
    minimap_pointer: Res<MinimapPointer>,
    mirror: Res<TileMirror>,
    world: Res<StudWorld>,
    stud_chunks: Query<&SimStudChunk>,
    mut target: ResMut<StudTarget>,
) {
    let pick = (|| {
        if !build.on || policy.blocks_game() || policy.ui_pointer || minimap_pointer.dragging {
            return None;
        }
        let home = mirror.region()?;
        let window = window.single().ok()?;
        // The minimap is Bevy-drawn, so `ui_pointer` is false over it.
        if window.cursor_position().is_some_and(|c| minimap::screen_rect(window.size()).contains(c)) {
            return None;
        }
        let ray = cursor_ray(window, view.single().ok()?, home)?;
        build_target(&build, &world, |e| stud_chunks.get(e).ok().map(|c| &c.chunk), home, ray.0, ray.1)
    })();
    let next = pick.unwrap_or_default();
    if *target != next {
        *target = next;
    }
}

#[derive(Default)]
pub struct BuildLocal {
    built_key: Option<(u16, u8, u16, bool)>,
    material: Option<Handle<StandardMaterial>>,
}

/// Clicks → orders, and the ghost on the placement.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn build_apply(
    mut commands: Commands,
    build: Res<StudBuild>,
    buttons: Res<ButtonInput<MouseButton>>,
    press: Res<RightPress>,
    target: Res<StudTarget>,
    events: Res<GameEvents>,
    local: Res<LocalPlayer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut ghost: GhostQuery,
    mut st: crate::session::SessionLocal<BuildLocal>,
) {
    let st = st.get();
    // The commander's own press tracker (UI / focus / overlay aware).
    let right_click = press.released.is_some_and(|t| t <= REMOVE_CLICK_TRAVEL_PX);

    if target.hit.is_none() {
        for (_, _, mut v) in &mut ghost {
            v.set_if_neq(Visibility::Hidden);
        }
        return;
    }
    let (place, valid) = (target.place, target.valid);
    if let Some(client) = local.0 {
        let order = if buttons.just_pressed(MouseButton::Left) {
            target.place_order()
        } else if right_click {
            target.remove_order()
        } else {
            None
        };
        if let Some(order) = order {
            let _ = events.0.send(game::GameEventKind::Order(client, order));
        }
    }

    let Some((coords, brick)) = place else {
        for (_, _, mut v) in &mut ghost {
            v.set_if_neq(Visibility::Hidden);
        }
        return;
    };
    let key = (build.part, build.rot, build.material, valid);
    let mat = st
        .material
        .get_or_insert_with(|| {
            materials.add(StandardMaterial {
                base_color: Color::WHITE,
                alpha_mode: AlphaMode::Blend,
                perceptual_roughness: 0.35,
                ..default()
            })
        })
        .clone();
    let min = brick.min;
    let offset = Vec3::new((min[0] as usize * STUD_UNITS) as f32, plate_units(min[1] as i32), (min[2] as usize * STUD_UNITS) as f32);
    let tf = Transform::from_translation(world_chunk_origin(coords) + offset);
    let rebuild = st.built_key != Some(key);
    match ghost.iter_mut().next() {
        Some((e, mut t, mut v)) => {
            *t = tf;
            v.set_if_neq(Visibility::Inherited);
            if rebuild {
                commands.entity(e).insert((Mesh3d(meshes.add(ghost_mesh(&build, valid, stud_assets()))), MeshMaterial3d(mat)));
            }
        }
        None => {
            commands.spawn((
                StudGhost,
                SessionScoped,
                tf,
                Visibility::Inherited,
                Mesh3d(meshes.add(ghost_mesh(&build, valid, stud_assets()))),
                MeshMaterial3d(mat),
            ));
        }
    }
    st.built_key = Some(key);
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{RegionCoords, STUD_CHUNK_STUDS};

    fn e(n: u32) -> Entity {
        Entity::from_raw_u32(n).unwrap()
    }

    /// Home region (0, 0) and its east neighbour, each with a floor slab in the
    /// chunk at the shared edge; the neighbour's has a pillar on it.
    fn two_regions() -> (StudWorld, Vec<(Entity, StudChunk)>, WorldChunkPos, WorldChunkPos) {
        let n = game::REGION_CHUNKS as i64;
        let (a, b) = (WorldChunkPos::new(n - 1, 0, 0), WorldChunkPos::new(n, 0, 0));
        let floor = StudBrick::filler([0, 0, 0], [STUD_CHUNK_STUDS as u8, 3, STUD_CHUNK_STUDS as u8], 4);
        let home_chunk = StudChunk::new([floor]).unwrap();
        let east_chunk = StudChunk::new([floor, StudBrick::filler([2, 3, 8], [2, 9, 2], 4)]).unwrap();
        let mut w = StudWorld::default();
        w.set_chunk(e(1), a, &home_chunk);
        w.set_chunk(e(2), b, &east_chunk);
        (w, vec![(e(1), home_chunk), (e(2), east_chunk)], a, b)
    }

    fn build() -> StudBuild {
        let part = STUD_PARTS.iter().position(|p| p.number == "3005").unwrap() as u16;
        StudBuild { on: true, part, rot: 0, ..Default::default() }
    }

    #[test]
    fn the_colour_key_cycles_its_short_list_from_any_material() {
        let mut b = StudBuild::default();
        assert_eq!(b.material, BUILD_COLORS[0]);
        for expected in BUILD_COLORS.iter().cycle().skip(1).take(BUILD_COLORS.len() + 1) {
            b.cycle_color();
            assert_eq!(b.material, *expected);
        }
        // From a material the key does not know (picked in the palette): the first.
        b.material = game::MAT_STONE;
        b.cycle_color();
        assert_eq!(b.material, BUILD_COLORS[0]);
    }

    #[test]
    fn a_ray_across_the_region_edge_places_with_world_coordinates() {
        let (w, chunks, a, b) = two_regions();
        let chunk_of = |en: Entity| chunks.iter().find(|(x, _)| *x == en).map(|(_, c)| c);
        let home = RegionCoords::new(0, 0);
        let edge = game::REGION_SIZE as f32;
        // From over the home region, down and east onto the neighbour's floor, one
        // stud past the edge: the brick goes into the NEIGHBOUR's chunk.
        let origin = Vector::new(edge - 40.0, 60.0, 20.0);
        let aim = Vector::new(edge + 4.0, plate_units(3), 20.0);
        let t = build_target(&build(), &w, chunk_of, home, origin, aim - origin).expect("a hit");
        let hit = t.hit.unwrap();
        assert_eq!(hit.chunk, b, "the hit is in the east region's chunk");
        assert_eq!((hit.cell, hit.normal), ([0, 2, 2], [0, 1, 0]));
        assert!(t.valid);
        let Some(Order::PlaceStudAt { chunk, brick }) = t.place_order() else { panic!("a world-addressed placement") };
        assert_eq!(chunk, b);
        assert_eq!(chunk.to_local().0, RegionCoords::new(1, 0));
        assert_eq!(brick.min, [0, 3, 2]);
        assert_eq!(t.remove_order(), None, "removal stays region-local: nothing for a neighbour's brick");

        // Against the west face of the neighbour's pillar from the east side, low
        // enough to see it: the brick lands beside it, still in the neighbour.
        let origin = Vector::new(edge - 30.0, plate_units(6) + 1.0, 8.0 * STUD_UNITS as f32 + 4.0);
        let t = build_target(&build(), &w, chunk_of, home, origin, Vector::new(1.0, 0.0, 0.0)).expect("the pillar");
        let hit = t.hit.unwrap();
        assert_eq!((hit.chunk, hit.cell, hit.normal), (b, [2, 6, 8], [-1, 0, 0]));
        assert_eq!(t.place.map(|(c, br)| (c, br.min)), Some((b, [1, 6, 8])));

        // Aimed at the home floor: placed at home, and its brick can be removed.
        let origin = Vector::new(edge - 40.0, 60.0, 20.0);
        let t = build_target(&build(), &w, chunk_of, home, origin, Vector::new(0.0, -1.0, 0.0)).expect("the home floor");
        assert_eq!(t.hit.unwrap().chunk, a);
        assert!(matches!(t.place_order(), Some(Order::PlaceStudAt { chunk, .. }) if chunk == a));
        assert!(matches!(t.remove_order(), Some(Order::RemoveStud { chunk, .. }) if chunk.x == a.x as usize));
    }

    #[test]
    fn an_occupied_or_out_of_reach_placement_is_invalid() {
        let (mut w, mut chunks, _, b) = two_regions();
        let home = RegionCoords::new(0, 0);
        let edge = game::REGION_SIZE as f32;
        // Straight down on top of the pillar's base beside a taller neighbour brick
        // is fine; a 2 x 4 brick whose footprint runs into the pillar is not.
        let big = StudBuild { part: STUD_PARTS.iter().position(|p| p.number == "3001").unwrap() as u16, ..build() };
        let origin = Vector::new(edge + 1.5 * STUD_UNITS as f32, 80.0, 9.5 * STUD_UNITS as f32);
        {
            let chunk_of = |en: Entity| chunks.iter().find(|(x, _)| *x == en).map(|(_, c)| c);
            let t = build_target(&big, &w, chunk_of, home, origin, Vector::new(0.0, -1.0, 0.0)).expect("the floor");
            assert_eq!(t.hit.unwrap().chunk, b);
            assert!(!t.valid, "the footprint overlaps the pillar");
            assert_eq!(t.place_order(), None);
        }
        // A chunk two regions away is outside what the home region's sim sees.
        let far = WorldChunkPos::new(2 * game::REGION_CHUNKS as i64, 0, 0);
        let slab = StudChunk::new([StudBrick::filler([0, 0, 0], [32, 3, 32], 4)]).unwrap();
        w.set_chunk(e(3), far, &slab);
        chunks.push((e(3), slab));
        let chunk_of = |en: Entity| chunks.iter().find(|(x, _)| *x == en).map(|(_, c)| c);
        let origin = Vector::new(2.0 * edge + 20.0, 60.0, 20.0);
        let t = build_target(&build(), &w, chunk_of, home, origin, Vector::new(0.0, -1.0, 0.0));
        assert!(t.is_none_or(|t| !t.valid), "beyond the neighbours: not placeable from here");
    }
}
