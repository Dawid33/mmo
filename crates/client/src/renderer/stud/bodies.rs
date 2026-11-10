//! Stud bodies (`game::stud_body`: bricks detached from stud terrain into one
//! falling, tumbling rigid body — a felled tree) drawn with the chunk mesher.
//!
//! The body is an ordinary sim entity (`SimEntity`, `Transform`,
//! `SimTarget::body`), so `interpolate_transforms` glides it between sim poses,
//! rotation included, and `RemoveEntity` despawns it with its children. The bridge tags it [`PendingStudBody`] (`AddStudBody`,
//! or a snapshot holding one); its model is meshed off the main thread
//! ([`StudMeshJob::Model`]: every brick whole, nothing culled) into two mesh
//! children — body and stud tops (and glass, if it holds any) — offset by minus the model's centre of mass,
//! where the body's pose sits (`AddStudBody`'s contract).

use std::sync::Arc;

use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, Task};
use game::stud_body::StudModel;

use super::mesh::StudMeshJob;
use super::material::{StudMaterials, StudTerrainMaterial};
use super::systems::{to_meshes, StudMeshes};

/// A body entity whose model still has to be meshed. A newer one replaces it.
#[derive(Component, Clone)]
pub struct PendingStudBody(pub Arc<StudModel>);

/// Marks a body entity drawn by its stud model (set with every
/// [`PendingStudBody`], never removed): `avatar` draws such an
/// `EntityKind::Body` no placeholder box.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct StudBodyModel;

/// The body's in-flight mesh build. Replacing it cancels the older build.
#[derive(Component)]
pub struct StudBodyMeshing(Task<StudMeshes>);

/// A mesh child of a stud body.
#[derive(Component, Clone, Copy, Debug)]
pub struct StudBodyPart;

/// Start meshing every pending body's model on the compute pool.
pub fn spawn_stud_body_meshes(mut commands: Commands, pending: Query<(Entity, &PendingStudBody)>) {
    for (e, PendingStudBody(model)) in &pending {
        let task = crate::renderer::compute::spawn(StudMeshJob::Model(StudModel::clone(model)), to_meshes);
        commands.entity(e).remove::<PendingStudBody>().insert(StudBodyMeshing(task));
    }
}

/// Land finished body meshes: replace the body's mesh children.
pub fn apply_stud_body_meshes(
    mut commands: Commands,
    mut tasks: Query<(Entity, &mut StudBodyMeshing, Option<&Children>)>,
    parts: Query<(), With<StudBodyPart>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StudTerrainMaterial>>,
    mut material: ResMut<StudMaterials>,
) {
    for (e, mut task, children) in &mut tasks {
        let Some(StudMeshes { body, top, glass, offset }) = block_on(future::poll_once(&mut task.0)) else { continue };
        for &child in children.into_iter().flatten() {
            if parts.contains(child) {
                commands.entity(child).despawn();
            }
        }
        commands.entity(e).remove::<StudBodyMeshing>();
        // The terrain's materials: a falling body is cut and darkened like the
        // ground it came from (the cut applies by world position).
        let opaque = material.opaque(&mut materials);
        for mesh in [body, top].into_iter().flatten() {
            commands.spawn((
                StudBodyPart,
                Transform::from_translation(offset),
                Visibility::default(),
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(opaque.clone()),
                ChildOf(e),
            ));
        }
        if let Some(mesh) = glass {
            commands.spawn((
                StudBodyPart,
                Transform::from_translation(offset),
                Visibility::default(),
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material.glass(&mut materials)),
                bevy::light::NotShadowCaster,
                ChildOf(e),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::stud::mesh::mesh_model;
    use game::stud_body::StudModelBrick;
    use game::{plate_units, StudShape, STUD_UNITS};

    fn model() -> StudModel {
        let filler = |min: [i16; 3], size| StudModelBrick { min, shape: StudShape::Filler { size }, material: 4, data: 0 };
        // Two stacked boxes, the upper past a chunk's 32-stud width from the lower's
        // origin: a model is not bound to the chunk grid.
        StudModel { bricks: vec![filler([0, 0, 0], [2, 3, 2]), filler([40, 3, 0], [2, 3, 2])] }
    }

    #[test]
    fn a_model_is_meshed_whole_about_its_centre_of_mass() {
        let m = model();
        let out = mesh_model(&m);
        assert_eq!(out.body.indices.len() / 3, 2 * 12, "every face of both boxes, nothing culled");
        assert!(out.top.is_empty(), "fillers carry no studs");
        let com = game::stud_body::stud_model_mass_properties(&m).local_com;
        assert_eq!(out.offset, [-com.x, -com.y, -com.z]);
        // Model cell (40, 3, 0)'s corner is a vertex, in model units.
        let corner = [(40 * STUD_UNITS) as f32, plate_units(3), 0.0];
        assert!(out.body.positions.contains(&corner), "{corner:?}");
        // Equal masses: the offset puts the midpoint of the two boxes on the entity.
        let mid = [(21 * STUD_UNITS) as f32, plate_units(3), STUD_UNITS as f32];
        for a in 0..3 {
            assert!((out.offset[a] + mid[a]).abs() < 1e-3, "axis {a}: {:?}", out.offset);
        }
    }

    #[test]
    fn a_part_in_a_model_keeps_its_studs() {
        let part = game::stud_catalog::STUD_PARTS.iter().position(|p| p.number == "3001").unwrap() as u16;
        let brick = |min| StudModelBrick { min, shape: StudShape::Part { part, rot: 0 }, material: 4, data: 0 };
        let one = mesh_model(&StudModel { bricks: vec![brick([0, 0, 0])] });
        assert!(!one.top.is_empty());
        // Stacked: unculled on purpose, so twice the studs and twice the faces.
        let two = mesh_model(&StudModel { bricks: vec![brick([0, 0, 0]), brick([0, 3, 0])] });
        assert_eq!(two.top.indices.len(), 2 * one.top.indices.len());
        assert_eq!(two.body.indices.len(), 2 * one.body.indices.len());
    }
}
