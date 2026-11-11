//! The prefab library screen (`ui/prefab-library.slint`, screen 7 of the frontend):
//! the player's prefabs and, in dev builds, the game's own; new, open, duplicate,
//! delete, import from and export to the clipboard.
//!
//! The folders are read on entering the screen and after every action that changes
//! them. Everything decided here is in [`apply_action`] (no UI, no Bevy), which the
//! screen's systems only feed and show.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::prelude::*;
use game::{PrefabDir, PrefabEntry, PrefabKindTag, PrefabListing};
use slint_ui::slint::{Model, ModelRc, SharedString, VecModel};
use slint_ui::FrontendUi;

use super::session::{prefab_dir, start_editor, EditorRequest, PrefabSource};
use crate::renderer::slint_overlay::SlintLayer;
use crate::session::{AppState, LastSessionError};

/// What `client debug prefab` and New build call a prefab nobody named.
pub const UNTITLED: &str = "untitled";

/// What the player did on the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryAction {
    PickMine(usize),
    PickGame(usize),
    Open,
    NewBuild,
    Duplicate,
    Delete,
    ConfirmDeleteYes,
    ConfirmDeleteNo,
    Import,
    Export,
}

/// The screen's state.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct LibraryState {
    pub mine: PrefabListing,
    pub game: PrefabListing,
    /// The selected prefab: which list, and its row there.
    pub selected: Option<(PrefabSource, usize)>,
    /// The last action's result ("" = nothing to say).
    pub status: String,
    /// Delete was pressed: the question is up.
    pub confirm_delete: bool,
}

/// After the editor session a library start led to: back to this screen, not the menu.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReturnToLibrary(pub bool);

/// The clipboard, as the screen needs it (a fake in tests).
pub trait Clipboard {
    fn get(&mut self) -> Result<String, String>;
    fn set(&mut self, text: &str) -> Result<(), String>;
}

/// The system clipboard.
pub struct SystemClipboard;

impl Clipboard for SystemClipboard {
    fn get(&mut self) -> Result<String, String> {
        arboard::Clipboard::new().and_then(|mut c| c.get_text()).map_err(|e| format!("clipboard: {e}"))
    }

    fn set(&mut self, text: &str) -> Result<(), String> {
        arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string())).map_err(|e| format!("clipboard: {e}"))
    }
}

/// One list row: "dev-arch — Build · 3 elements · 8 x 6 x 4".
pub fn row_text(e: &PrefabEntry) -> String {
    let kind = match e.kind {
        PrefabKindTag::Build => "Build",
        PrefabKindTag::Prop => "Prop",
    };
    let elements = if e.elements == 1 { "1 element".to_string() } else { format!("{} elements", e.elements) };
    format!("{} \u{2014} {kind} \u{b7} {elements} \u{b7} {} x {} x {}", e.name, e.size[0], e.size[1], e.size[2])
}

impl LibraryState {
    /// Reads both folders again; the selection follows its prefab by name.
    pub fn refresh(&mut self, dirs: &dyn Fn(PrefabSource) -> Option<PrefabDir>) {
        let selected = self.selected_entry().map(|(source, e)| (source, e.name.clone()));
        self.mine = dirs(PrefabSource::Library).map(|d| d.list()).unwrap_or_default();
        self.game = dirs(PrefabSource::Game).map(|d| d.list()).unwrap_or_default();
        self.selected = selected.and_then(|(source, name)| self.find(source, &name));
    }

    fn list(&self, source: PrefabSource) -> &PrefabListing {
        match source {
            PrefabSource::Library => &self.mine,
            PrefabSource::Game => &self.game,
        }
    }

    fn find(&self, source: PrefabSource, name: &str) -> Option<(PrefabSource, usize)> {
        self.list(source).entries.iter().position(|e| e.name == name).map(|i| (source, i))
    }

    pub fn selected_entry(&self) -> Option<(PrefabSource, &PrefabEntry)> {
        let (source, i) = self.selected?;
        self.list(source).entries.get(i).map(|e| (source, e))
    }

    /// The selected prefab of the player's own library (what Duplicate and Delete act on).
    fn selected_mine(&self) -> Option<&PrefabEntry> {
        self.selected_entry().filter(|(source, _)| *source == PrefabSource::Library).map(|(_, e)| e)
    }

    /// The status lines: the last result, then every file that is not a prefab.
    pub fn status_lines(&self) -> Vec<String> {
        let bad = |listing: &PrefabListing| -> Vec<String> {
            listing.bad.iter().map(|(file, why)| format!("{file}: {why}")).collect()
        };
        let mut lines = vec![self.status.clone()];
        lines.extend(bad(&self.mine));
        lines.extend(bad(&self.game));
        lines
    }
}

/// Applies one action to the screen's state and the folders. Returns the prefab to
/// edit when the action opens the editor. `new_name` is the name field's text.
pub fn apply_action(
    state: &mut LibraryState,
    action: LibraryAction,
    new_name: &str,
    dirs: &dyn Fn(PrefabSource) -> Option<PrefabDir>,
    clipboard: &mut dyn Clipboard,
) -> Option<EditorRequest> {
    let library = dirs(PrefabSource::Library);
    let no_library = || "No data directory for the prefab library (set HOME or XDG_DATA_HOME)".to_string();
    let mut edit = None;
    match action {
        LibraryAction::PickMine(i) => state.selected = (i < state.mine.entries.len()).then_some((PrefabSource::Library, i)),
        LibraryAction::PickGame(i) => state.selected = (i < state.game.entries.len()).then_some((PrefabSource::Game, i)),
        LibraryAction::Open => match state.selected_entry() {
            Some((_, e)) if e.kind == PrefabKindTag::Prop => state.status = "Prop prefabs cannot be edited yet".into(),
            Some((source, e)) => edit = Some(EditorRequest { source, name: e.name.clone(), new_kind: None }),
            None => {}
        },
        LibraryAction::NewBuild => {
            let typed = new_name.trim();
            let base = if typed.is_empty() { UNTITLED } else { typed };
            match &library {
                None => state.status = no_library(),
                Some(_) if !game::prefab_name_ok(base) => {
                    state.status = format!("{base:?} is not a prefab name: a-z, 0-9 and '-', at most 48");
                }
                Some(dir) => {
                    let request = EditorRequest {
                        source: PrefabSource::Library,
                        name: dir.unused_name(base),
                        new_kind: Some(PrefabKindTag::Build),
                    };
                    edit = Some(request);
                }
            }
        }
        LibraryAction::Duplicate => {
            if let (Some(dir), Some(name)) = (&library, state.selected_mine().map(|e| e.name.clone())) {
                state.status = match dir.duplicate(&name) {
                    Ok(copy) => format!("Duplicated {name:?} as {copy:?}"),
                    Err(e) => format!("Could not duplicate {name:?}: {e}"),
                };
            }
        }
        LibraryAction::Delete => state.confirm_delete = state.selected_mine().is_some(),
        LibraryAction::ConfirmDeleteNo => state.confirm_delete = false,
        LibraryAction::ConfirmDeleteYes => {
            if let (true, Some(dir), Some(name)) =
                (state.confirm_delete, &library, state.selected_mine().map(|e| e.name.clone()))
            {
                state.status = match dir.delete(&name) {
                    Ok(()) => format!("Deleted {name:?}"),
                    Err(e) => format!("Could not delete {name:?}: {e}"),
                };
                state.selected = None;
            }
            state.confirm_delete = false;
        }
        LibraryAction::Import => {
            state.status = match &library {
                None => no_library(),
                Some(dir) => match clipboard.get().and_then(|text| dir.import(&text).map_err(|e| e.to_string())) {
                    Ok(name) => format!("Imported {name:?}"),
                    Err(e) => format!("Not imported: {e}"),
                },
            };
        }
        LibraryAction::Export => {
            if let Some((source, name)) = state.selected_entry().map(|(source, e)| (source, e.name.clone())) {
                let exported = dirs(source)
                    .ok_or_else(|| "no folder".to_string())
                    .and_then(|dir| dir.export(&name).map_err(|e| e.to_string()))
                    .and_then(|text| clipboard.set(&text));
                state.status = match exported {
                    Ok(()) => format!("Copied {name:?} to the clipboard"),
                    Err(e) => format!("Not exported: {e}"),
                };
            }
        }
    }
    // The folders may have changed under any of these.
    if matches!(action, LibraryAction::Duplicate | LibraryAction::ConfirmDeleteYes | LibraryAction::Import) {
        state.refresh(dirs);
    }
    edit
}

/// The screen's queued callbacks (main thread, like the UI itself).
pub struct LibraryUi(Rc<RefCell<Vec<LibraryAction>>>);

/// Connect the screen's callbacks. Called on every (re)built `FrontendUi`.
pub fn connect(ui: &FrontendUi, world: &mut World) {
    let actions = Rc::new(RefCell::new(Vec::new()));
    let push = |action: LibraryAction| {
        let queue = actions.clone();
        move || queue.borrow_mut().push(action)
    };
    let pick = |make: fn(usize) -> LibraryAction| {
        let queue = actions.clone();
        move |i: i32| queue.borrow_mut().push(make(i.max(0) as usize))
    };
    ui.on_prefab_pick_mine(pick(LibraryAction::PickMine));
    ui.on_prefab_pick_game(pick(LibraryAction::PickGame));
    ui.on_prefab_open(push(LibraryAction::Open));
    ui.on_prefab_new_build(push(LibraryAction::NewBuild));
    ui.on_prefab_duplicate(push(LibraryAction::Duplicate));
    ui.on_prefab_delete(push(LibraryAction::Delete));
    ui.on_prefab_confirm_delete_yes(push(LibraryAction::ConfirmDeleteYes));
    ui.on_prefab_confirm_delete_no(push(LibraryAction::ConfirmDeleteNo));
    ui.on_prefab_import(push(LibraryAction::Import));
    ui.on_prefab_export(push(LibraryAction::Export));
    ui.set_prefab_show_game(cfg!(feature = "dev_ui"));
    world.insert_non_send(LibraryUi(actions));
}

/// Entering the screen: read the folders.
fn refresh_library(mut state: ResMut<LibraryState>) {
    state.refresh(&prefab_dir);
    state.confirm_delete = false;
}

/// An editor session started here ends here (unless it was lost: the menu shows why).
fn return_to_library(
    mut back: ResMut<ReturnToLibrary>,
    error: Res<LastSessionError>,
    mut next: ResMut<NextState<AppState>>,
) {
    if std::mem::take(&mut back.0) && error.0.is_none() {
        next.set(AppState::PrefabLibrary);
    }
}

fn apply_library_actions(
    ui: Option<NonSend<LibraryUi>>,
    layer: Option<NonSend<SlintLayer<FrontendUi>>>,
    app_state: Res<State<AppState>>,
    mut state: ResMut<LibraryState>,
    mut back: ResMut<ReturnToLibrary>,
    mut commands: Commands,
    mut next: ResMut<NextState<AppState>>,
) {
    let Some(ui) = ui else { return };
    let queued: Vec<LibraryAction> = ui.0.borrow_mut().drain(..).collect();
    if *app_state.get() != AppState::PrefabLibrary {
        return;
    }
    let new_name = layer.as_ref().map(|l| l.overlay.ui().get_prefab_new_name().to_string()).unwrap_or_default();
    for action in queued {
        let Some(request) = apply_action(&mut state, action, &new_name, &prefab_dir, &mut SystemClipboard) else {
            continue;
        };
        match start_editor(&mut commands, &mut next, request) {
            Ok(()) => back.0 = true,
            Err(e) => state.status = e,
        }
        // The session is starting: whatever else was clicked this frame is dropped.
        break;
    }
}

/// The state → the screen, only when it changed.
fn push_library_state(
    layer: Option<NonSend<SlintLayer<FrontendUi>>>,
    state: Res<LibraryState>,
    mut shown: Local<Option<LibraryState>>,
) {
    let Some(layer) = layer else { return };
    let ui = layer.overlay.ui();
    // A rebuilt UI has empty lists: push again.
    let lost = !state.mine.entries.is_empty() && ui.get_prefab_mine().row_count() == 0;
    if shown.as_ref() == Some(&*state) && !lost {
        return;
    }
    let strings = |lines: Vec<String>| -> ModelRc<SharedString> {
        ModelRc::new(VecModel::from(lines.into_iter().map(SharedString::from).collect::<Vec<_>>()))
    };
    ui.set_prefab_mine(strings(state.mine.entries.iter().map(row_text).collect()));
    ui.set_prefab_game(strings(state.game.entries.iter().map(row_text).collect()));
    let row = |source: PrefabSource| match state.selected {
        Some((s, i)) if s == source => i as i32,
        _ => -1,
    };
    ui.set_prefab_selected_mine(row(PrefabSource::Library));
    ui.set_prefab_selected_game(row(PrefabSource::Game));
    ui.set_prefab_status_lines(strings(state.status_lines()));
    ui.set_prefab_confirm_delete(state.confirm_delete);
    ui.set_prefab_delete_name(state.selected_entry().map(|(_, e)| e.name.as_str()).unwrap_or_default().into());
    *shown = Some(state.clone());
}

pub struct PrefabLibraryPlugin;

impl Plugin for PrefabLibraryPlugin {
    fn build(&self, app: &mut App) {
        // The library outlives world sessions: none of it is session-reset.
        app.init_resource::<LibraryState>()
            .init_resource::<ReturnToLibrary>()
            .add_systems(OnEnter(AppState::PrefabLibrary), refresh_library)
            .add_systems(OnEnter(AppState::MainMenu), return_to_library)
            .add_systems(Update, apply_library_actions)
            .add_systems(
                PostUpdate,
                push_library_state.before(crate::renderer::slint_overlay::render_slint_layer::<FrontendUi>),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{prefab_from_ron, PrefabError};

    const ARCH_RON: &str = include_str!("../../../../assets/prefabs/dev-arch.prefab.ron");

    struct FakeClipboard(Result<String, String>);

    impl Clipboard for FakeClipboard {
        fn get(&mut self) -> Result<String, String> {
            self.0.clone()
        }

        fn set(&mut self, text: &str) -> Result<(), String> {
            self.0 = Ok(text.to_string());
            Ok(())
        }
    }

    /// A library folder (and an empty game folder beside it) under the temp dir.
    struct Folders(std::path::PathBuf);

    impl Folders {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!("mmo-prefab-library-ui-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            Self(root)
        }

        fn dir(&self, source: PrefabSource) -> Option<PrefabDir> {
            Some(PrefabDir::new(self.0.join(if source == PrefabSource::Library { "mine" } else { "game" })))
        }
    }

    impl Drop for Folders {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn act(state: &mut LibraryState, folders: &Folders, clip: &mut FakeClipboard, action: LibraryAction) -> Option<EditorRequest> {
        apply_action(state, action, "", &|s| folders.dir(s), clip)
    }

    #[test]
    fn export_delete_import_round_trip() {
        let folders = Folders::new("round");
        folders.dir(PrefabSource::Library).unwrap().save(&prefab_from_ron(ARCH_RON).unwrap()).unwrap();
        let (mut state, mut clip) = (LibraryState::default(), FakeClipboard(Err("empty".into())));
        state.refresh(&|s| folders.dir(s));
        assert_eq!(state.mine.entries.len(), 1);
        assert_eq!(row_text(&state.mine.entries[0]), "dev-arch \u{2014} Build \u{b7} 3 elements \u{b7} 8 x 6 x 4");

        act(&mut state, &folders, &mut clip, LibraryAction::PickMine(0));
        act(&mut state, &folders, &mut clip, LibraryAction::Export);
        assert!(clip.0.as_ref().unwrap().starts_with(game::EXPORT_PREFIX), "{}", state.status);

        // Delete asks first; No keeps the file.
        act(&mut state, &folders, &mut clip, LibraryAction::Delete);
        assert!(state.confirm_delete);
        act(&mut state, &folders, &mut clip, LibraryAction::ConfirmDeleteNo);
        assert_eq!(state.mine.entries.len(), 1);
        act(&mut state, &folders, &mut clip, LibraryAction::Delete);
        act(&mut state, &folders, &mut clip, LibraryAction::ConfirmDeleteYes);
        assert!(state.mine.entries.is_empty() && state.selected.is_none() && !state.confirm_delete);

        act(&mut state, &folders, &mut clip, LibraryAction::Import);
        assert_eq!(state.mine.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["dev-arch"], "{}", state.status);

        act(&mut state, &folders, &mut clip, LibraryAction::PickMine(0));
        act(&mut state, &folders, &mut clip, LibraryAction::Duplicate);
        assert_eq!(state.mine.entries.len(), 2);
        assert_eq!(state.selected_entry().map(|(_, e)| e.name.as_str()), Some("dev-arch"), "the selection follows its prefab");
    }

    #[test]
    fn a_confirm_without_the_question_deletes_nothing() {
        let folders = Folders::new("stray");
        folders.dir(PrefabSource::Library).unwrap().save(&prefab_from_ron(ARCH_RON).unwrap()).unwrap();
        let (mut state, mut clip) = (LibraryState::default(), FakeClipboard(Err("empty".into())));
        state.refresh(&|s| folders.dir(s));
        act(&mut state, &folders, &mut clip, LibraryAction::PickMine(0));
        act(&mut state, &folders, &mut clip, LibraryAction::ConfirmDeleteYes);
        assert_eq!(state.mine.entries.len(), 1);
    }

    #[test]
    fn a_bad_clipboard_is_reported_not_imported() {
        let folders = Folders::new("clip");
        let mut state = LibraryState::default();
        for (clip, expect) in [
            (Err("clipboard: nothing there".to_string()), "clipboard: nothing there".to_string()),
            (Ok("just some text".to_string()), PrefabError::NotAPrefabString.to_string()),
        ] {
            act(&mut state, &folders, &mut FakeClipboard(clip), LibraryAction::Import);
            assert!(state.status.contains(&expect), "{}", state.status);
            assert!(state.mine.entries.is_empty());
        }
    }

    #[test]
    fn junk_files_show_in_the_status_and_the_list_still_works() {
        let folders = Folders::new("junk");
        let dir = folders.dir(PrefabSource::Library).unwrap();
        dir.save(&prefab_from_ron(ARCH_RON).unwrap()).unwrap();
        std::fs::write(dir.root().join("junk.prefab.ron"), "not a prefab").unwrap();
        let mut state = LibraryState::default();
        state.refresh(&|s| folders.dir(s));
        assert_eq!(state.mine.entries.len(), 1);
        let lines = state.status_lines();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].starts_with("junk.prefab.ron: "), "{lines:?}");
    }

    #[test]
    fn new_build_names_the_prefab_and_opens_the_editor() {
        let folders = Folders::new("new");
        folders.dir(PrefabSource::Library).unwrap().save(&prefab_from_ron(ARCH_RON).unwrap()).unwrap();
        let (mut state, mut clip) = (LibraryState::default(), FakeClipboard(Err("empty".into())));
        let new = |state: &mut LibraryState, clip: &mut FakeClipboard, name: &str| {
            apply_action(state, LibraryAction::NewBuild, name, &|s| folders.dir(s), clip)
        };
        let request = new(&mut state, &mut clip, "").unwrap();
        assert_eq!((request.name.as_str(), request.new_kind), (UNTITLED, Some(PrefabKindTag::Build)));
        // A taken name gets the next free one; a bad one is refused.
        assert_eq!(new(&mut state, &mut clip, " dev-arch ").unwrap().name, "dev-arch-2");
        assert_eq!(new(&mut state, &mut clip, "My House"), None);
        assert!(state.status.contains("not a prefab name"));
        // Open edits the selected prefab where it lives.
        state.refresh(&|s| folders.dir(s));
        act(&mut state, &folders, &mut clip, LibraryAction::PickMine(0));
        let open = act(&mut state, &folders, &mut clip, LibraryAction::Open).unwrap();
        assert_eq!(open, EditorRequest { source: PrefabSource::Library, name: "dev-arch".into(), new_kind: None });
    }
}
