//! ECS side of the stud mesher: keep the [`StudWorld`] index in step with the
//! stud chunk entities, hand dirty chunks to the compute pool as
//! [`StudMeshJob`]s (a few per frame), land their meshes (a few per frame), and
//! keep the detail (studs and seam lines) on the chunks near the viewer — the
//! commander camera's focus, or the eye in first person ([`stud_detail`]).

use std::collections::BTreeMap;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, Task};
use game::movement::UNITS_PER_METER;
use game::{WorldChunkPos, CHUNK_UNITS, STUD_CHUNK_HEIGHT_UNITS};

use super::material::{StudMaterials, StudTerrainMaterial};
use super::mesh::{StudMeshData, StudMeshOutput};
use super::world::{world_chunk, ChunkDrawn, Finished, StudWorld, StudWorldMsg};
use super::SimStudChunk;
use crate::renderer::bridge::{LocalBody, RegionRoots, SimEntity};
use crate::renderer::commander::first_person::FirstPerson;
use crate::renderer::commander::CommanderCamera;

/// Stud mesh jobs started per frame at most (each clones its chunk on the main thread).
pub const STUD_MESH_JOBS_PER_FRAME: usize = 8;
/// Stud mesh jobs in flight at most.
pub const STUD_MESH_MAX_IN_FLIGHT: usize = 16;
/// Finished stud chunk meshes uploaded per frame at most.
pub const STUD_MESH_UPLOADS_PER_FRAME: usize = 8;
/// Studs and seam lines are meshed and drawn for chunks whose centre is within
/// this of the commander camera's focus, metres (and kept a little farther:
/// `world::STUD_DETAIL_KEEP_MARGIN_UNITS`). Five chunks: at the first 40 m (2.5
/// chunks) the bare ground began in plain sight of the viewer. The cost grows
/// with the area — `measure::MAX_DETAIL_TRIS` gates it.
pub const STUD_TOPS_RADIUS_M: f32 = 80.0;
/// ... and within this of the eye in first person, metres. The same reach: a
/// stud is as small on screen at that distance from the eye as from the focus.
pub const STUD_TOPS_FIRST_PERSON_RADIUS_M: f32 = STUD_TOPS_RADIUS_M;

/// The body mesh child of a stud chunk.
#[derive(Component)]
pub struct StudBodyMesh;

/// The detail mesh child of a stud chunk: its studs and seam lines, there only
/// while the chunk is near the viewer (see [`stud_detail`]).
#[derive(Component)]
pub struct StudTopMesh;

/// The glass mesh child of a stud chunk: its transparent bricks, blended.
#[derive(Component)]
pub struct StudGlassMesh;

/// On a stud chunk: its mesh children.
#[derive(Component, Clone, Copy)]
pub struct StudMeshChildren {
    pub body: Entity,
    pub top: Entity,
    pub glass: Entity,
}

/// A finished job's meshes (`None` = nothing to draw) and its frame's offset.
pub struct StudMeshes {
    pub body: Option<Mesh>,
    pub top: Option<Mesh>,
    pub glass: Option<Mesh>,
    pub offset: Vec3,
}

struct InFlight {
    entity: Entity,
    stamp: u64,
    task: Task<StudMeshes>,
}

/// Chunk mesh jobs in flight, by world chunk. Dropped with the session (which
/// cancels them where the executor can).
#[derive(Resource, Default)]
pub struct StudMeshTasks(BTreeMap<WorldChunkPos, InFlight>);

impl StudMeshTasks {
    /// Chunk mesh jobs in flight.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

pub(super) fn to_mesh(d: StudMeshData) -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, d.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, d.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, d.colors)
        .with_inserted_indices(Indices::U32(d.indices))
}

/// A job's output as Bevy meshes: the completion step of every stud mesh job
/// (cheap — it only wraps the vectors).
pub fn to_meshes(out: StudMeshOutput) -> StudMeshes {
    let wrap = |d: StudMeshData| (!d.is_empty()).then(|| to_mesh(d));
    StudMeshes { body: wrap(out.body), top: wrap(out.top), glass: wrap(out.glass), offset: Vec3::from_array(out.offset) }
}

/// Keep the [`StudWorld`] index in step: the bridge's messages, unloaded
/// regions, and added / changed / removed stud chunk entities.
pub fn track_stud_world(
    mut world: ResMut<StudWorld>,
    mut msgs: MessageReader<StudWorldMsg>,
    roots: Option<Res<RegionRoots>>,
    changed: Query<(Entity, &SimEntity, &SimStudChunk), Changed<SimStudChunk>>,
    mut removed: RemovedComponents<SimStudChunk>,
) {
    for msg in msgs.read() {
        world.apply(msg);
    }
    if let Some(roots) = roots.filter(|r| r.is_changed()) {
        world.prune_regions(|r| roots.0.contains_key(r));
    }
    for e in removed.read() {
        world.remove_entity(e);
    }
    for (e, sim, c) in &changed {
        world.set_chunk(e, world_chunk(sim.region, c.coords), &c.chunk);
    }
}

/// Start mesh jobs for queued chunks, within the per-frame and in-flight budgets.
pub fn submit_stud_meshes(mut world: ResMut<StudWorld>, mut tasks: ResMut<StudMeshTasks>, chunks: Query<&SimStudChunk>) {
    if world.queued() == 0 {
        return;
    }
    let room = STUD_MESH_MAX_IN_FLIGHT.saturating_sub(tasks.0.len()).min(STUD_MESH_JOBS_PER_FRAME);
    if room == 0 {
        return;
    }
    for (key, entity, stamp) in world.take_jobs(room) {
        let Ok(c) = chunks.get(entity) else {
            // Despawned this frame: its removal reaches the index next frame.
            let _ = world.finish(key, entity, stamp);
            continue;
        };
        let task = crate::renderer::compute::spawn(world.job(key, &c.chunk), to_meshes);
        tasks.0.insert(key, InFlight { entity, stamp, task });
    }
}

/// Land finished jobs: at most [`STUD_MESH_UPLOADS_PER_FRAME`] meshes shown per
/// frame; the rest stay finished in their tasks until the next.
pub fn apply_stud_meshes(
    mut commands: Commands,
    mut world: ResMut<StudWorld>,
    mut tasks: ResMut<StudMeshTasks>,
    chunks: Query<Option<&StudMeshChildren>, With<SimStudChunk>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StudTerrainMaterial>>,
    mut material: ResMut<StudMaterials>,
) {
    if tasks.0.is_empty() {
        return;
    }
    let mut done: Vec<(WorldChunkPos, StudMeshes)> = Vec::new();
    for (key, flight) in tasks.0.iter_mut() {
        if done.len() >= STUD_MESH_UPLOADS_PER_FRAME {
            break;
        }
        if let Some(out) = block_on(future::poll_once(&mut flight.task)) {
            done.push((*key, out));
        }
    }
    for (key, StudMeshes { body, top, glass, .. }) in done {
        let Some(flight) = tasks.0.remove(&key) else { continue };
        if world.finish(key, flight.entity, flight.stamp) == Finished::Drop {
            continue;
        }
        let Ok(children) = chunks.get(flight.entity) else { continue };
        let e = flight.entity;
        // The viewer left while the job ran: its detail is not wanted any more.
        let top = top.filter(|_| world.wants_detail(key));
        let children = match children {
            Some(&ch) => ch,
            None => {
                let body = commands.spawn((StudBodyMesh, Transform::IDENTITY, Visibility::default(), ChildOf(e))).id();
                // Studs and seam lines are too small to shade the ground: no shadow pass.
                let top = commands
                    .spawn((StudTopMesh, Transform::IDENTITY, Visibility::default(), bevy::light::NotShadowCaster, ChildOf(e)))
                    .id();
                // Blended, so no shadow: a glass wall lets the sun through.
                let glass = commands
                    .spawn((StudGlassMesh, Transform::IDENTITY, Visibility::default(), bevy::light::NotShadowCaster, ChildOf(e)))
                    .id();
                let ch = StudMeshChildren { body, top, glass };
                commands.entity(e).insert(ch);
                ch
            }
        };
        let count = |m: &Option<Mesh>| m.as_ref().map_or((0, 0), |m| (m.indices().map_or(0, |i| i.len() / 3) as u32, m.count_vertices() as u32));
        let ((body_tris, body_verts), (detail_tris, detail_verts)) = (count(&body), count(&top));
        world.set_drawn(key, ChunkDrawn { body_tris, body_verts, detail_tris, detail_verts });
        // The cut material only in the home region (`material::retarget_stud_materials`
        // follows a home change).
        let opaque = material.opaque_in(key.to_local().0, &mut materials);
        let glass_mat = material.glass(&mut materials);
        for (child, mesh, mat) in [(children.body, body, &opaque), (children.top, top, &opaque), (children.glass, glass, &glass_mat)] {
            let mut ec = commands.entity(child);
            match mesh {
                Some(mesh) => {
                    ec.insert((Mesh3d(meshes.add(mesh)), MeshMaterial3d(mat.clone())));
                }
                None => {
                    ec.remove::<(Mesh3d, MeshMaterial3d<StudTerrainMaterial>)>();
                }
            }
        }
    }
    // One line per region, when its last first mesh has landed: what it draws.
    if world.regions_unreported() {
        let detail_set = world.detail_count();
        for (region, d) in world.region_meshed() {
            game::perf_event!(
                "stud_region_meshed",
                "region={},{} chunks={} body_meshes={} body_tris={} body_verts={} detail_meshes={} detail_tris={} detail_verts={} detail_set={}",
                region.x,
                region.z,
                d.chunks,
                d.body_meshes,
                d.body_tris,
                d.body_verts,
                d.detail_meshes,
                d.detail_tris,
                d.detail_verts,
                detail_set
            );
        }
    }
}

/// Where stud tops are drawn around: `(centre, radius)` in world sim units —
/// the eye in first person (`first_person_eye`), else the commander camera's focus.
pub fn stud_tops_centre(first_person_eye: Option<Vec3>, focus: Option<Vec3>) -> Option<(Vec3, f32)> {
    match first_person_eye {
        Some(eye) => Some((eye, STUD_TOPS_FIRST_PERSON_RADIUS_M * UNITS_PER_METER)),
        None => focus.map(|f| (f, STUD_TOPS_RADIUS_M * UNITS_PER_METER)),
    }
}

/// Whether the stud tops of the chunk whose min corner is at `origin` (world
/// sim units) are drawn around `centre`.
pub fn stud_tops_near(origin: Vec3, centre: Option<(Vec3, f32)>) -> bool {
    let half = Vec3::new(CHUNK_UNITS as f32, STUD_CHUNK_HEIGHT_UNITS as f32, CHUNK_UNITS as f32) * 0.5;
    centre.is_some_and(|(c, radius)| (origin + half).distance(c) <= radius)
}

/// Keep the detail set around the viewer ([`stud_tops_centre`]): the local body's
/// eyes while first person is on (the body carrying the render camera,
/// `first_person::sync_first_person_camera`), else the commander camera's focus.
/// Looked at again only when the viewer has moved
/// (`world::STUD_DETAIL_STEP_UNITS`); chunks that come into reach are queued for
/// a job with detail, the detail meshes of those that left are dropped here.
pub fn stud_detail(
    mut commands: Commands,
    cam: Option<Res<CommanderCamera>>,
    first_person: Option<Res<FirstPerson>>,
    eyes: Query<&GlobalTransform, (With<LocalBody>, With<Camera3d>)>,
    mut world: ResMut<StudWorld>,
    chunks: Query<&StudMeshChildren>,
) {
    let focus = cam.map(|c| Vec3::new(c.focus.x, c.ground_y, c.focus.y));
    let eye = first_person.filter(|f| f.on).and_then(|_| eyes.iter().next()).map(|gt| gt.translation());
    let centre = stud_tops_centre(eye, focus);
    if !world.detail_stale(centre) {
        return;
    }
    for key in world.set_detail_centre(centre) {
        let Some(children) = world.entity_at(key).and_then(|e| chunks.get(e).ok()) else { continue };
        commands.entity(children.top).remove::<(Mesh3d, MeshMaterial3d<StudTerrainMaterial>)>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::stud::mesh::StudMeshJob;
    use game::{RegionCoords, StudBrick, StudChunk, StudChunkCoords};

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<StudWorld>()
            .init_resource::<StudMeshTasks>()
            .init_resource::<StudMaterials>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StudTerrainMaterial>>()
            .add_message::<StudWorldMsg>()
            .add_systems(Update, (track_stud_world, stud_detail, submit_stud_meshes, apply_stud_meshes).chain());
        app
    }

    fn key(n: u64) -> game::EntityKey {
        game::EntityKey::from(slotmapd::KeyData::from_ffi((1 << 32) | n))
    }

    fn spawn(app: &mut App, n: u64, coords: StudChunkCoords, chunk: StudChunk) -> Entity {
        let region = RegionCoords::new(0, 0);
        app.world_mut().spawn((SimEntity { region, key: key(n) }, SimStudChunk { coords, chunk })).id()
    }

    fn settle(app: &mut App) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            app.update();
            let idle = app.world().resource::<StudMeshTasks>().0.is_empty() && app.world().resource::<StudWorld>().queued() == 0;
            if idle {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("stud meshing did not settle");
    }

    fn body_tris(app: &App, chunk: Entity) -> usize {
        let Some(ch) = app.world().get::<StudMeshChildren>(chunk) else { return 0 };
        let Some(mesh) = app.world().get::<Mesh3d>(ch.body) else { return 0 };
        app.world().resource::<Assets<Mesh>>().get(&mesh.0).unwrap().indices().unwrap().len() / 3
    }

    /// Chunks are meshed through the job path, a neighbour's arrival re-culls
    /// the shared face, an edit lands, and a despawn leaves nothing behind.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn chunks_mesh_off_thread_and_recull_when_a_neighbour_changes() {
        bevy::tasks::AsyncComputeTaskPool::get_or_init(bevy::tasks::TaskPool::new);
        let mut app = app();
        // A filler filling the east half of chunk (0,0,0): its +X face is on the chunk edge.
        let half = StudChunk::new([StudBrick::filler([16, 0, 0], [16, 90, 32], 4)]).unwrap();
        let a = spawn(&mut app, 1, StudChunkCoords { x: 0, y: 0, z: 0 }, half.clone());
        settle(&mut app);
        assert_eq!(body_tris(&app, a), 12, "a lone box: six faces");
        // The chunk to the east, solid along the shared face: a's +X face goes.
        let wall = StudChunk::new([StudBrick::filler([0, 0, 0], [1, 90, 32], 4)]).unwrap();
        let b = spawn(&mut app, 2, StudChunkCoords { x: 1, y: 0, z: 0 }, wall);
        settle(&mut app);
        assert_eq!(body_tris(&app, a), 10);
        assert_eq!(body_tris(&app, b), 10, "and b's -X face");
        // An edit to a: emptied.
        app.world_mut().get_mut::<SimStudChunk>(a).unwrap().chunk = StudChunk::default();
        settle(&mut app);
        assert_eq!(body_tris(&app, a), 0);
        assert_eq!(body_tris(&app, b), 12, "b's face shows again");
        // b unloads.
        app.world_mut().entity_mut(b).despawn();
        settle(&mut app);
        assert_eq!(app.world().resource::<StudWorld>().chunk_count(), 1);
    }

    /// The detail mesh (studs and seam lines) is there only while the chunk is
    /// near the viewer: built when the camera comes, dropped — with no job — when
    /// it leaves; the body is the same either way.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn detail_is_meshed_near_the_viewer_and_dropped_when_it_leaves() {
        bevy::tasks::AsyncComputeTaskPool::get_or_init(bevy::tasks::TaskPool::new);
        let mut app = app();
        let part = game::stud_catalog::STUD_PARTS.iter().position(|p| p.number == "3001").unwrap() as u16;
        let chunk = StudChunk::new([StudBrick::part_at([4, 0, 4], part, 0, 4)]).unwrap();
        let e = spawn(&mut app, 1, StudChunkCoords { x: 0, y: 0, z: 0 }, chunk);
        let tris_of = |app: &App, child: Entity| {
            let mesh = app.world().get::<Mesh3d>(child)?;
            Some(app.world().resource::<Assets<Mesh>>().get(&mesh.0).unwrap().indices().unwrap().len() / 3)
        };
        // No viewer: a body of six merged quads, no detail.
        settle(&mut app);
        let ch = *app.world().get::<StudMeshChildren>(e).unwrap();
        assert_eq!(tris_of(&app, ch.body), Some(12));
        assert_eq!(tris_of(&app, ch.top), None);
        assert!(app.world().get::<bevy::light::NotShadowCaster>(ch.top).is_some(), "detail casts no shadow");
        assert!(app.world().get::<bevy::light::NotShadowCaster>(ch.body).is_none());
        // The commander camera over the chunk: one more job, now with studs and seams.
        let centre = Vec3::new(CHUNK_UNITS as f32, STUD_CHUNK_HEIGHT_UNITS as f32, CHUNK_UNITS as f32) * 0.5;
        let cam = |at: Vec3| CommanderCamera { focus: Vec2::new(at.x, at.z), ground_y: at.y, ..Default::default() };
        app.insert_resource(cam(centre));
        settle(&mut app);
        assert!(app.world().resource::<StudWorld>().wants_detail(WorldChunkPos::new(0, 0, 0)));
        assert_eq!(tris_of(&app, ch.body), Some(12), "the body does not change");
        let detail = tris_of(&app, ch.top).expect("a detail mesh");
        assert!(detail > 8, "eight studs and the seam lines: {detail}");
        // The camera leaves: the detail mesh goes at once, nothing is queued.
        let far = centre + Vec3::X * 4.0 * STUD_TOPS_RADIUS_M * UNITS_PER_METER;
        app.insert_resource(cam(far));
        app.update();
        assert_eq!(app.world().resource::<StudWorld>().queued(), 0);
        assert!(app.world().resource::<StudMeshTasks>().0.is_empty());
        assert_eq!(tris_of(&app, ch.top), None);
        assert_eq!(tris_of(&app, ch.body), Some(12));
    }

    #[test]
    fn stud_tops_follow_the_first_person_eye_else_the_commander_focus() {
        let focus = Vec3::new(1000.0, 50.0, 1000.0);
        let eye = Vec3::new(-4000.0, 80.0, 0.0);
        let half = Vec3::new(CHUNK_UNITS as f32, STUD_CHUNK_HEIGHT_UNITS as f32, CHUNK_UNITS as f32) * 0.5;
        // Commander: around the focus.
        let c = stud_tops_centre(None, Some(focus));
        assert_eq!(c, Some((focus, STUD_TOPS_RADIUS_M * UNITS_PER_METER)));
        assert!(stud_tops_near(focus - half, c));
        assert!(!stud_tops_near(eye - half, c));
        // First person: around the eye, wherever the commander focus was left.
        let c = stud_tops_centre(Some(eye), Some(focus));
        assert_eq!(c, Some((eye, STUD_TOPS_FIRST_PERSON_RADIUS_M * UNITS_PER_METER)));
        assert!(stud_tops_near(eye - half, c));
        assert!(!stud_tops_near(focus - half, c));
        let edge = STUD_TOPS_FIRST_PERSON_RADIUS_M * UNITS_PER_METER;
        assert!(stud_tops_near(eye - half + Vec3::X * (edge - 1.0), c));
        assert!(!stud_tops_near(eye - half + Vec3::X * (edge + 1.0), c));
        // Neither: nothing.
        assert!(!stud_tops_near(Vec3::ZERO, stud_tops_centre(None, None)));
    }

    #[test]
    fn the_job_input_is_what_the_index_holds() {
        let mut w = StudWorld::default();
        let k = WorldChunkPos::new(0, 0, 0);
        let c = StudChunk::new([StudBrick::filler([0, 0, 0], [1, 1, 1], 4)]).unwrap();
        w.set_chunk(Entity::from_raw_u32(1).unwrap(), k, &c);
        let StudMeshJob::Chunk { chunk, neighbours, detail } = w.job(k, &c) else { panic!("a chunk job") };
        assert!(!detail, "no viewer: no detail");
        assert_eq!(chunk, c);
        assert_eq!(neighbours, Default::default(), "nothing around it");
    }
}
