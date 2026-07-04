use bevy::prelude::*;

use super::bridge::SimKind;
use game::EntityKind;

/// Lazily-created shared avatar assets (one mesh + material per kind for now).
#[derive(Resource, Default)]
pub struct AvatarAssets(pub Option<(Handle<Mesh>, Handle<StandardMaterial>)>);

/// Attaches a renderable capsule to kind-bearing entities. Entities with an
/// active `Camera3d` are the local player — the first-person camera sits
/// inside the capsule, so the local player gets no mesh.
pub fn attach_avatars(
    mut commands: Commands,
    added: Query<(Entity, &SimKind), (Added<SimKind>, Without<Camera3d>)>,
    mut assets: ResMut<AvatarAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (e, kind) in &added {
        match kind.0 {
            EntityKind::Player => {
                let (mesh, material) = assets
                    .0
                    .get_or_insert_with(|| {
                        (
                            // Total height 1.8 m: mirrors the sim capsule
                            // capsule_y(0.5, 0.4) in create_player_safe.
                            meshes.add(Capsule3d::new(0.4, 1.0)),
                            materials.add(StandardMaterial {
                                base_color: Color::srgb(0.8, 0.3, 0.3),
                                ..Default::default()
                            }),
                        )
                    })
                    .clone();
                commands.entity(e).insert((Mesh3d(mesh), MeshMaterial3d(material)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<Mesh>();
        app.init_asset::<StandardMaterial>();
        app.init_resource::<AvatarAssets>();
        app.add_systems(Update, attach_avatars);
        app
    }

    #[test]
    fn remote_player_gets_capsule_mesh() {
        let mut app = test_app();
        let e = app.world_mut().spawn(SimKind(EntityKind::Player)).id();
        app.update();
        assert!(app.world().entity(e).contains::<Mesh3d>());
        assert!(app.world().entity(e).contains::<MeshMaterial3d<StandardMaterial>>());
    }

    #[test]
    fn local_player_with_camera_gets_no_mesh() {
        let mut app = test_app();
        let e = app
            .world_mut()
            .spawn((SimKind(EntityKind::Player), Camera3d::default()))
            .id();
        app.update();
        assert!(!app.world().entity(e).contains::<Mesh3d>());
    }

    #[test]
    fn mesh_and_material_handles_are_shared() {
        let mut app = test_app();
        let e1 = app.world_mut().spawn(SimKind(EntityKind::Player)).id();
        let e2 = app.world_mut().spawn(SimKind(EntityKind::Player)).id();
        app.update();
        let m1 = app.world().entity(e1).get::<Mesh3d>().unwrap().0.clone();
        let m2 = app.world().entity(e2).get::<Mesh3d>().unwrap().0.clone();
        assert_eq!(m1, m2);
    }
}
