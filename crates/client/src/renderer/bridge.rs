use std::collections::BTreeMap;
use std::sync::Arc;

use bevy::prelude::*;
use crossbeam::channel::Receiver;
use game::{ClientUpdateEvent, EntityKey, GameData, GameDataUpdate, GameDataUpdateKind, RegionId};
use super::history_view::HistoryMsg;

use super::convert::{iso_to_transform, perspective_to_projection};
use super::commander::tiles::TileMirrorMsg;
use super::hud::HudStatus;
use super::lod_rings;
use super::{ClientUpdates, LocalPlayer};
use crate::session::SessionScoped;

#[derive(Resource, Default)]
pub struct Regions(pub BTreeMap<RegionId, Receiver<GameDataUpdate>>);

/// Each region's terrain root: the parent of its stud chunks and
/// its other static render entities, at the region's fixed world offset. Its
/// moving entities (players, slimes, falling bodies, …) hang off a separate
/// BODIES root at the same offset ([`RegionRoots::bodies`]): Bevy's static
/// transform optimisation re-walks a whole tree when any node in it changes
/// (a dirty root's `GlobalTransform` is rewritten, which flags every direct
/// child), so one hopping slime under the terrain root cost a ~345k-brick
/// walk every frame — ~15 ms/frame at a 512 m region (2026-10-31 trace).
/// The bodies root is despawned with its region root ([`BodiesOf`]).
#[derive(Resource, Default)]
pub struct RegionRoots(pub BTreeMap<RegionId, Entity>, BodyRoots);

/// The bodies roots behind [`RegionRoots::bodies`].
#[derive(Default)]
pub struct BodyRoots(BTreeMap<RegionId, Entity>);

impl RegionRoots {
    /// The parent of `region`'s non-terrain entities: its bodies root, or
    /// the region root itself for a root mapped without one (tests that
    /// insert into `.0` directly).
    pub fn bodies(&self, region: RegionId) -> Option<Entity> {
        self.1 .0.get(&region).or_else(|| self.0.get(&region)).copied()
    }

    fn insert(&mut self, region: RegionId, root: Entity, bodies: Entity) {
        self.0.insert(region, root);
        self.1 .0.insert(region, bodies);
    }

    fn remove(&mut self, region: &RegionId) -> Option<Entity> {
        self.1 .0.remove(region);
        self.0.remove(region)
    }
}

/// On a region's bodies root: the terrain root it belongs to. Despawning the
/// region root despawns its bodies root and everything under it.
#[derive(Component)]
#[relationship(relationship_target = RegionBodies)]
pub struct BodiesOf(pub Entity);

/// On a region root: its bodies root (see [`BodiesOf`]).
#[derive(Component, Default)]
#[relationship_target(relationship = BodiesOf, linked_spawn)]
pub struct RegionBodies(Vec<Entity>);

/// The region root and bodies root of a newly received region, both at its
/// fixed world offset (they never move).
fn spawn_region_roots(commands: &mut Commands, id: RegionId) -> (Entity, Entity) {
    let offset = id.world_offset();
    let at = Transform::from_translation(Vec3::new(offset[0], offset[1], offset[2]));
    let root = commands
        .spawn((at, Visibility::default(), Name::new(format!("region {:?}", id)), SessionScoped))
        .id();
    // Scoped too, though it goes with its region root: nothing of a
    // session may outlive it, whatever the relationship does.
    let bodies = commands
        .spawn((
            at,
            Visibility::default(),
            Name::new(format!("region {:?} bodies", id)),
            BodiesOf(root),
            SessionScoped,
        ))
        .id();
    (root, bodies)
}

/// Region roots released from the window but not yet despawned, oldest first.
/// A window shift releases a whole row at once — five regions of ~20k chunk
/// entities at the 5x5 window — and despawning all of them in the crossing
/// frame cost up to 372 ms of command application (2026-09-23 `fly-probe`
/// trace). [`despawn_retired_regions`] takes [`REGION_DESPAWNS_PER_FRAME`] a
/// frame instead. A retired root is already out of every map, so nothing
/// reads or updates it; it just stays drawn for those few frames.
#[derive(Resource, Default)]
pub struct RetiredRegionRoots(pub std::collections::VecDeque<(RegionId, Entity)>);

/// Region trees despawned per frame by [`despawn_retired_regions`].
pub const REGION_DESPAWNS_PER_FRAME: usize = 1;

pub fn despawn_retired_regions(mut commands: Commands, mut retired: ResMut<RetiredRegionRoots>) {
    for _ in 0..REGION_DESPAWNS_PER_FRAME {
        let Some((_, root)) = retired.0.pop_front() else { break };
        // despawn() removes descendants via ChildOf relationships.
        commands.entity(root).despawn();
    }
}

#[derive(Resource, Default)]
pub struct SimEntityMap(pub BTreeMap<(RegionId, EntityKey), Entity>);

// identity breadcrumbs for debugging; not yet read
#[allow(dead_code)]
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
/// Per-frame lerp factor for the camera's *position*. Kept < 1.0 so the
/// eye glides between 50Hz sim poses instead of stepping. Rotation is a
/// separate factor (see `camera`) held at 1.0 so the look stays instant.
pub const CAMERA_POS_SMOOTHING: f32 = 0.3;

#[derive(Component)]
pub struct SimTarget {
    pub pos: Vec3,
    pub rot: Quat,
    pub pos_smoothing: f32,
    pub rot_smoothing: f32,
    pub pos_snap: f32,
    pub rot_snap: f32,
    /// Added to `pos` to get where the `Transform` settles. Zero for bodies;
    /// the eye height for the local player's camera, whose sim pose is the
    /// capsule centre.
    pub offset: Vec3,
}

impl SimTarget {
    // pos_snap values are world-unit distances (1 unit = 1/16 m); smoothing
    // factors and rotation snaps are dimensionless/radians.
    pub fn body(pos: Vec3, rot: Quat) -> Self {
        Self {
            pos,
            rot,
            pos_smoothing: 0.5,
            rot_smoothing: 0.5,
            pos_snap: 1.6,
            rot_snap: 0.1,
            offset: Vec3::ZERO,
        }
    }
    pub fn camera(pos: Vec3, rot: Quat) -> Self {
        Self::camera_eyed(pos, rot, game::movement::PLAYER_EYE_OFFSET)
    }

    /// [`Self::camera`] with the eye `eye` above the body origin (the region's
    /// `BodyScale::eye_offset`).
    pub fn camera_eyed(pos: Vec3, rot: Quat, eye: f32) -> Self {
        // The render camera SNAPS its rotation (rot_smoothing = 1.0) so
        // there's no slerp-arc tilt during turns, but LERPS its position so
        // movement glides instead of stepping. The snapped per-tick sim look
        // is then overwritten by the per-frame local look (`look.rs`).
        Self {
            pos,
            rot,
            pos_smoothing: CAMERA_POS_SMOOTHING,
            rot_smoothing: 1.0,
            pos_snap: 0.008,
            rot_snap: 0.001,
            offset: Vec3::new(0.0, eye, 0.0),
        }
    }

    /// Where the `Transform` converges: the sim pose plus `offset`.
    pub fn goal(&self) -> Vec3 {
        self.pos + self.offset
    }
}

/// The first-person eye sits this far above the player's body origin.
pub const EYE_OFFSET: Vec3 = Vec3::new(0.0, game::movement::PLAYER_EYE_OFFSET, 0.0);

/// Marker: the local player's sim body. In commander mode it is hidden and
/// gets no camera (the commander has no body to look through or show).
#[derive(Component)]
pub struct LocalBody;

/// True when the game renders from the commander camera, not the body's.
fn commander(mode: Option<&super::commander::CameraMode>) -> bool {
    mode.is_some_and(|m| *m == super::commander::CameraMode::Commander)
}

/// Mirror of the sim's `EntityKind` for this entity. Consumed by the avatar
/// system to attach a renderable mesh.
#[derive(Component, Clone, Copy)]
pub struct SimKind(pub game::EntityKind);

/// Marks a bevy entity as an entity-halo mirror (`game::halo`) of the owned
/// entity `id` in region `source`. Used only for render dedupe
/// ([`dedupe_halo_entities`]): one copy per id is drawn.
#[derive(Component, Clone, Copy)]
pub struct HaloMirror {
    pub id: game::EntityId,
    pub source: RegionId,
}

/// The text a lost session shows on the main menu.
pub fn session_end_reason(error: &game::GameError) -> String {
    match error {
        game::GameError::Disconnected(reason) => reason.clone(),
        game::GameError::CrashedOnServerEvent => "the game crashed on a server event".into(),
        game::GameError::QuitRequested => "the session was quit".into(),
    }
}

pub fn drain_client_updates(
    mut commands: Commands,
    updates: Res<ClientUpdates>,
    mut player: ResMut<LocalPlayer>,
    mut regions: ResMut<Regions>,
    mut roots: ResMut<RegionRoots>,
    mut map: ResMut<SimEntityMap>,
    // Paired to stay within Bevy's 16 system parameters.
    (mut hud_status, mut tick_times): (ResMut<HudStatus>, Option<ResMut<super::hud::RegionTickTimes>>),
    mut ui_updates: ResMut<crate::renderer::slint_overlay::UiUpdates>,
    mut debug_lines: ResMut<crate::renderer::collider_debug::DebugColliderLines>,
    mut lod_rings: ResMut<lod_rings::LodRings>,
    mut retired_roots: ResMut<RetiredRegionRoots>,
    camera_mode: Option<Res<super::commander::CameraMode>>,
    mut tile_msgs: Option<ResMut<Messages<TileMirrorMsg>>>,
    mut history_out: super::history_view::HistoryOut,
    mut session_ended: Option<ResMut<Messages<crate::session::SessionEnded>>>,
) {
    // Unbudgeted: drains everything the game thread queued since last frame.
    let mut stall = game::perf::StallGuard::new("drain_client_updates");
    // One region tree spawned per frame (~20k entities at the 5x5 window):
    // a slow frame that collected several snapshots spent up to 233 ms here
    // (2026-09-23 `fly-probe` trace). Later events wait, in order.
    let mut spawned_region = false;
    while !spawned_region {
        let Ok(event) = updates.0.try_recv() else { break };
        stall.add_work(1);
        match event {
            ClientUpdateEvent::NewRegion(id, data, receiver) => {
                spawned_region = true;
                // Back before its old tree was despawned: finish that first,
                // so two trees of one region never share the stud index.
                retired_roots.0.retain(|&(rc, root)| {
                    if rc == id {
                        commands.entity(root).despawn();
                    }
                    rc != id
                });
                if roots.0.contains_key(&id) {
                    // Replace-on-re-receipt: resubscribe/crash-respawn resync.
                    remove_region(&mut commands, &mut regions, &mut roots, &mut map, id);
                }
                let (root, bodies) = spawn_region_roots(&mut commands, id);
                roots.insert(id, root, bodies);
                regions.0.insert(id, receiver);
                // The snapshot's item piles and halo pile mirrors (cells in
                // the region's frame), ahead of the region's own updates.
                let frame = |m: &std::collections::BTreeMap<game::WorldBlockPos, game::ItemPile>| -> Vec<_> {
                    m.iter().map(|(p, pile)| (game::local_cell(id, *p), Some(pile.view()))).collect()
                };
                let (piles, mirrors) = (frame(data.item_piles()), frame(data.halo_piles()));
                // The stud mirror: terrain kind, stud recipes and the stud halo.
                let stud_msg = super::stud::world::StudWorldMsg::of_snapshot(id, &data);
                commands.queue(move |world: &mut World| {
                    if let Some(mut msgs) = world.get_resource_mut::<Messages<super::stud::world::StudWorldMsg>>() {
                        msgs.write(stud_msg);
                    }
                    if let Some(mut msgs) = world.get_resource_mut::<Messages<super::item_piles::ItemPileMsg>>() {
                        msgs.write(super::item_piles::ItemPileMsg::RemoveRegion(id));
                        if !piles.is_empty() {
                            msgs.write(super::item_piles::ItemPileMsg::Set { region: id, changes: piles });
                        }
                        if !mirrors.is_empty() {
                            msgs.write(super::item_piles::ItemPileMsg::SetMirrors { region: id, changes: mirrors });
                        }
                    }
                });
                // The fortress tools' tile mirror (commander only).
                if let Some(msgs) = tile_msgs.as_mut() {
                    msgs.write(TileMirrorMsg::Reset {
                        region: id,
                        designations: data.designations().iter().map(|(p, d)| (*p, *d)).collect(),
                        constructed: data.constructed().keys().copied().collect(),
                    });
                }
                let spawn = game::perf::Stopwatch::start();
                spawn_region_snapshot(&mut commands, root, bodies, id, &data, &mut map, player.0, commander(camera_mode.as_deref()));
                // Main-thread cost of one region: an entity per sim entity
                // (the stud chunks are meshed off-thread, `stud::systems`).
                game::perf_event!(
                    "bridge_region_spawned",
                    "region={},{} spawn_ms={} entities={} chunks={} regions={}",
                    id.x,
                    id.z,
                    game::perf::ms(spawn.elapsed()),
                    data.ecs.entities.len(),
                    data.ecs.stud.iter().filter(|(_, chunk)| chunk.is_some()).count(),
                    roots.0.len()
                );
                info!("bridge: region {:?} loaded", id);
            }
            ClientUpdateEvent::SetPlayer(client_id) => player.0 = Some(client_id),
            ClientUpdateEvent::RemoveRegion(id) => {
                // Unmapped now, despawned over the next frames.
                if let Some(root) = unmap_region(&mut regions, &mut roots, &mut map, id) {
                    retired_roots.0.push_back((id, root));
                }
                debug_lines.0.remove(&id);
                commands.queue(move |world: &mut World| {
                    if let Some(mut msgs) = world.get_resource_mut::<Messages<super::item_piles::ItemPileMsg>>() {
                        msgs.write(super::item_piles::ItemPileMsg::RemoveRegion(id));
                    }
                });
                info!("bridge: region {:?} removed", id);
            }
            ClientUpdateEvent::StudLodData(key, tile) => {
                lod_rings.pending.push(lod_rings::LodEventMsg::StudData(key, tile));
            }
            ClientUpdateEvent::RemoveLod { key, near_takeover } => {
                lod_rings
                    .pending
                    .push(lod_rings::LodEventMsg::Remove { key, near_takeover });
            }
            ClientUpdateEvent::GameCrash(e) => {
                // The session ended without our Quit (lost server, failed
                // connect, crash): the session flow takes us back to the menu.
                error!("bridge: game session ended: {:?}", e);
                if let Some(ended) = session_ended.as_mut() {
                    ended.write(crate::session::SessionEnded { reason: Some(session_end_reason(&e)) });
                }
            }
            ClientUpdateEvent::HudStatus { home_region, viewer_region, connection } => {
                hud_status.home_region = Some(home_region);
                hud_status.viewer_region = Some(viewer_region);
                hud_status.connection = connection;
            }
            ClientUpdateEvent::UiChannel(recv) => {
                ui_updates.0 = Some(recv);
            }
            ClientUpdateEvent::DebugLines(id, lines) => {
                debug_lines.0.insert(id, lines);
            }
            ClientUpdateEvent::WorldMap(world_map) => {
                if let Some(msgs) = history_out.world_map.as_mut() {
                    msgs.write(super::world_map::WorldMapMsg::Map(world_map));
                }
            }
            ClientUpdateEvent::PlayRefused(rc, why) => {
                if let Some(msgs) = history_out.world_map.as_mut() {
                    msgs.write(super::world_map::WorldMapMsg::Refused(rc, why));
                }
            }
            ClientUpdateEvent::Sites(v) => history_out.write(HistoryMsg::Sites(v)),
            ClientUpdateEvent::Calendar(c) => history_out.write(HistoryMsg::Calendar(c)),
            ClientUpdateEvent::Fortress(f) => history_out.write(HistoryMsg::Fortress(f)),
            ClientUpdateEvent::RegionTickTimes(stats) => {
                if let Some(times) = tick_times.as_mut() {
                    times.0 = stats;
                }
            }
            ClientUpdateEvent::Notice(n) => history_out.write(HistoryMsg::Notice(n)),
            ClientUpdateEvent::EmbarkOptions(o) => {
                history_out.write(HistoryMsg::EmbarkOptions(o))
            }
            ClientUpdateEvent::EmbarkResult(r) => {
                history_out.write(HistoryMsg::EmbarkResult(r))
            }
            ClientUpdateEvent::PersonInfo(p) => history_out.write(HistoryMsg::PersonInfo(p)),
            ClientUpdateEvent::Civs(c) => history_out.write(HistoryMsg::Civs(c)),
            ClientUpdateEvent::Legends(p) => history_out.write(HistoryMsg::Legends(p)),
            ClientUpdateEvent::AdventureResult(r) => history_out.write(HistoryMsg::AdventureResult(r)),
        }
    }
}

/// Tear down everything the bridge built for a region: root entity tree,
/// update receiver, entity map entries. Also the first half of
/// replace-on-re-receipt (crash-respawn resync).
fn remove_region(
    commands: &mut Commands,
    regions: &mut Regions,
    roots: &mut RegionRoots,
    map: &mut SimEntityMap,
    id: RegionId,
) {
    if let Some(root) = unmap_region(regions, roots, map, id) {
        // despawn() removes descendants via ChildOf relationships (Bevy 0.16+).
        commands.entity(root).despawn();
    }
}

/// Take a region out of every bridge map and return its root, which the
/// caller despawns.
fn unmap_region(
    regions: &mut Regions,
    roots: &mut RegionRoots,
    map: &mut SimEntityMap,
    id: RegionId,
) -> Option<Entity> {
    regions.0.remove(&id);
    map.0.retain(|(region, _), _| *region != id);
    roots.remove(&id)
}

/// Port of the old `TrueWorld::new` snapshot walk.
fn spawn_region_snapshot(
    commands: &mut Commands,
    root: Entity,
    bodies: Entity,
    region: RegionId,
    data: &GameData,
    map: &mut SimEntityMap,
    local_player: Option<game::ClientId>,
    commander: bool,
) {
    let body_scale = *data.body_scale;
    commands.insert_resource(super::stud::ClientBodyScale(body_scale));
    // Every player in the snapshot has a sim camera, but only the local
    // player's may become this window's active Camera3d.
    let local_camera_key = local_player.and_then(|id| data.player_entites.get(&id).copied());
    // Entity-halo mirrors in the snapshot, by entity.
    let mirrors: BTreeMap<EntityKey, HaloMirror> = data
        .halo_entities
        .iter()
        .map(|(id, h)| (h.entity, HaloMirror { id: *id, source: h.source_region }))
        .collect();
    for (key, _) in data.ecs.entities.iter() {
        // Terrain under the static region root, everything else under the
        // bodies root (see `RegionRoots`).
        let stud = data.ecs.stud.try_get(key).as_ref().cloned();
        let mut e = commands.spawn((
            SimEntity { region, key },
            Transform::IDENTITY,
            Visibility::default(),
            ChildOf(if stud.is_some() { root } else { bodies }),
        ));
        let mut local_position = Vec3::ZERO;
        let mut has_pose = false;
        if let Some(handle) = data.ecs.rigidbody.try_get(key) {
            if let Some(body) = data.physics.bodies.get(*handle) {
                has_pose = true;
                let tf = iso_to_transform(body.position());
                local_position = tf.translation;
                e.insert((tf, SimTarget::body(tf.translation, tf.rotation)));
            }
        }
        if let Some(chunk) = stud {
            if has_pose {
                let coords = game::StudChunkCoords::from_origin(local_position.to_array().into());
                e.insert(super::stud::SimStudChunk { coords, chunk });
            } else {
                warn!("bridge: stud chunk {:?} in snapshot has no body pose; skipped", key);
            }
        }
        if let Some(cam) = data.ecs.camera.try_get(key) {
            if Some(key) == local_camera_key && commander {
                e.insert((LocalBody, Visibility::Hidden));
            } else if Some(key) == local_camera_key {
                if let Some(handle) = cam.view_matrix {
                    if let Some(body) = data.physics.bodies.get(handle) {
                        let mut tf = iso_to_transform(body.position());
                        let target = SimTarget::camera_eyed(tf.translation, tf.rotation, body_scale.eye_offset());
                        tf.translation = target.goal();
                        e.insert((
                            Camera3d::default(),
                            Projection::Perspective(perspective_to_projection(&cam.proj_matrix)),
                            tf,
                            target,
                            super::UiCamera,
                        ));
                    }
                }
            }
            // Other players' pose tracking comes from the rigidbody branch
            // above; their cameras stay inert in this window.
        }
        if let Some(kind) = *data.ecs.kind.try_get(key) {
            e.insert(SimKind(kind));
        }
        if let Some(mirror) = mirrors.get(&key) {
            e.insert(*mirror);
        }
        if let Some(body) = data.ecs.stud_body.try_get(key) {
            e.insert((super::stud::bodies::PendingStudBody(Arc::new(body.model.clone())), super::stud::bodies::StudBodyModel));
        }
        map.0.insert((region, key), e.id());
    }
}

pub fn drain_region_updates(
    mut commands: Commands,
    regions: Res<Regions>,
    roots: Res<RegionRoots>,
    mut map: ResMut<SimEntityMap>,
    mut targets: Query<&mut SimTarget>,
    local_player: Res<LocalPlayer>,
    mut cursor_policy: ResMut<super::cursor::CursorPolicy>,
    camera_mode: Option<Res<super::commander::CameraMode>>,
    mut tile_msgs: Option<ResMut<Messages<TileMirrorMsg>>>,
    mut pile_msgs: Option<ResMut<Messages<super::item_piles::ItemPileMsg>>>,
    body_scale: Res<super::stud::ClientBodyScale>,
) {
    let commander = commander(camera_mode.as_deref());
    // Local cameras inserted by Commands this drain, not yet queryable: a
    // position update for one must keep its camera target (eye offset,
    // smoothing) rather than overwrite it with a body target.
    let mut new_cameras: Vec<Entity> = Vec::new();
    for (&region, receiver) in regions.0.iter() {
        let root = *roots.0.get(&region).expect("RegionRoots co-populated with Regions");
        let bodies = roots.bodies(region).expect("RegionRoots co-populated with Regions");
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
                            ChildOf(bodies),
                        ))
                        .id();
                    map.0.insert((region, key), e);
                }
                GameDataUpdateKind::RemoveEntity(key) => {
                    if let Some(e) = map.0.remove(&(region, key)) {
                        commands.entity(e).despawn();
                    } else {
                        warn!("bridge: RemoveEntity for unmapped {:?}", key);
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
                        // not yet queryable. Insert (overwrites on apply), and
                        // start it at its first pose rather than gliding in
                        // from the region origin.
                        let target = if new_cameras.contains(&e) {
                            SimTarget::camera_eyed(tf.translation, tf.rotation, body_scale.0.eye_offset())
                        } else {
                            SimTarget::body(tf.translation, tf.rotation)
                        };
                        let start = Transform { translation: target.goal(), ..tf };
                        commands.entity(e).insert((start, target));
                    }
                }
                GameDataUpdateKind::AddCameraComponent(key, client_id, proj, iso) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: AddCameraComponent for unmapped {:?}", key);
                        continue;
                    };
                    let tf = iso_to_transform(&iso);
                    if local_player.0 == Some(client_id) && commander {
                        // The commander's own body: tracked, never shown.
                        commands.entity(e).insert((
                            tf,
                            SimTarget::body(tf.translation, tf.rotation),
                            LocalBody,
                            Visibility::Hidden,
                        ));
                    } else if local_player.0 == Some(client_id) {
                        let target = SimTarget::camera_eyed(tf.translation, tf.rotation, body_scale.0.eye_offset());
                        let tf = Transform { translation: target.goal(), ..tf };
                        new_cameras.push(e);
                        commands.entity(e).insert((
                            Camera3d::default(),
                            Projection::Perspective(perspective_to_projection(&proj)),
                            tf,
                            target,
                            super::UiCamera,
                        ));
                    } else {
                        // Another player's camera: track its pose so the
                        // entity follows the sim, but never render from it.
                        commands.entity(e).insert((tf, SimTarget::body(tf.translation, tf.rotation)));
                    }
                }
                GameDataUpdateKind::RemoveCameraComponent(key) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.entity(e).remove::<(Camera3d, Projection)>();
                    } else {
                        warn!("bridge: RemoveCameraComponent for unmapped {:?}", key);
                    }
                }
                GameDataUpdateKind::UpdateCameraViewProj(key, proj) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.entity(e).insert(Projection::Perspective(perspective_to_projection(&proj)));
                    } else {
                        warn!("bridge: UpdateCameraViewProj for unmapped {:?}", key);
                    }
                }
                GameDataUpdateKind::UpdateCameraViewMatrix(key, iso) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: UpdateCameraViewMatrix for unmapped {:?}", key);
                        continue;
                    };
                    let tf = iso_to_transform(&iso);
                    if let Ok(mut target) = targets.get_mut(e) {
                        target.pos = tf.translation;
                        target.rot = tf.rotation;
                    } else {
                        commands.entity(e).insert(SimTarget::camera_eyed(tf.translation, tf.rotation, body_scale.0.eye_offset()));
                    }
                }
                GameDataUpdateKind::SetEntityKind(key, kind) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: SetEntityKind for unmapped {:?}", key);
                        continue;
                    };
                    match kind {
                        Some(k) => { commands.entity(e).insert(SimKind(k)); }
                        None => { commands.entity(e).remove::<SimKind>(); }
                    }
                }
                GameDataUpdateKind::SetHaloSource(key, src) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: SetHaloSource for unmapped {:?}", key);
                        continue;
                    };
                    match src {
                        Some((source, id)) => { commands.entity(e).insert(HaloMirror { id, source }); }
                        None => {
                            commands.entity(e).remove::<HaloMirror>();
                            // Upgraded in place: an owned body draws.
                            commands.entity(e).insert(Visibility::Inherited);
                        }
                    }
                }
                // Plain per-box / per-tile writes, undo included: forwarded to the
                // fortress tools' tile mirror.
                GameDataUpdateKind::SetDesignations(changes) => {
                    if let Some(msgs) = tile_msgs.as_mut() {
                        msgs.write(TileMirrorMsg::Designations { region, changes });
                    }
                }
                GameDataUpdateKind::SetConstructed(changes) => {
                    if let Some(msgs) = tile_msgs.as_mut() {
                        msgs.write(TileMirrorMsg::Constructed { region, changes });
                    }
                }
                // Item piles (`game::item_piles`), drawn by `item_piles`.
                GameDataUpdateKind::SetItemPiles(changes) => {
                    if let Some(msgs) = pile_msgs.as_mut() {
                        msgs.write(super::item_piles::ItemPileMsg::Set { region, changes });
                    }
                }
                GameDataUpdateKind::SetHaloPiles(changes) => {
                    if let Some(msgs) = pile_msgs.as_mut() {
                        msgs.write(super::item_piles::ItemPileMsg::SetMirrors { region, changes });
                    }
                }
                // Render-only pickup / landing events, animated by `item_flights`.
                GameDataUpdateKind::ItemsPickedUp(picks) => {
                    if let Some(msgs) = pile_msgs.as_mut() {
                        msgs.write(super::item_piles::ItemPileMsg::PickedUp { region, picks });
                    }
                }
                GameDataUpdateKind::ItemsLanded(flights) => {
                    if let Some(msgs) = pile_msgs.as_mut() {
                        msgs.write(super::item_piles::ItemPileMsg::Landed { region, flights });
                    }
                }
                GameDataUpdateKind::AddStudChunk(key, coords, chunk) => {
                    // Adopt the entity a runtime `CreateEntity` already made
                    // (so undo's `RemoveEntity` despawns it), else spawn one. Stud chunks
                    // live under the terrain root and never move, so the pose
                    // is the chunk origin.
                    let origin = coords.origin();
                    let bundle = (
                        SimEntity { region, key },
                        Transform::from_xyz(origin.x, origin.y, origin.z),
                        Visibility::default(),
                        super::stud::SimStudChunk { coords, chunk: *chunk },
                        ChildOf(root),
                    );
                    if let Some(&existing) = map.0.get(&(region, key)) {
                        commands.entity(existing).insert(bundle);
                    } else {
                        let e = commands.spawn(bundle).id();
                        map.0.insert((region, key), e);
                    }
                }
                GameDataUpdateKind::SetStudChunk(key, chunk) => {
                    // Queued (not a query): the entity's `SimStudChunk` may be
                    // inserted by Commands earlier in this same drain.
                    match map.0.get(&(region, key)) {
                        Some(&e) => commands.queue(move |world: &mut World| {
                            match world.get_mut::<super::stud::SimStudChunk>(e) {
                                Some(mut mirror) => mirror.chunk = *chunk,
                                None => warn!("bridge: SetStudChunk for unmirrored {:?}", key),
                            }
                        }),
                        None => warn!("bridge: SetStudChunk for unmapped {:?}", key),
                    }
                }
                // Detached bricks (`game::stud_body`): meshed by `stud::bodies` into
                // children that ride the entity's interpolated pose;
                // `RemoveEntity` despawns them with it. Starts at the pose
                // rather than gliding in.
                GameDataUpdateKind::AddStudBody(key, iso, model) => {
                    let tf = iso_to_transform(&iso);
                    let bundle = (
                        tf,
                        SimTarget::body(tf.translation, tf.rotation),
                        super::stud::bodies::PendingStudBody(Arc::new(*model)),
                        super::stud::bodies::StudBodyModel,
                    );
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.entity(e).insert(bundle);
                    } else {
                        let e = commands
                            .spawn((SimEntity { region, key }, Visibility::default(), ChildOf(bodies), bundle))
                            .id();
                        map.0.insert((region, key), e);
                    }
                }
                // The stud halo (render-only mirror): for the stud index (edge
                // culling) and the tile mirror. Queued so it lands after this
                // region's snapshot message.
                kind @ (GameDataUpdateKind::SetStudHalo(_) | GameDataUpdateKind::SetStudHaloGround(_)) => {
                    if let Some(msg) = super::stud::world::StudWorldMsg::from_update(region, &kind) {
                        commands.queue(move |world: &mut World| {
                            if let Some(mut msgs) = world.get_resource_mut::<Messages<super::stud::world::StudWorldMsg>>() {
                                msgs.write(msg);
                            }
                        });
                    }
                }
                GameDataUpdateKind::SetFreeCam(client_id, enabled) => {
                    // Only the local player's toggle may grab this window's cursor.
                    if local_player.0 != Some(client_id) {
                        continue;
                    }
                    // Idempotent: SetFreeCam is emitted Do-only (no Undo
                    // pairing), so every reconcile that replays the toggle tick
                    // re-delivers the SAME value; `apply_cursor_policy` only
                    // writes the window on a real change.
                    cursor_policy.sim_grab = enabled;
                }
                // Sim-owned animation (`game::anim`), drawn by `rigs`.
                GameDataUpdateKind::SetAnimator(key, animator) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: SetAnimator for unmapped {:?}", key);
                        continue;
                    };
                    match animator {
                        Some(a) => { commands.entity(e).insert(super::rigs::SimAnimator(a)); }
                        None => { commands.entity(e).remove::<super::rigs::SimAnimator>(); }
                    }
                }
            }
        }
    }
}

/// Render rule: one copy per `EntityId`. A halo mirror hides while its
/// source region is loaded locally (the owned copy draws there); otherwise
/// the mirror in the lowest hosting region draws and the others (a corner
/// entity is mirrored into up to 3 regions) hide. Runs every frame; mirrors
/// are few.
pub fn dedupe_halo_entities(
    roots: Res<RegionRoots>,
    mut mirrors: Query<(&HaloMirror, &SimEntity, &mut Visibility)>,
) {
    // The host region drawing each id whose owner is not loaded here.
    let mut drawn: BTreeMap<game::EntityId, RegionId> = BTreeMap::new();
    for (m, host, _) in mirrors.iter() {
        if !roots.0.contains_key(&m.source) {
            let r = drawn.entry(m.id).or_insert(host.region);
            *r = (*r).min(host.region);
        }
    }
    for (m, host, mut vis) in mirrors.iter_mut() {
        let shown = drawn.get(&m.id) == Some(&host.region);
        let want = if shown { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want {
            *vis = want;
        }
    }
}

/// Enforce exactly one active render camera: the local player's camera in the
/// *home* region. During a boundary handoff the player is briefly owned by both
/// the outgoing and incoming region, so both spawn a local `Camera3d` +
/// `UiCamera`. Two active cameras with equal priority make Bevy pick
/// ambiguously per frame (view flickers; mouse-look/HUD bind to the wrong one).
///
/// Rather than remove `Camera3d` (which would need fragile re-add logic when
/// the player crosses back into a still-loaded region), this toggles
/// `Camera::is_active` and moves the single `UiCamera` marker so only
/// the home camera renders. All `Camera3d` entities here are the local player's
/// (the bridge never gives another player a `Camera3d`), so no ownership filter
/// is needed. Falls back to a deterministic keeper if no camera sits in the
/// home region yet (handoff mid-flight) to avoid a black screen or flicker.
///
/// While the frontend has the screen (`session::frontend_has_screen`: menu,
/// starting screen, arrival map) no body camera is active or carries
/// `UiCamera` — the frontend camera does.
pub fn enforce_single_active_camera(
    mut commands: Commands,
    hud_status: Res<HudStatus>,
    state: Option<Res<State<crate::session::AppState>>>,
    embarked: Option<Res<crate::session::Embarked>>,
    mut cams: Query<(Entity, &SimEntity, &mut Camera, Has<super::UiCamera>), With<Camera3d>>,
) {
    if crate::session::frontend_has_screen(state.as_deref(), embarked.as_deref()) {
        for (e, _, mut cam, has_marker) in &mut cams {
            if cam.is_active {
                cam.is_active = false;
            }
            if has_marker {
                commands.entity(e).remove::<super::UiCamera>();
            }
        }
        return;
    }
    let home = hud_status.home_region;
    // Choose the single keeper: the home-region camera if one exists, else a
    // deterministic fallback (lowest Entity) so the pick doesn't flip frame to
    // frame while a handoff is mid-flight (which would itself flicker).
    let mut home_keeper: Option<Entity> = None;
    let mut fallback: Option<Entity> = None;
    for (e, se, _, _) in cams.iter() {
        if fallback.is_none_or(|f| e < f) {
            fallback = Some(e);
        }
        if Some(se.region) == home && home_keeper.is_none_or(|k| e < k) {
            home_keeper = Some(e);
        }
    }
    let Some(keeper) = home_keeper.or(fallback) else { return };

    for (e, _se, mut cam, has_marker) in cams.iter_mut() {
        let active = e == keeper;
        if cam.is_active != active {
            cam.is_active = active;
        }
        match (active, has_marker) {
            (true, false) => {
                commands.entity(e).insert(super::UiCamera);
            }
            (false, true) => {
                commands.entity(e).remove::<super::UiCamera>();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::{ClientUpdates, GameEvents, LocalPlayer};
    use game::{ClientUpdateEvent, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, RegionCoords, Rollback};
    use game::EntityKey;
    use slotmapd::KeyData;

    fn app_shell() -> (App, crossbeam::channel::Sender<ClientUpdateEvent>) {
        let (client_send, client_recv) = crossbeam::channel::unbounded();
        let (game_send, _game_recv) = crossbeam::channel::unbounded();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(ClientUpdates(client_recv))
            .insert_resource(GameEvents(game_send))
            .init_resource::<LocalPlayer>()
            .init_resource::<Regions>()
            .init_resource::<RegionRoots>()
            .init_resource::<SimEntityMap>()
            .init_resource::<HudStatus>()
            .init_resource::<crate::renderer::slint_overlay::UiUpdates>()
            .init_resource::<crate::renderer::collider_debug::DebugColliderLines>()
            .init_resource::<lod_rings::LodRings>()
            .init_resource::<crate::renderer::cursor::CursorPolicy>()
            .init_resource::<RetiredRegionRoots>()
            .init_resource::<crate::renderer::stud::ClientBodyScale>()
            .add_systems(
                PreUpdate,
                (
                    drain_client_updates,
                    despawn_retired_regions,
                    drain_region_updates,
                )
                    .chain(),
            );
        (app, client_send)
    }

    fn test_app() -> (App, crossbeam::channel::Sender<ClientUpdateEvent>, crossbeam::channel::Sender<GameDataUpdate>, game::RegionId) {
        let (app, client_send) = app_shell();
        let (update_send, update_recv) = crossbeam::channel::unbounded();
        let region_id = RegionCoords::new(0, 0);
        let rb = Rollback::new(None);
        client_send
            .send(ClientUpdateEvent::NewRegion(region_id, (*rb.data).clone(), update_recv))
            .unwrap();
        (app, client_send, update_send, region_id)
    }

    /// `AddStudChunk` then `SetStudChunk` drained in ONE call: the set must
    /// land on the mirror even though the add is still deferred Commands.
    #[test]
    fn set_stud_chunk_in_same_drain_as_add() {
        use crate::renderer::stud::SimStudChunk;
        let (mut app, _client, updates, region) = test_app();
        app.update();
        let key = key(31);
        let brick = |min| game::StudBrick { min, shape: game::StudShape::Filler { size: [1, 1, 1] }, material: 0, data: 0 };
        let first = game::StudChunk::new([brick([0, 0, 0])]).unwrap();
        let second = game::StudChunk::new([brick([1, 0, 0]), brick([2, 0, 0])]).unwrap();
        let coords = game::StudChunkCoords { x: 1, y: 0, z: 2 };
        let send = |kind| updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, kind)).unwrap();
        send(GameDataUpdateKind::AddStudChunk(key, coords, Box::new(first)));
        send(GameDataUpdateKind::SetStudChunk(key, Box::new(second.clone())));
        app.update();
        app.update();
        let e = *app.world().resource::<SimEntityMap>().0.get(&(region, key)).unwrap();
        let mirror = app.world().get::<SimStudChunk>(e).expect("mirror");
        assert_eq!(mirror.chunk, second);
        assert_eq!(mirror.coords, coords);
    }

    /// The stud halo's render updates reach the stud index as messages (never as
    /// entities), and a stud body's model is tagged for meshing on its entity.
    #[test]
    fn stud_halo_updates_and_stud_bodies_reach_the_stud_systems() {
        use crate::renderer::stud::bodies::PendingStudBody;
        use crate::renderer::stud::world::{StudWorld, StudWorldMsg};
        let (mut app, _client, updates, region) = test_app();
        app.add_message::<StudWorldMsg>().init_resource::<StudWorld>();
        // Stand-in for the stud plugin's index system.
        app.add_systems(Update, |mut w: ResMut<StudWorld>, mut msgs: MessageReader<StudWorldMsg>| {
            for m in msgs.read() {
                w.apply(m);
            }
        });
        app.update();
        let count = |app: &mut App| app.world_mut().query::<Entity>().iter(app.world()).count();
        let entities_before = count(&mut app);
        let brick = game::StudBrick::filler([0, 0, 0], [1, 1, 1], 4);
        let chunk = game::StudChunk::new([brick]).unwrap();
        let at = game::WorldChunkPos::from_local(region, [-1, 0, 0]);
        let send = |kind| updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, kind)).unwrap();
        // Before the region's snapshot message the index drops it; the test app's
        // region has none, so announce it the way a snapshot does.
        app.world_mut().resource_mut::<Messages<StudWorldMsg>>().write(StudWorldMsg::Region {
            region,
            ground: Default::default(),
            halo: Vec::new(),
            halo_ground: Vec::new(),
        });
        send(GameDataUpdateKind::SetStudHalo(vec![(at, Some(chunk.clone()))]));
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(app.world().resource::<StudWorld>().halo_chunk(at), Some(&chunk));
        assert_eq!(count(&mut app), entities_before, "a halo chunk is data: nothing is spawned to draw");
        send(GameDataUpdateKind::SetStudHalo(vec![(at, None)]));
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(app.world().resource::<StudWorld>().halo_chunk(at), None);

        let key = key(32);
        let model = game::stud_body::StudModel {
            bricks: vec![game::stud_body::StudModelBrick { min: [0, 0, 0], shape: brick.shape, material: 4, data: 0 }],
        };
        send(GameDataUpdateKind::AddStudBody(key, game::Pose::identity(), Box::new(model.clone())));
        app.update();
        app.update();
        let e = *app.world().resource::<SimEntityMap>().0.get(&(region, key)).unwrap();
        assert_eq!(*app.world().get::<PendingStudBody>(e).expect("tagged for meshing").0, model);
        assert!(app.world().get::<crate::renderer::stud::bodies::StudBodyModel>(e).is_some(), "no placeholder box");
    }

    /// The runtime spawn path: `CreateEntity` then `AddStudChunk` for the same
    /// key make ONE sim entity, under the terrain root; undo's `RemoveEntity`
    /// despawns it.
    #[test]
    fn runtime_stud_chunk_adopts_the_created_entity_and_undo_despawns_it() {
        use crate::renderer::stud::SimStudChunk;
        let (mut app, _client, updates, region) = test_app();
        app.update();
        let k = key(21);
        let coords = game::StudChunkCoords { x: 1, y: -1, z: 2 };
        let chunk = game::StudChunk::new([game::StudBrick::filler([0, 0, 0], [1, 1, 1], 4)]).unwrap();
        let send = |kind, how| updates.send(GameDataUpdate::new(how, kind)).unwrap();
        send(GameDataUpdateKind::CreateEntity(k), GameDataTransactionKind::Do);
        send(GameDataUpdateKind::AddStudChunk(k, coords, Box::new(chunk.clone())), GameDataTransactionKind::Do);
        app.update();
        app.update();
        let e = *app.world().resource::<SimEntityMap>().0.get(&(region, k)).expect("chunk mapped");
        let mut sims = app.world_mut().query::<&SimEntity>();
        assert_eq!(sims.iter(app.world()).filter(|s| s.key == k).count(), 1, "one sim entity for the key");
        assert_eq!(app.world().get::<SimStudChunk>(e).map(|c| (c.coords, c.chunk.clone())), Some((coords, chunk)));
        let root = *app.world().resource::<RegionRoots>().0.get(&region).unwrap();
        assert_eq!(app.world().get::<ChildOf>(e).map(ChildOf::parent), Some(root), "terrain hangs off the terrain root");
        let origin = coords.origin();
        assert_eq!(app.world().get::<Transform>(e).unwrap().translation, Vec3::new(origin.x, origin.y, origin.z));

        send(GameDataUpdateKind::RemoveEntity(k), GameDataTransactionKind::Undo);
        app.update();
        assert!(app.world().resource::<SimEntityMap>().0.get(&(region, k)).is_none());
        assert!(app.world().get_entity(e).is_err(), "sim entity despawned");
    }

    fn test_proj() -> game::Perspective3 {
        game::Perspective3::new(
            1.5,
            1.2,
            0.1,
            100.0,
        )
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
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetEntityPosition(k, game::Pose::identity()))).unwrap();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).expect("entity mapped");
        assert!(app.world().entity(e).contains::<SimTarget>());
        let target = app.world().entity(e).get::<SimTarget>().expect("SimTarget present");
        assert_eq!(target.pos, Vec3::ZERO, "identity isometry should convert to zero translation");
        assert_eq!(target.rot, Quat::IDENTITY, "identity isometry should convert to identity rotation");

        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::RemoveEntity(k))).unwrap();
        app.update();
        assert!(app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).is_none());
        assert!(app.world().get_entity(e).is_err(), "despawned");
    }

    #[test]
    fn unknown_key_is_tolerated() {
        let (mut app, _c, updates, _region_id) = test_app();
        app.update();
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetEntityPosition(key(99), game::Pose::identity()))).unwrap();
        app.update(); // must not panic
    }

    #[test]
    fn camera_add_update_remove() {
        let (mut app, client, updates, region_id) = test_app();
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        app.update();
        let k = key(3);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::AddCameraComponent(k, 0, test_proj(), game::Pose::identity()),
        )).unwrap();
        app.update();
        // second frame: AddCameraComponent on a same-drain-spawned entity goes through Commands
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).unwrap();
        assert!(app.world().entity(e).contains::<Camera3d>());
        assert!(
            app.world().entity(e).contains::<crate::renderer::UiCamera>(),
            "local player's camera must be the default UI camera"
        );
        let Projection::Perspective(p) = app.world().entity(e).get::<Projection>().unwrap() else {
            panic!("expected perspective projection");
        };
        assert!((p.fov - 1.2).abs() < 1e-6);

        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::RemoveCameraComponent(k))).unwrap();
        app.update();
        assert!(!app.world().entity(e).contains::<Camera3d>());
    }

    #[test]
    fn foreign_camera_is_tracked_but_not_activated() {
        let (mut app, client, updates, region_id) = test_app();
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        app.update();
        let k = key(4);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            // client 1's camera arriving at client 0's window
            GameDataUpdateKind::AddCameraComponent(k, 1, test_proj(), game::Pose::identity()),
        )).unwrap();
        app.update();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).unwrap();
        assert!(
            !app.world().entity(e).contains::<Camera3d>(),
            "another player's camera must not render this window"
        );
        assert!(
            app.world().entity(e).contains::<SimTarget>(),
            "foreign player's pose must still be tracked"
        );
    }

    #[test]
    fn set_entity_kind_mirrors_to_sim_kind() {
        let (mut app, _c, updates, region_id) = test_app();
        app.update();
        let k = key(5);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(k, Some(game::EntityKind::Player)),
        )).unwrap();
        app.update();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).unwrap();
        assert!(app.world().entity(e).contains::<SimKind>());

        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(k, None),
        )).unwrap();
        app.update();
        assert!(!app.world().entity(e).contains::<SimKind>());
    }

    #[test]
    fn snapshot_carries_entity_kind() {
        let (mut app, client) = app_shell();
        let (_update_send, update_recv) = crossbeam::channel::unbounded();
        let mut rb = Rollback::new(None);
        rb.new_transaction();
        rb.create_player_safe(0);
        rb.create_player_safe(1);
        let data = (*rb.data).clone();
        let k0 = *data.player_entites.get(&0).unwrap();
        let k1 = *data.player_entites.get(&1).unwrap();

        let region_id = RegionCoords::new(0, 0);
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_id, data, update_recv)).unwrap();
        app.update();

        let map = app.world().resource::<SimEntityMap>();
        let e0 = *map.0.get(&(region_id, k0)).unwrap();
        let e1 = *map.0.get(&(region_id, k1)).unwrap();
        assert!(app.world().entity(e0).contains::<SimKind>());
        assert!(app.world().entity(e1).contains::<SimKind>());
    }

    #[test]
    fn region_root_sits_at_world_offset() {
        let (mut app, client) = app_shell();
        let (_send, update_recv) = crossbeam::channel::unbounded();
        let region_id = game::RegionCoords::new(1, -2);
        let rb = Rollback::new(None);
        client
            .send(ClientUpdateEvent::NewRegion(region_id, (*rb.data).clone(), update_recv))
            .unwrap();
        app.update();
        let root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();
        let tf = app.world().entity(root).get::<Transform>().unwrap();
        assert_eq!(tf.translation, Vec3::new(game::REGION_SIZE, 0.0, -2.0 * game::REGION_SIZE));
    }

    #[test]
    fn debug_lines_populate_replace_and_teardown() {
        let (mut app, send) = app_shell();
        let id = RegionCoords::new(0, 0);
        let line = game::DebugLine {
            a: [0.0, 0.0, 0.0],
            b: [1.0, 0.0, 0.0],
            color: [30.0, 1.0, 0.4, 1.0],
        };
        send.send(ClientUpdateEvent::DebugLines(id, vec![line])).unwrap();
        app.update();
        let lines = &app.world().resource::<crate::renderer::collider_debug::DebugColliderLines>().0;
        assert_eq!(lines.get(&id).map(|v| v.len()), Some(1));

        // Replace-on-receipt: an empty buffer overwrites, not appends.
        send.send(ClientUpdateEvent::DebugLines(id, vec![])).unwrap();
        app.update();
        let lines = &app.world().resource::<crate::renderer::collider_debug::DebugColliderLines>().0;
        assert_eq!(lines.get(&id).map(|v| v.len()), Some(0));

        // RemoveRegion drops the entry entirely.
        send.send(ClientUpdateEvent::RemoveRegion(id)).unwrap();
        app.update();
        let lines = &app.world().resource::<crate::renderer::collider_debug::DebugColliderLines>().0;
        assert!(lines.get(&id).is_none());
    }

    #[test]
    fn remove_region_tears_down_root_maps_and_receiver() {
        let (mut app, client, _updates, region_id) = test_app();
        app.update();
        let root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();

        client.send(ClientUpdateEvent::RemoveRegion(region_id)).unwrap();
        app.update();

        assert!(app.world().get_entity(root).is_err(), "root despawned (with children)");
        assert!(!app.world().resource::<RegionRoots>().0.contains_key(&region_id));
        assert!(!app.world().resource::<Regions>().0.contains_key(&region_id));
        assert!(app.world().resource::<SimEntityMap>().0.keys().all(|(r, _)| *r != region_id));
    }

    /// A window shift releases a row of regions at once; their trees go one
    /// per frame, and a region re-received meanwhile loses its old tree first.
    #[test]
    fn released_regions_despawn_one_per_frame_and_rereceipt_flushes() {
        let (mut app, client, _updates, a) = test_app();
        let b = RegionCoords::new(1, 0);
        let c = RegionCoords::new(2, 0);
        for id in [b, c] {
            let (_send, recv) = crossbeam::channel::unbounded();
            let rb = Rollback::new(None);
            client.send(ClientUpdateEvent::NewRegion(id, (*rb.data).clone(), recv)).unwrap();
        }
        // One tree spawns per frame.
        for _ in 0..3 {
            app.update();
        }
        let root = |app: &App, id| *app.world().resource::<RegionRoots>().0.get(&id).unwrap();
        let (ra, rb_, rc) = (root(&app, a), root(&app, b), root(&app, c));

        for id in [a, b, c] {
            client.send(ClientUpdateEvent::RemoveRegion(id)).unwrap();
        }
        app.update();
        assert!(app.world().resource::<RegionRoots>().0.is_empty(), "unmapped at once");
        assert!(app.world().get_entity(ra).is_err(), "first tree despawned this frame");
        assert!(app.world().get_entity(rb_).is_ok(), "the rest wait their frame");

        // c comes straight back before its old tree's turn.
        let (_send, recv) = crossbeam::channel::unbounded();
        let rb = Rollback::new(None);
        client.send(ClientUpdateEvent::NewRegion(c, (*rb.data).clone(), recv)).unwrap();
        app.update();
        assert!(app.world().get_entity(rc).is_err(), "old tree flushed by the re-receipt");
        assert!(app.world().get_entity(rb_).is_err(), "b's turn came");
        assert_ne!(root(&app, c), rc);
    }

    #[test]
    fn new_region_for_loaded_region_replaces_it() {
        let (mut app, client, _updates, region_id) = test_app();
        app.update();
        let first_root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();

        let (_send2, update_recv2) = crossbeam::channel::unbounded();
        let rb = Rollback::new(None);
        client
            .send(ClientUpdateEvent::NewRegion(region_id, (*rb.data).clone(), update_recv2))
            .unwrap();
        app.update();

        let second_root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();
        assert_ne!(first_root, second_root, "old root replaced");
        assert!(app.world().get_entity(first_root).is_err(), "old root despawned");
    }

    #[test]
    fn snapshot_spawns_camera_only_for_local_player() {
        let (mut app, client) = app_shell();
        let (_update_send, update_recv) = crossbeam::channel::unbounded();

        // Region snapshot that already contains two players (the join flow).
        let mut rb = Rollback::new(None);
        rb.new_transaction();
        rb.create_player_safe(0);
        rb.create_player_safe(1);
        let data = (*rb.data).clone();
        let k0 = *data.player_entites.get(&0).unwrap();
        let k1 = *data.player_entites.get(&1).unwrap();

        let region_id = RegionCoords::new(0, 0);
        // SetPlayer always precedes NewRegion (PlayerRegion precedes Region).
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_id, data, update_recv)).unwrap();
        app.update();

        let map = app.world().resource::<SimEntityMap>();
        let e0 = *map.0.get(&(region_id, k0)).unwrap();
        let e1 = *map.0.get(&(region_id, k1)).unwrap();
        assert!(app.world().entity(e0).contains::<Camera3d>(), "local player's camera is active");
        assert!(
            !app.world().entity(e1).contains::<Camera3d>(),
            "other player's camera from the snapshot must not be active"
        );
    }

    #[test]
    fn halo_mirror_drawn_once_per_entity_id() {
        let (mut app, client, updates, region_id) = test_app();
        app.add_systems(Update, dedupe_halo_entities);
        // A second host of the same entity's mirror (a corner entity).
        let other_host = RegionCoords::new(0, 1);
        let (updates2, recv2) = crossbeam::channel::unbounded();
        client.send(ClientUpdateEvent::NewRegion(other_host, (*Rollback::new(None).data).clone(), recv2)).unwrap();
        app.update();

        let k = key(11);
        let src = RegionCoords::new(1, 0);
        let id = game::EntityId { birth_region: src, serial: 3 };
        for u in [&updates, &updates2] {
            u.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
            u.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetHaloSource(k, Some((src, id)))))
                .unwrap();
        }
        app.update();
        app.update(); // component insert lands a frame after spawn-in-same-drain

        let map = app.world().resource::<SimEntityMap>();
        let e = *map.0.get(&(region_id, k)).unwrap();
        let e2 = *map.0.get(&(other_host, k)).unwrap();
        let vis = |app: &App, e: Entity| *app.world().entity(e).get::<Visibility>().unwrap();
        // Source region NOT loaded: one mirror draws — the lowest host's.
        assert_ne!(vis(&app, e), Visibility::Hidden, "the mirror renders while its source region is absent");
        assert_eq!(vis(&app, e2), Visibility::Hidden, "a second mirror of the same id hides");

        // Load the source region: the owned copy renders there; both mirrors hide.
        let (_s3, recv3) = crossbeam::channel::unbounded();
        client.send(ClientUpdateEvent::NewRegion(src, (*Rollback::new(None).data).clone(), recv3)).unwrap();
        app.update();
        app.update();
        assert_eq!(vis(&app, e), Visibility::Hidden, "the mirror hides when its source region loads");
        assert_eq!(vis(&app, e2), Visibility::Hidden);

        // Unload the source: one mirror again.
        client.send(ClientUpdateEvent::RemoveRegion(src)).unwrap();
        app.update();
        app.update();
        assert_ne!(vis(&app, e), Visibility::Hidden);
        assert_eq!(vis(&app, e2), Visibility::Hidden);

        // Upgrade clears the mark: never hidden again, even with the old
        // source loaded.
        updates
            .send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetHaloSource(k, None)))
            .unwrap();
        client
            .send(ClientUpdateEvent::NewRegion(src, (*Rollback::new(None).data).clone(), {
                let (_s4, r4) = crossbeam::channel::unbounded();
                r4
            }))
            .unwrap();
        app.update();
        app.update();
        assert_ne!(vis(&app, e), Visibility::Hidden, "upgraded (owned) entity is never deduped");
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn body_camera_yields_to_the_frontend() {
        use crate::session::{AppState, Embarked, SessionPlugin};
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::state::app::StatesPlugin, SessionPlugin { skip_menu: false, login_first: false }))
            .init_resource::<HudStatus>()
            .add_systems(PreUpdate, enforce_single_active_camera);
        let cam = app
            .world_mut()
            .spawn((
                SimEntity { region: RegionCoords::new(0, 0), key: key(1) },
                Camera3d::default(),
                Camera { is_active: true, ..default() },
                crate::renderer::UiCamera,
            ))
            .id();
        let state = |app: &App| {
            let e = app.world().entity(cam);
            (e.get::<Camera>().unwrap().is_active, e.contains::<crate::renderer::UiCamera>())
        };

        app.update();
        assert_eq!(state(&app), (false, false), "menu: the body camera neither renders nor has the UI");

        app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::InWorld);
        app.update();
        app.update();
        assert_eq!(state(&app), (false, false), "in world but not embarked: still the frontend's");

        app.world_mut().resource_mut::<Embarked>().0 = true;
        app.update();
        assert_eq!(state(&app), (true, true), "embarked: the body camera has the screen again");
    }

    #[test]
    fn only_home_region_camera_is_active_across_regions() {
        use game::ConnectionState;

        let (mut app, client) = app_shell();
        app.add_systems(
            PreUpdate,
            enforce_single_active_camera.after(drain_region_updates),
        );

        // Two loaded regions that BOTH have the local player owning a camera —
        // the transient dual-ownership a boundary handoff produces.
        let region_a = RegionCoords::new(0, 0);
        let region_b = RegionCoords::new(1, 0);

        let mut rb_a = Rollback::new(None);
        rb_a.new_transaction();
        rb_a.create_player_safe(0);
        let data_a = (*rb_a.data).clone();
        let ka = *data_a.player_entites.get(&0).unwrap();

        let mut rb_b = Rollback::new(None);
        rb_b.new_transaction();
        rb_b.create_player_safe(0);
        let data_b = (*rb_b.data).clone();
        let kb = *data_b.player_entites.get(&0).unwrap();

        let (_sa, ra) = crossbeam::channel::unbounded();
        let (_sb, rbr) = crossbeam::channel::unbounded();
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_a, data_a, ra)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_b, data_b, rbr)).unwrap();
        client
            .send(ClientUpdateEvent::HudStatus {
                home_region: region_a,
                viewer_region: region_a,
                connection: ConnectionState::Ready,
            })
            .unwrap();
        // One region tree spawns per frame; a frame more for the second.
        app.update();
        app.update();
        app.update();

        let map = app.world().resource::<SimEntityMap>();
        let ea = *map.0.get(&(region_a, ka)).unwrap();
        let eb = *map.0.get(&(region_b, kb)).unwrap();

        assert!(
            app.world().entity(ea).get::<Camera>().unwrap().is_active,
            "home-region camera must be the active one"
        );
        assert!(
            !app.world().entity(eb).get::<Camera>().unwrap().is_active,
            "non-home-region camera must be inactive"
        );
        assert!(
            app.world().entity(ea).contains::<crate::renderer::UiCamera>(),
            "home-region camera must be the default UI camera"
        );
        assert!(
            !app.world().entity(eb).contains::<crate::renderer::UiCamera>(),
            "non-home-region camera must not keep the default UI camera marker"
        );
    }

    #[test]
    fn active_camera_follows_home_region_flip() {
        use game::ConnectionState;

        let (mut app, client) = app_shell();
        app.add_systems(
            PreUpdate,
            enforce_single_active_camera.after(drain_region_updates),
        );

        let region_a = RegionCoords::new(0, 0);
        let region_b = RegionCoords::new(1, 0);
        let mut rb_a = Rollback::new(None);
        rb_a.new_transaction();
        rb_a.create_player_safe(0);
        let data_a = (*rb_a.data).clone();
        let ka = *data_a.player_entites.get(&0).unwrap();
        let mut rb_b = Rollback::new(None);
        rb_b.new_transaction();
        rb_b.create_player_safe(0);
        let data_b = (*rb_b.data).clone();
        let kb = *data_b.player_entites.get(&0).unwrap();

        let (_sa, ra) = crossbeam::channel::unbounded();
        let (_sb, rbr) = crossbeam::channel::unbounded();
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_a, data_a, ra)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_b, data_b, rbr)).unwrap();
        let hud = |home| ClientUpdateEvent::HudStatus {
            home_region: home,
            viewer_region: home,
            connection: ConnectionState::Ready,
        };
        client.send(hud(region_a)).unwrap();
        // One region tree spawns per frame; a frame more for the second.
        app.update();
        app.update();
        app.update();

        let (ea, eb) = {
            let map = app.world().resource::<SimEntityMap>();
            (*map.0.get(&(region_a, ka)).unwrap(), *map.0.get(&(region_b, kb)).unwrap())
        };
        assert!(app.world().entity(ea).get::<Camera>().unwrap().is_active);
        assert!(!app.world().entity(eb).get::<Camera>().unwrap().is_active);

        // Cross into region_b: home flips. The region_b camera — whose
        // `Camera3d` was never removed, only deactivated — must reactivate,
        // and region_a's must go inactive. No AddCameraComponent is replayed.
        client.send(hud(region_b)).unwrap();
        app.update();

        assert!(
            !app.world().entity(ea).get::<Camera>().unwrap().is_active,
            "old home camera deactivates after the flip"
        );
        assert!(
            app.world().entity(eb).get::<Camera>().unwrap().is_active,
            "new home camera reactivates after the flip"
        );
        assert!(app.world().entity(eb).contains::<crate::renderer::UiCamera>());
        assert!(!app.world().entity(ea).contains::<crate::renderer::UiCamera>());
    }

    #[test]
    fn drain_client_updates_writes_hud_status() {
        use game::ConnectionState;

        let (client, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(ClientUpdates(client_recv))
            .init_resource::<LocalPlayer>()
            .init_resource::<Regions>()
            .init_resource::<RegionRoots>()
            .init_resource::<RetiredRegionRoots>()
            .init_resource::<SimEntityMap>()
            .init_resource::<HudStatus>()
            .init_resource::<crate::renderer::slint_overlay::UiUpdates>()
            .init_resource::<crate::renderer::collider_debug::DebugColliderLines>()
            .init_resource::<lod_rings::LodRings>()
            .add_systems(Update, drain_client_updates);

        client
            .send(ClientUpdateEvent::HudStatus {
                home_region: RegionCoords::new(2, -1),
                viewer_region: RegionCoords::new(2, 0),
                connection: ConnectionState::Ready,
            })
            .unwrap();

        app.update();

        let status = app.world().resource::<HudStatus>();
        assert_eq!(status.home_region, Some(RegionCoords::new(2, -1)));
        assert_eq!(status.viewer_region, Some(RegionCoords::new(2, 0)));
        assert_eq!(status.connection, ConnectionState::Ready);
    }
}
