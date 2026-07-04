use std::collections::BTreeMap;

use bevy::prelude::*;
use crossbeam::channel::Receiver;
use game::{ClientUpdateEvent, GameData, GameDataUpdate, GameDataUpdateKind, RegionId};
use rollback::{EntityKey, Voxel};

use super::convert::iso_to_transform;
use super::{ClientUpdates, LocalPlayer};

#[derive(Resource, Default)]
pub struct Regions(pub BTreeMap<RegionId, Receiver<GameDataUpdate>>);

#[derive(Resource, Default)]
pub struct RegionRoots(pub BTreeMap<RegionId, Entity>);

#[derive(Resource, Default)]
pub struct SimEntityMap(pub BTreeMap<(RegionId, EntityKey), Entity>);

#[derive(Component)]
pub struct SimEntity {
    pub region: RegionId,
    pub key: EntityKey,
}

/// Where the sim says this entity should be. `Transform` chases it (task 5).
/// Written by the bridge on every Do *and* Undo event: target writes are
/// last-write-wins, so a rollback/reapply burst drained in one frame
/// collapses to the final pose. That idempotence is the invariant that
/// makes the one-way bridge safe — do not switch to incremental deltas.
#[derive(Component)]
pub struct SimTarget {
    pub pos: Vec3,
    pub rot: Quat,
    pub smoothing: f32,
    pub pos_snap: f32,
    pub rot_snap: f32,
}

impl SimTarget {
    pub fn body(pos: Vec3, rot: Quat) -> Self {
        Self { pos, rot, smoothing: 0.5, pos_snap: 0.1, rot_snap: 0.1 }
    }
    pub fn camera(pos: Vec3, rot: Quat) -> Self {
        Self { pos, rot, smoothing: 0.1, pos_snap: 0.0005, rot_snap: 0.001 }
    }
}

#[derive(Component)]
pub struct VoxelData(pub Vec<Voxel>);

pub fn drain_client_updates(
    mut commands: Commands,
    updates: Res<ClientUpdates>,
    mut player: ResMut<LocalPlayer>,
    mut regions: ResMut<Regions>,
    mut roots: ResMut<RegionRoots>,
    mut map: ResMut<SimEntityMap>,
) {
    while let Ok(event) = updates.0.try_recv() {
        match event {
            ClientUpdateEvent::NewRegion(id, data, receiver) => {
                let root = commands
                    .spawn((Transform::IDENTITY, Visibility::default(), Name::new(format!("region {:?}", id))))
                    .id();
                roots.0.insert(id, root);
                regions.0.insert(id, receiver);
                spawn_region_snapshot(&mut commands, root, id, &data, &mut map);
                info!("bridge: region {:?} loaded", id);
            }
            ClientUpdateEvent::SetPlayer(client_id) => player.0 = Some(client_id),
            ClientUpdateEvent::GameCrash(e) => error!("bridge: game thread crashed: {:?}", e),
        }
    }
}

/// Port of the old `TrueWorld::new` snapshot walk.
fn spawn_region_snapshot(
    commands: &mut Commands,
    root: Entity,
    region: RegionId,
    data: &GameData,
    map: &mut SimEntityMap,
) {
    for (key, _) in data.ecs.entities.iter() {
        let mut e = commands.spawn((
            SimEntity { region, key },
            Transform::IDENTITY,
            Visibility::default(),
            ChildOf(root),
        ));
        if let Some(handle) = data.ecs.rigidbody.try_get(key) {
            if let Some(body) = data.physics.bodies.get(*handle) {
                let tf = iso_to_transform(body.position());
                e.insert((tf, SimTarget::body(tf.translation, tf.rotation)));
            }
        }
        if let Some(chunk) = data.ecs.chunk.try_get(key) {
            e.insert(VoxelData(chunk.voxels.clone()));
        }
        // camera components: added in task 6
        map.0.insert((region, key), e.id());
    }
}

pub fn drain_region_updates(
    mut commands: Commands,
    regions: Res<Regions>,
    roots: Res<RegionRoots>,
    mut map: ResMut<SimEntityMap>,
    mut targets: Query<&mut SimTarget>,
) {
    for (&region, receiver) in regions.0.iter() {
        let root = roots.0[&region];
        while let Ok(update) = receiver.try_recv() {
            // Do and Undo both applied: see SimTarget doc comment.
            match update.update_kind {
                GameDataUpdateKind::CreateEntity(key) => {
                    let e = commands
                        .spawn((
                            SimEntity { region, key },
                            Transform::IDENTITY,
                            Visibility::default(),
                            SimTarget::body(Vec3::ZERO, Quat::IDENTITY),
                            ChildOf(root),
                        ))
                        .id();
                    map.0.insert((region, key), e);
                }
                GameDataUpdateKind::RemoveEntity(key) => {
                    if let Some(e) = map.0.remove(&(region, key)) {
                        commands.entity(e).despawn();
                    }
                }
                GameDataUpdateKind::SetEntityPosition(key, iso) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: SetEntityPosition for unmapped {:?}", key);
                        continue;
                    };
                    let tf = iso_to_transform(&iso);
                    if let Ok(mut target) = targets.get_mut(e) {
                        target.pos = tf.translation;
                        target.rot = tf.rotation;
                    } else {
                        // Entity spawned via Commands earlier this same drain —
                        // not yet queryable. Insert (overwrites on apply).
                        commands.entity(e).insert(SimTarget::body(tf.translation, tf.rotation));
                    }
                }
                GameDataUpdateKind::SetVoxelComponent(key, Some(voxels)) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.entity(e).insert(VoxelData(voxels));
                    }
                }
                GameDataUpdateKind::SetVoxelComponent(key, None) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.entity(e).remove::<VoxelData>();
                    }
                }
                // Camera arms: task 6. Freecam: task 8.
                GameDataUpdateKind::AddCameraComponent(..)
                | GameDataUpdateKind::RemoveCameraComponent(..)
                | GameDataUpdateKind::UpdateCameraViewProj(..)
                | GameDataUpdateKind::UpdateCameraViewMatrix(..)
                | GameDataUpdateKind::SetFreeCam(..) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::{ClientUpdates, GameEvents, LocalPlayer};
    use game::{ChunkCoords, ClientUpdateEvent, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, Rollback};
    use rollback::EntityKey;
    use slotmapd::KeyData;

    fn test_app() -> (App, crossbeam::channel::Sender<ClientUpdateEvent>, crossbeam::channel::Sender<GameDataUpdate>, game::RegionId) {
        let (client_send, client_recv) = crossbeam::channel::unbounded();
        let (update_send, update_recv) = crossbeam::channel::unbounded();
        let (game_send, _game_recv) = crossbeam::channel::unbounded();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(ClientUpdates(client_recv))
            .insert_resource(GameEvents(game_send))
            .init_resource::<LocalPlayer>()
            .init_resource::<Regions>()
            .init_resource::<RegionRoots>()
            .init_resource::<SimEntityMap>()
            .add_systems(PreUpdate, (drain_client_updates, drain_region_updates).chain());

        let region_id = ChunkCoords::new(0, 0, 0);
        let rb = Rollback::new(None);
        client_send
            .send(ClientUpdateEvent::NewRegion(region_id, (*rb.data).clone(), update_recv))
            .unwrap();
        (app, client_send, update_send, region_id)
    }

    fn key(n: u64) -> EntityKey {
        EntityKey::from(KeyData::from_ffi((1 << 32) | n))
    }

    #[test]
    fn new_region_spawns_root() {
        let (mut app, _c, _u, region_id) = test_app();
        app.update();
        let roots = app.world().resource::<RegionRoots>();
        assert!(roots.0.contains_key(&region_id));
    }

    #[test]
    fn create_set_position_remove() {
        let (mut app, _c, updates, region_id) = test_app();
        app.update();

        let k = key(7);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetEntityPosition(k, rollback::IsometryReal::identity()))).unwrap();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).expect("entity mapped");
        assert!(app.world().entity(e).contains::<SimTarget>());

        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::RemoveEntity(k))).unwrap();
        app.update();
        assert!(app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).is_none());
        assert!(app.world().get_entity(e).is_err(), "despawned");
    }

    #[test]
    fn unknown_key_is_tolerated() {
        let (mut app, _c, updates, _region_id) = test_app();
        app.update();
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetEntityPosition(key(99), rollback::IsometryReal::identity()))).unwrap();
        app.update(); // must not panic
    }
}
