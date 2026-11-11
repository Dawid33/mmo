//! The editor session: getting into the editor world and laying the opened prefab.

use std::path::Path;

use bevy::prelude::*;
use game::{
    layout_build, plate_units, resolve, CellBox, Marker, PrefabCommand, PrefabDir, PrefabKindFile, PrefabKindTag,
    PrefabMeta, PrefabRef, ResolvedKind, ResolvedPrefab, Trust, WorldChunkPos, PREFAB_HALF_STUDS, STUD_CHUNK_PLATES,
    STUD_CHUNK_STUDS, STUD_UNITS,
};
use worldgen::prefab_world::editor_anchor;

use crate::renderer::commander::CommanderCamera;
use crate::renderer::stud::build::{StudBuild, StudEdits};
use crate::renderer::stud::world::StudWorld;
use crate::renderer::stud::SimStudChunk;
use crate::renderer::{GameEvents, LocalPlayer};
use crate::session::{world_systems_enabled, AppState, Embarked, PendingWorld, SessionResetExt};
use crate::world_scale::WorldScale;

/// `VOXEL_WORLD_FILE` value of the editor's world (the server's `PREFAB_EDITOR_KEYWORD`).
pub const EDITOR_WORLD_KEYWORD: &str = "prefab-editor";
/// `worlds/<this>/`: only the editor server's `server.log` lives there.
pub const EDITOR_ENTRY_SLUG: &str = "prefab-editor";
/// The editor world is 3 x 3 regions with nothing beyond them: its session plays with
/// this scale whatever the app's is (restored when the session ends).
pub const EDITOR_WORLD_SCALE: WorldScale = WorldScale { near_radius: 1, fortress_radius: 0, lod_table: &[] };
/// How often the opening stamp is sent again until its bricks show up (the sim
/// drops commands sent before it has caught up with the server).
const STAMP_RETRY_SECONDS: f32 = 0.75;

const ANCHOR_COLOR: Color = Color::srgb(1.0, 0.85, 0.1);
const EXTENT_COLOR: Color = Color::srgba(1.0, 1.0, 1.0, 0.35);
const CARVE_COLOR: Color = Color::srgb(1.0, 0.3, 0.2);
const MARKER_COLOR: Color = Color::srgb(0.2, 0.9, 1.0);

/// Which folder a prefab lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefabSource {
    /// The player's library: `<data dir>/prefabs/`.
    Library,
    /// The game's own `assets/prefabs/` on disk. Dev builds only.
    Game,
}

/// The prefab the next session edits, set by [`start_editor`]; consumed when the
/// session is in the world (and dropped if the start fails).
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct EditorRequest {
    pub source: PrefabSource,
    pub name: String,
    /// The kind of the new prefab when no file called `name` exists (`None`: Build).
    pub new_kind: Option<PrefabKindTag>,
}

/// The prefab being edited. Exists only during an editor session.
#[derive(Resource, Debug)]
pub struct EditorSession {
    pub source: PrefabSource,
    pub name: String,
    pub kind: PrefabKindTag,
    /// Carve boxes and markers of the opened file: kept and shown, written back on save.
    pub kept_carve: Vec<CellBox>,
    pub kept_markers: Vec<Marker>,
    pub meta: PrefabMeta,
    /// The opened prefab's bricks are in the world (at once for a new prefab).
    pub loaded: bool,
    /// Bricks were placed or removed since the last save.
    pub dirty: bool,
    /// The last thing worth telling the author (a load or save result).
    pub message: Option<String>,
    /// Parts and boxes a save would write now (refreshed while editing: `save::count_elements`).
    pub parts: usize,
    pub boxes: usize,
    /// The prefab still to be laid into the world.
    to_stamp: Option<ResolvedPrefab>,
    /// Seconds until the opening stamp is sent (again).
    stamp_in: f32,
    /// The camera has been moved over the anchor.
    camera_set: bool,
    /// `StudEdits` as last seen.
    pub(super) edits_seen: u64,
    /// The edit count the element counts were last taken at, and the one they are
    /// settled for.
    pub(super) counted_at: Option<u64>,
    pub(super) counted: Option<u64>,
}

/// The app's own world scale while an editor session overrides it.
#[derive(Resource, Clone, Copy)]
struct SavedWorldScale(WorldScale);

/// The folder of `source`: the library under the client's data dir (`None` without
/// one), the game's prefabs in the repository (`None` outside dev builds).
pub fn prefab_dir(source: PrefabSource) -> Option<PrefabDir> {
    match source {
        PrefabSource::Library => crate::player_name::data_dir().map(|d| PrefabDir::new(d.join("prefabs"))),
        PrefabSource::Game if cfg!(feature = "dev_ui") => {
            Some(PrefabDir::new(crate::renderer::resolve_blocks_dir().join("..").join("prefabs")))
        }
        PrefabSource::Game => None,
    }
}

/// Starts an editor session on `request`: the editor world as the pending world, the
/// request kept for the session, `AppState::Starting`. An error (nothing started)
/// says what is missing.
pub fn start_editor(
    commands: &mut Commands,
    next: &mut NextState<AppState>,
    request: EditorRequest,
) -> Result<(), String> {
    if !game::prefab_name_ok(&request.name) {
        return Err(format!("bad prefab name {:?}", request.name));
    }
    if prefab_dir(request.source).is_none() {
        return Err(match request.source {
            PrefabSource::Library => "no data directory for the prefab library (set HOME or XDG_DATA_HOME)".into(),
            PrefabSource::Game => "the game's own prefabs can only be edited in a dev build".into(),
        });
    }
    let dir = worldgen::forged::from_repo_root(Path::new(worldgen::world_store::WORLDS_DIR)).join(EDITOR_ENTRY_SLUG);
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    commands.insert_resource(PendingWorld {
        entry_dir: dir,
        terrain: EDITOR_WORLD_KEYWORD.into(),
        name: format!("Prefab editor: {}", request.name),
    });
    commands.insert_resource(request);
    // The session's sim is told the world scale when it connects.
    commands.queue(|world: &mut World| {
        if !world.contains_resource::<SavedWorldScale>() {
            if let Some(scale) = world.get_resource::<WorldScale>().copied() {
                world.insert_resource(SavedWorldScale(scale));
            }
        }
        world.insert_resource(EDITOR_WORLD_SCALE);
    });
    next.set(AppState::Starting);
    Ok(())
}

/// The session ended (or its start failed): nothing of the editor stays behind.
fn reset_editor(world: &mut World) {
    world.remove_resource::<EditorRequest>();
    world.remove_resource::<EditorSession>();
    if let Some(saved) = world.remove_resource::<SavedWorldScale>() {
        world.insert_resource(saved.0);
    }
}

/// What opening `request` gives: the session, with the prefab to lay (if any).
fn open(request: &EditorRequest) -> EditorSession {
    let mut session = EditorSession {
        source: request.source,
        name: request.name.clone(),
        kind: request.new_kind.unwrap_or(PrefabKindTag::Build),
        kept_carve: Vec::new(),
        kept_markers: Vec::new(),
        meta: PrefabMeta::default(),
        loaded: true,
        dirty: false,
        message: None,
        parts: 0,
        boxes: 0,
        to_stamp: None,
        stamp_in: 0.0,
        camera_set: false,
        edits_seen: 0,
        counted_at: None,
        counted: None,
    };
    let Some(dir) = prefab_dir(request.source) else { return session };
    if !dir.exists(&request.name) {
        session.message = Some(format!("New prefab {:?}", request.name));
        return session;
    }
    let file = match dir.load(&request.name) {
        Ok(file) => file,
        Err(e) => {
            session.message = Some(format!("Could not open {:?}: {e}", request.name));
            return session;
        }
    };
    session.kind = file.kind_tag();
    session.meta = file.meta.clone();
    let PrefabKindFile::Build(build) = &file.kind else {
        session.message = Some("Prop prefabs cannot be edited yet".into());
        return session;
    };
    session.kept_carve = build.carve.clone();
    session.kept_markers = build.markers.clone();
    // The game's own prefabs may tag bricks with `data`; a registered prefab may not,
    // and the world's bricks are what a save reads back — so such a prefab is not
    // opened rather than opened without its tags.
    let trust = if request.source == PrefabSource::Game { Trust::Builtin } else { Trust::Player };
    match resolve(&file, trust) {
        Ok(prefab) if prefab.validate(Trust::Player).is_err() => {
            session.message = Some("This prefab tags bricks with data, which the editor cannot keep yet".into());
        }
        Ok(prefab) => {
            session.loaded = false;
            session.to_stamp = Some(prefab);
        }
        Err(problems) => {
            let list: Vec<String> = problems.iter().map(ToString::to_string).collect();
            session.message = Some(format!("Could not open {:?}: {}", request.name, list.join("; ")));
        }
    }
    session
}

/// In the world with a request: the editor session begins. The editor world has no
/// world map, so connected is embarked; build mode is on from the start.
fn enter_editor(
    mut commands: Commands,
    request: Option<Res<EditorRequest>>,
    state: Res<State<AppState>>,
    mut embarked: ResMut<Embarked>,
    mut build: ResMut<StudBuild>,
    mut cam: Option<ResMut<CommanderCamera>>,
) {
    let Some(request) = request else { return };
    if *state.get() != AppState::InWorld {
        return;
    }
    let session = open(&request);
    info!(
        "[prefab-editor] editing {:?} ({:?}, {:?}){}",
        session.name,
        session.source,
        session.kind,
        session.message.as_deref().map(|m| format!(": {m}")).unwrap_or_default()
    );
    commands.remove_resource::<EditorRequest>();
    commands.insert_resource(session);
    embarked.0 = true;
    build.on = true;
    if let Some(cam) = cam.as_mut() {
        cam.placed = false;
    }
}

/// The world cell whose brick shows the opening stamp has landed: the first brick the
/// layout lays (`None`: the prefab lays no brick).
fn witness_cell(prefab: &ResolvedPrefab) -> Option<(WorldChunkPos, [u8; 3])> {
    let ResolvedKind::Build(build) = &prefab.kind;
    let layout = layout_build(build, editor_anchor(), 0);
    layout.chunks.iter().find_map(|(c, s)| s.bricks.first().map(|b| (*c, b.min)))
}

/// Whether the ground around the anchor has arrived: the recipes of the chunk columns
/// under the prefab bounds.
pub(super) fn ground_arrived(world: &StudWorld) -> bool {
    let (a, n, half) = (editor_anchor(), STUD_CHUNK_STUDS as i64, i64::from(PREFAB_HALF_STUDS));
    let (x0, x1) = ((a[0] - half).div_euclid(n), (a[0] + half - 1).div_euclid(n));
    let (z0, z1) = ((a[2] - half).div_euclid(n), (a[2] + half - 1).div_euclid(n));
    (x0..=x1).all(|x| (z0..=z1).all(|z| world.recipe(x, z).is_some()))
}

/// Lays the opened prefab: once the ground is there, registers it, stamps it at the
/// anchor with no turn and deregisters it — again every [`STAMP_RETRY_SECONDS`] until
/// its first brick is in the world (all three commands are idempotent).
fn load_into_world(
    time: Res<Time>,
    session: Option<ResMut<EditorSession>>,
    world: Res<StudWorld>,
    chunks: Query<&SimStudChunk>,
    local: Res<LocalPlayer>,
    events: Res<GameEvents>,
) {
    let Some(mut session) = session else { return };
    if session.loaded {
        return;
    }
    let Some(prefab) = session.to_stamp.clone() else {
        session.loaded = true;
        return;
    };
    let witness = witness_cell(&prefab);
    let landed = witness.is_some_and(|(chunk, cell)| {
        world.entity_at(chunk).and_then(|e| chunks.get(e).ok()).is_some_and(|c| c.chunk.brick_at(cell).is_some())
    });
    if landed {
        session.loaded = true;
        session.to_stamp = None;
        return;
    }
    let Some(client) = local.0 else { return };
    if !ground_arrived(&world) {
        return;
    }
    session.stamp_in -= time.delta_secs();
    if session.stamp_in > 0.0 {
        return;
    }
    session.stamp_in = STAMP_RETRY_SECONDS;
    let key = prefab.key();
    let send = |command| {
        let _ = events.0.send(game::GameEventKind::Prefab(client, command));
    };
    send(PrefabCommand::Register(Box::new(prefab)));
    send(PrefabCommand::Stamp { prefab: PrefabRef::Registered(key), anchor: editor_anchor(), turn: 0 });
    send(PrefabCommand::Deregister(key));
    if witness.is_none() {
        // Nothing to see land (no bricks): sent once is all that can be known.
        session.loaded = true;
        session.to_stamp = None;
    }
}

/// The camera starts over the body at the spawn point: move it over the anchor once.
fn focus_camera_on_anchor(session: Option<ResMut<EditorSession>>, cam: Option<ResMut<CommanderCamera>>) {
    let (Some(mut session), Some(mut cam)) = (session, cam) else { return };
    if session.camera_set || !cam.placed {
        return;
    }
    let (a, s) = (editor_anchor(), STUD_UNITS as f32);
    cam.focus = Vec2::new((a[0] as f32 + 0.5) * s, (a[2] as f32 + 0.5) * s);
    session.camera_set = true;
}

/// Any brick placed or removed in build mode makes the prefab unsaved.
fn mark_dirty(session: Option<ResMut<EditorSession>>, edits: Res<StudEdits>) {
    let Some(mut session) = session else { return };
    if edits.0 != session.edits_seen {
        session.edits_seen = edits.0;
        session.dirty = true;
    }
}

/// The sim-unit box of the prefab cell box `min .. min + size` (cells relative to the anchor).
fn cell_box_transform(min: [i32; 3], size: [u32; 3]) -> Transform {
    let (a, s) = (editor_anchor(), STUD_UNITS as f32);
    let lo = Vec3::new(
        (a[0] + i64::from(min[0])) as f32 * s,
        plate_units((a[1] + i64::from(min[1])) as i32),
        (a[2] + i64::from(min[2])) as f32 * s,
    );
    let extent = Vec3::new(size[0] as f32 * s, plate_units(size[1] as i32), size[2] as f32 * s);
    Transform::from_translation(lo + extent / 2.0).with_scale(extent)
}

/// Outlines, as gizmos (no entities to leak): the anchor cell on the baseplate, the
/// bounds a prefab must stay in, and the opened file's carve boxes and markers.
fn draw_editor_guides(session: Option<Res<EditorSession>>, mut gizmos: Gizmos) {
    let Some(session) = session else { return };
    let half = PREFAB_HALF_STUDS;
    // The anchor: the cell itself, one plate high, and a post so it is found from afar.
    gizmos.cube(cell_box_transform([0, 0, 0], [1, 1, 1]), ANCHOR_COLOR);
    gizmos.cube(cell_box_transform([0, 0, 0], [1, STUD_CHUNK_PLATES as u32 / 6, 1]), ANCHOR_COLOR.with_alpha(0.4));
    // The bounds, as a square on the baseplate.
    gizmos.cube(cell_box_transform([-half, 0, -half], [2 * half as u32, 0, 2 * half as u32]), EXTENT_COLOR);
    for c in &session.kept_carve {
        gizmos.cube(cell_box_transform(c.min, c.size), CARVE_COLOR);
    }
    for m in &session.kept_markers {
        // A cross in the marker's cell, with a tick toward the way it faces.
        let cell = cell_box_transform(m.at, [1, 3, 1]);
        let (c, r) = (cell.translation, cell.scale.x / 2.0);
        gizmos.line(c - Vec3::X * r, c + Vec3::X * r, MARKER_COLOR);
        gizmos.line(c - Vec3::Z * r, c + Vec3::Z * r, MARKER_COLOR);
        gizmos.line(c - Vec3::Y * cell.scale.y / 2.0, c + Vec3::Y * cell.scale.y / 2.0, MARKER_COLOR);
        let t = game::turn_cell([1, 0, 0], m.turn);
        gizmos.line(c, c + Vec3::new(t[0] as f32, 0.0, t[2] as f32) * 2.0 * r, MARKER_COLOR);
    }
}

pub struct PrefabEditorPlugin;

impl Plugin for PrefabEditorPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(super::library_ui::PrefabLibraryPlugin)
            .add_systems(
                Update,
                (super::panel::apply_editor_ui_actions, super::panel::sync_leave_guard, super::panel::sync_editor_panel)
                    .chain()
                    .after(super::save::save_editor)
                    .run_if(world_systems_enabled()),
            )
            .add_session_reset_with(reset_editor)
            .add_message::<super::save::SaveRequested>()
            .add_session_message_clear::<super::save::SaveRequested>()
            .add_systems(
                Update,
                (
                    enter_editor,
                    load_into_world,
                    focus_camera_on_anchor,
                    mark_dirty,
                    super::save::request_save_on_ctrl_s,
                    super::save::save_editor,
                    super::save::count_elements,
                    draw_editor_guides,
                )
                    .chain()
                    .run_if(world_systems_enabled()),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_witness_is_the_first_brick_the_stamp_lays() {
        let file = game::prefab_from_ron(include_str!("../../../../assets/prefabs/dev-arch.prefab.ron")).unwrap();
        let prefab = resolve(&file, Trust::Player).unwrap();
        let (chunk, cell) = witness_cell(&prefab).expect("the arch lays bricks");
        let a = editor_anchor();
        let n = STUD_CHUNK_STUDS as i64;
        // The first post stands on the anchor cell.
        assert_eq!((chunk.x, chunk.z), (a[0].div_euclid(n), a[2].div_euclid(n)));
        assert_eq!(cell, [a[0].rem_euclid(n) as u8, a[1].rem_euclid(STUD_CHUNK_PLATES as i64) as u8, a[2].rem_euclid(n) as u8]);
        // A prefab without bricks has none.
        let mut empty = prefab.clone();
        let ResolvedKind::Build(b) = &mut empty.kind;
        b.parts.clear();
        assert_eq!(witness_cell(&empty), None);
    }

    #[test]
    fn the_guides_box_a_cell_in_sim_units() {
        let a = editor_anchor();
        let t = cell_box_transform([0, 0, 0], [1, 3, 1]);
        let s = STUD_UNITS as f32;
        assert_eq!(t.scale, Vec3::new(s, plate_units(3), s));
        assert_eq!(t.translation.x, (a[0] as f32 + 0.5) * s);
        assert_eq!(t.translation.y, plate_units(a[1] as i32) + plate_units(3) / 2.0);
    }
}
