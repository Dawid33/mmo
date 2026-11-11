//! Saving from the editor: the world's bricks around the anchor, read back into the
//! opened prefab's file (`game::capture_build`).

use bevy::prelude::*;
use game::{
    capture_build, resolve, BuildFile, PrefabFile, PrefabKindFile, PrefabKindTag, StudChunk, Trust, WorldChunkPos,
    PREFAB_FORMAT,
};
use worldgen::prefab_world::editor_anchor;

use super::session::{ground_arrived, prefab_dir, EditorSession, PrefabSource};
use crate::renderer::stud::world::StudWorld;
use crate::renderer::stud::SimStudChunk;

/// Save the prefab being edited (Ctrl+S, the panel's Save button).
#[derive(Message, Clone, Copy, Debug, Default)]
pub struct SaveRequested;

/// How often the panel's part and box counts are refreshed while there are unsaved edits.
const COUNT_SECONDS: f32 = 1.0;

/// The prefab as it stands in the world: `Err` says why it cannot be read yet.
fn capture(session: &EditorSession, world: &StudWorld, chunks: &Query<&SimStudChunk>) -> Result<BuildFile, String> {
    if session.kind != PrefabKindTag::Build {
        return Err("prop prefabs cannot be saved yet".into());
    }
    if !session.loaded || !ground_arrived(world) {
        return Err("the world is still loading".into());
    }
    let chunk = |pos: WorldChunkPos| -> Option<StudChunk> {
        world.entity_at(pos).and_then(|e| chunks.get(e).ok()).map(|c| c.chunk.clone())
    };
    let pristine = |pos: WorldChunkPos| world.recipe(pos.x, pos.z).and_then(|g| g.pristine_chunk(pos.y));
    capture_build(chunk, pristine, editor_anchor(), session.kept_carve.clone(), session.kept_markers.clone())
        .map_err(|e| e.to_string())
}

/// The file a save writes: the session's name and meta around the captured build,
/// checked as its folder's prefabs are (a library prefab is a player's).
fn file_of(session: &EditorSession, build: BuildFile) -> Result<PrefabFile, String> {
    let file = PrefabFile {
        format: PREFAB_FORMAT,
        name: session.name.clone(),
        meta: session.meta.clone(),
        kind: PrefabKindFile::Build(build),
    };
    let trust = if session.source == PrefabSource::Game { Trust::Builtin } else { Trust::Player };
    resolve(&file, trust).map_err(|problems| {
        let list: Vec<String> = problems.iter().map(ToString::to_string).collect();
        list.join("; ")
    })?;
    Ok(file)
}

/// Ctrl+S asks for a save.
pub fn request_save_on_ctrl_s(
    keys: Res<ButtonInput<KeyCode>>,
    session: Option<Res<EditorSession>>,
    mut requests: MessageWriter<SaveRequested>,
) {
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    if session.is_some() && ctrl && keys.just_pressed(KeyCode::KeyS) {
        requests.write(SaveRequested);
    }
}

/// On a save request: capture, check, write. The outcome goes to the session's
/// message; a refused save writes nothing.
pub fn save_editor(
    mut requests: MessageReader<SaveRequested>,
    session: Option<ResMut<EditorSession>>,
    world: Res<StudWorld>,
    chunks: Query<&SimStudChunk>,
) {
    if requests.read().last().is_none() {
        return;
    }
    let Some(mut session) = session else { return };
    let saved = capture(&session, &world, &chunks).and_then(|build| file_of(&session, build)).and_then(|file| {
        let dir = prefab_dir(session.source).ok_or("no folder to save into")?;
        dir.save(&file).map_err(|e| e.to_string())?;
        Ok((file, dir.path_of(&session.name)))
    });
    match saved {
        Ok((file, path)) => {
            let PrefabKindFile::Build(build) = &file.kind else { unreachable!("a captured prefab is a Build") };
            session.parts = build.parts.len();
            session.boxes = build.boxes.len();
            session.dirty = false;
            info!("[prefab-editor] saved {}", path.display());
            session.message = Some(format!("Saved {:?}: {} parts, {} boxes", session.name, session.parts, session.boxes));
        }
        Err(why) => {
            warn!("[prefab-editor] {:?} not saved: {why}", session.name);
            session.message = Some(format!("Not saved: {why}"));
        }
    }
}

/// Keeps the session's part and box counts current while the prefab is being edited
/// (and right after it has loaded), at most every [`COUNT_SECONDS`].
pub fn count_elements(
    time: Res<Time>,
    session: Option<ResMut<EditorSession>>,
    world: Res<StudWorld>,
    chunks: Query<&SimStudChunk>,
    mut wait: crate::session::SessionLocal<f32>,
) {
    let Some(mut session) = session else { return };
    let wait = wait.get();
    *wait -= time.delta_secs();
    if *wait > 0.0 || !session.loaded || session.counted == Some(session.edits_seen) {
        return;
    }
    *wait = COUNT_SECONDS;
    if let Ok(build) = capture(&session, &world, &chunks) {
        (session.parts, session.boxes) = (build.parts.len(), build.boxes.len());
        // An edit's bricks arrive a moment after its order: count once more after
        // the edits stop.
        session.counted = (session.counted_at == Some(session.edits_seen)).then_some(session.edits_seen);
        session.counted_at = Some(session.edits_seen);
    }
}
