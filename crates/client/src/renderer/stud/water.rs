//! Recipe water on stud terrain. Water is recipe data, never
//! bricks: each stud column's `StudColumnGround::water_top` (plates) over its
//! `surface`. It is drawn as the water SURFACE only — one mesh per chunk column
//! holding water, a quad per rectangle of stud columns at one water level —
//! with the water material (`water::material`: blended, tinted by
//! vertex colour, cut with the working level).
//!
//! Static, like the sim's (`water.rs`): the recipe alone decides, so a hole dug
//! beside a lake stays dry and bricks built into water stand in it. No side
//! faces: a recipe's water lies in a basin (the column beside a water column
//! holds the same level or ground above it), so nothing here depends on chunk
//! edits and a region's water is built once, when its recipes arrive.
//!
//! Meshes are `SessionScoped` children of the region root, so they leave with
//! the region.

use std::collections::BTreeMap;
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use game::movement::UNITS_PER_METER;
use game::{plate_units, RegionId, StudColumnGround, CHUNK_UNITS, STUD_CHUNK_STUDS, STUD_UNITS};

use super::world::{StudGround, StudWorld};
use crate::renderer::bridge::RegionRoots;
use crate::renderer::water::material::{WaterMaterial, WaterMaterialHandle};
use crate::renderer::water::{water_alpha, WATER_COLOR};
use crate::session::SessionScoped;

/// Regions whose stud water is built per frame at most (each walks every stud
/// column of the region once).
pub const STUD_WATER_REGIONS_PER_FRAME: usize = 1;

/// One rectangle of stud columns under one water level: studs `x0..x1`, `z0..z1`
/// (chunk-local, exclusive ends), the surface at plate `top`, `depth` plates deep
/// on average.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterQuad {
    pub x0: u8,
    pub x1: u8,
    pub z0: u8,
    pub z1: u8,
    pub top: i32,
    pub depth: f32,
}

/// The water surface of one chunk column: the stud columns whose recipe holds
/// water over its ground, merged into rectangles of one level (runs along x,
/// equal runs joined along z).
pub fn stud_water_quads(g: &StudColumnGround) -> Vec<WaterQuad> {
    let n = STUD_CHUNK_STUDS;
    let level = |x: usize, z: usize| g.water_top(x, z).filter(|&top| top > g.surface(x, z));
    let mut out: Vec<WaterQuad> = Vec::new();
    // Quads still growing along z, by their index in `out`.
    let mut open: Vec<usize> = Vec::new();
    for z in 0..n {
        let mut row: Vec<WaterQuad> = Vec::new();
        let mut x = 0;
        while x < n {
            let Some(top) = level(x, z) else {
                x += 1;
                continue;
            };
            let start = x;
            let mut depth = 0.0;
            while x < n && level(x, z) == Some(top) {
                depth += (top - g.surface(x, z)) as f32;
                x += 1;
            }
            row.push(WaterQuad { x0: start as u8, x1: x as u8, z0: z as u8, z1: z as u8 + 1, top, depth: depth / (x - start) as f32 });
        }
        let mut next_open = Vec::new();
        for q in row {
            let grown = open.iter().copied().find(|&i| (out[i].x0, out[i].x1, out[i].top) == (q.x0, q.x1, q.top));
            match grown {
                Some(i) => {
                    let rows = (out[i].z1 - out[i].z0) as f32;
                    out[i].depth = (out[i].depth * rows + q.depth) / (rows + 1.0);
                    out[i].z1 = q.z1;
                    next_open.push(i);
                }
                None => {
                    out.push(q);
                    next_open.push(out.len() - 1);
                }
            }
        }
        open = next_open;
    }
    out
}

/// The quads as an upward-facing surface mesh in chunk-column-local sim units
/// (`y` in the region's plate frame), coloured by the water tint and depth.
pub fn stud_water_mesh(quads: &[WaterQuad]) -> Option<Mesh> {
    if quads.is_empty() {
        return None;
    }
    let s = STUD_UNITS as f32;
    let (mut positions, mut normals, mut colors, mut indices) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for q in quads {
        let y = plate_units(q.top);
        let (x0, x1, z0, z1) = (q.x0 as f32 * s, q.x1 as f32 * s, q.z0 as f32 * s, q.z1 as f32 * s);
        let alpha = water_alpha(plate_units(1) * q.depth / UNITS_PER_METER);
        let base = positions.len() as u32;
        // Counter-clockwise seen from above.
        for p in [[x0, y, z0], [x0, y, z1], [x1, y, z1], [x1, y, z0]] {
            positions.push(p);
            normals.push([0.0, 1.0, 0.0]);
            colors.push([WATER_COLOR[0], WATER_COLOR[1], WATER_COLOR[2], alpha]);
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    Some(
        Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
            .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
            .with_inserted_indices(Indices::U32(indices)),
    )
}

/// A stud water mesh entity (one chunk column's surface).
#[derive(Component, Clone, Copy, Debug)]
pub struct StudWater;

/// Which regions' stud water is built: the region root it hangs under and the
/// recipes it was built from. A new root (a re-sent snapshot) or new recipes
/// rebuild it.
#[derive(Resource, Default)]
pub struct StudWaterBuilt(BTreeMap<RegionId, (Entity, StudGround)>);

/// Build the water of regions whose recipes arrived (or changed).
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn build_stud_water(
    mut commands: Commands,
    world: Res<StudWorld>,
    roots: Option<Res<RegionRoots>>,
    mut built: ResMut<StudWaterBuilt>,
    mut handle: ResMut<WaterMaterialHandle>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<WaterMaterial>>>,
    fortress: Option<Res<crate::renderer::fortress_view::FortressViewUniform>>,
    mut epoch: crate::session::SessionLocal<Option<u64>>,
) {
    let (Some(roots), Some(mut meshes), Some(mut materials)) = (roots, meshes, materials) else { return };
    let epoch = epoch.get();
    // Nothing arrived or left since everything was built.
    if *epoch == Some(world.ground_epoch()) && !roots.is_changed() {
        return;
    }
    built.0.retain(|r, (root, _)| roots.0.get(r) == Some(root));
    let mut todo = 0;
    let mut done = 0;
    for (region, ground) in world.grounds() {
        let Some(&root) = roots.0.get(&region) else { continue };
        if built.0.get(&region).is_some_and(|(r, g)| *r == root && Arc::ptr_eq(g, ground)) {
            continue;
        }
        if done >= STUD_WATER_REGIONS_PER_FRAME {
            todo += 1;
            continue;
        }
        done += 1;
        // The old meshes (same root, new recipes) go first.
        if let Some((old_root, _)) = built.0.insert(region, (root, ground.clone())) {
            if old_root == root {
                commands.queue(move |world: &mut World| despawn_water_under(world, root));
            }
        }
        for (c, g) in ground.iter() {
            let Some(mesh) = stud_water_mesh(&stud_water_quads(g)) else { continue };
            let material = handle.get_or_add(&mut materials, fortress.as_deref().copied().unwrap_or_default());
            commands.spawn((
                StudWater,
                Name::new(format!("stud water {region:?} {c:?}")),
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material),
                Transform::from_xyz((c.x * CHUNK_UNITS) as f32, 0.0, (c.z * CHUNK_UNITS) as f32),
                Visibility::default(),
                NotShadowCaster,
                SessionScoped,
                ChildOf(root),
            ));
        }
    }
    if todo == 0 {
        *epoch = Some(world.ground_epoch());
    }
}

fn despawn_water_under(world: &mut World, root: Entity) {
    let Some(children) = world.get::<Children>(root) else { return };
    let water: Vec<Entity> = children.iter().filter(|&c| world.get::<StudWater>(c).is_some()).collect();
    for e in water {
        if let Ok(ec) = world.get_entity_mut(e) {
            ec.despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{StudProfile, MAT_DIRT, MAT_GRASS, MAT_STONE, STUD_COLUMN_FOOTPRINT, STUD_NO_WATER};

    const N: usize = STUD_CHUNK_STUDS;

    fn ground(surface: impl Fn(usize, usize) -> i32, water: impl Fn(usize, usize) -> i32) -> StudColumnGround {
        let at = |f: &dyn Fn(usize, usize) -> i32| (0..STUD_COLUMN_FOOTPRINT).map(|i| f(i % N, i / N)).collect::<Vec<_>>().into();
        StudColumnGround::new(
            at(&surface),
            at(&water),
            vec![0; STUD_COLUMN_FOOTPRINT].into(),
            vec![StudProfile { cover: MAT_GRASS, cover_plates: 1, soil: MAT_DIRT, soil_plates: 3 }],
            vec![0; STUD_COLUMN_FOOTPRINT].into(),
            vec![(i32::MAX, MAT_STONE)],
            -100,
        )
        .unwrap()
    }

    #[test]
    fn quads_only_where_water_stands_over_the_ground() {
        // A basin: ground at 10 for x < 8, else 30; sea level 20 everywhere.
        let g = ground(|x, _| if x < 8 { 10 } else { 30 }, |_, _| 20);
        let quads = stud_water_quads(&g);
        assert_eq!(quads, vec![WaterQuad { x0: 0, x1: 8, z0: 0, z1: N as u8, top: 20, depth: 10.0 }], "one merged rectangle");
        let mesh = stud_water_mesh(&quads).unwrap();
        let positions = mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap().as_float3().unwrap();
        assert_eq!(positions.len(), 4);
        assert!(positions.iter().all(|p| p[1] == plate_units(20)), "at the water plane");
        let max_x = positions.iter().map(|p| p[0]).fold(f32::MIN, f32::max);
        assert_eq!(max_x, (8 * STUD_UNITS) as f32, "nothing over the dry bank");

        // Dry: no water set, or water not above the ground.
        assert!(stud_water_quads(&ground(|_, _| 10, |_, _| STUD_NO_WATER)).is_empty());
        assert!(stud_water_quads(&ground(|_, _| 10, |_, _| 10)).is_empty());
        assert!(stud_water_mesh(&[]).is_none());
    }

    #[test]
    fn levels_and_holes_split_the_surface() {
        // Two ponds at different levels and an island column in the first.
        let g = ground(
            |x, z| if (x, z) == (2, 2) { 25 } else { 10 },
            |x, _| if x < 4 { 20 } else if x >= 28 { 14 } else { STUD_NO_WATER },
        );
        let quads = stud_water_quads(&g);
        let covered = |x: usize, z: usize| quads.iter().filter(|q| (q.x0 as usize..q.x1 as usize).contains(&x) && (q.z0 as usize..q.z1 as usize).contains(&z)).count();
        for z in 0..N {
            for x in 0..N {
                let wet = (x < 4 || x >= 28) && (x, z) != (2, 2);
                assert_eq!(covered(x, z), usize::from(wet), "column ({x}, {z})");
            }
        }
        assert!(quads.iter().any(|q| q.top == 14 && (q.x0, q.x1, q.z0, q.z1) == (28, 32, 0, N as u8)));
        assert!(quads.iter().all(|q| q.top == 14 || q.top == 20));
        assert!(quads.len() <= 6, "merged: {}", quads.len());
    }
}
