//! The editor's in-world panel (`ui/prefab-editor.slint`, embedded in `GameUi`): what
//! is open, Save, Exit, and the question before leaving with unsaved edits.
//!
//! Like the rest of the game UI: the view is pushed only when it changes, and Slint
//! callbacks queue [`EditorUiAction`]s that a system applies in `Update`.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::prelude::*;
use game::PrefabKindTag;
use slint_ui::GameUi;

use super::save::SaveRequested;
use super::session::EditorSession;
use crate::renderer::slint_overlay::SlintLayer;
use crate::session::{LeaveGuard, SessionEnded};

/// What a panel button asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorUiAction {
    Save,
    Exit,
    ConfirmDiscard,
    CancelDiscard,
}

/// The panel's queued callbacks (main thread, like the UI itself).
pub struct EditorUiActions(Rc<RefCell<Vec<EditorUiAction>>>);

/// Connect the panel's callbacks. Called on every (re)built `GameUi`
/// (`slint_overlay::connect_game_ui`).
pub fn connect(ui: &GameUi, world: &mut World) {
    let actions = Rc::new(RefCell::new(Vec::new()));
    let push = |action: EditorUiAction| {
        let queue = actions.clone();
        move || queue.borrow_mut().push(action)
    };
    ui.on_prefab_save(push(EditorUiAction::Save));
    ui.on_prefab_exit(push(EditorUiAction::Exit));
    ui.on_prefab_discard(push(EditorUiAction::ConfirmDiscard));
    ui.on_prefab_keep_editing(push(EditorUiAction::CancelDiscard));
    world.insert_non_send(EditorUiActions(actions));
}

/// Queued panel callbacks: a save request, or the way out (asked first with unsaved
/// edits — the same question the Esc menu's Main menu puts through [`LeaveGuard`]).
pub fn apply_editor_ui_actions(
    actions: Option<NonSend<EditorUiActions>>,
    session: Option<Res<EditorSession>>,
    mut guard: ResMut<LeaveGuard>,
    mut save: MessageWriter<SaveRequested>,
    mut ended: MessageWriter<SessionEnded>,
) {
    let Some(actions) = actions else { return };
    let queued: Vec<EditorUiAction> = actions.0.borrow_mut().drain(..).collect();
    if session.is_none() {
        return;
    }
    for action in queued {
        match action {
            EditorUiAction::Save => {
                save.write(SaveRequested);
            }
            EditorUiAction::Exit => {
                if guard.request_leave() {
                    ended.write(SessionEnded { reason: None });
                }
            }
            EditorUiAction::ConfirmDiscard => {
                guard.asked = false;
                ended.write(SessionEnded { reason: None });
            }
            EditorUiAction::CancelDiscard => guard.asked = false,
        }
    }
}

/// Unsaved edits hold the player's ways out back until asked.
pub fn sync_leave_guard(session: Option<Res<EditorSession>>, mut guard: ResMut<LeaveGuard>) {
    let unsaved = session.is_some_and(|s| s.dirty);
    if guard.unsaved != unsaved {
        guard.unsaved = unsaved;
        // Saved while the question was up: nothing left to ask.
        guard.asked &= unsaved;
    }
}

/// What the panel shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PanelView {
    pub open: bool,
    pub title: String,
    pub counts: String,
    pub dirty: bool,
    pub message: String,
    pub can_save: bool,
    pub confirm_exit: bool,
}

/// The panel for `session` (closed without one).
pub fn panel_view(session: Option<&EditorSession>, guard: LeaveGuard) -> PanelView {
    let Some(s) = session else { return PanelView::default() };
    let kind = match s.kind {
        PrefabKindTag::Build => "Build",
        PrefabKindTag::Prop => "Prop",
    };
    let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    PanelView {
        open: true,
        title: format!("{} \u{2014} {kind}", s.name),
        counts: if s.loaded {
            format!("{}, {}", plural(s.parts, "part", "parts"), plural(s.boxes, "box", "boxes"))
        } else {
            "Loading\u{2026}".into()
        },
        dirty: s.dirty,
        message: s.message.clone().unwrap_or_default(),
        can_save: s.kind == PrefabKindTag::Build && s.loaded,
        confirm_exit: guard.asked,
    }
}

/// The session → the panel, only when it changed (or the UI was rebuilt and lost it).
pub fn sync_editor_panel(
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    session: Option<Res<EditorSession>>,
    guard: Res<LeaveGuard>,
    mut shown: crate::session::SessionLocal<Option<PanelView>>,
) {
    let Some(slint) = slint else { return };
    let ui = slint.overlay.ui();
    let view = panel_view(session.as_deref(), *guard);
    let shown = shown.get();
    // A rebuilt UI starts closed.
    let lost = view.open && !ui.get_prefab_panel_open();
    if shown.as_ref() == Some(&view) && !lost {
        return;
    }
    ui.set_prefab_panel_open(view.open);
    ui.set_prefab_title(view.title.as_str().into());
    ui.set_prefab_counts(view.counts.as_str().into());
    ui.set_prefab_dirty(view.dirty);
    ui.set_prefab_message(view.message.as_str().into());
    ui.set_prefab_can_save(view.can_save);
    ui.set_prefab_confirm_exit(view.confirm_exit);
    *shown = Some(view);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_session_no_panel() {
        assert_eq!(panel_view(None, LeaveGuard::default()), PanelView::default());
        assert!(!panel_view(None, LeaveGuard { unsaved: true, asked: true }).open);
    }

    #[test]
    fn leaving_asks_only_with_unsaved_edits() {
        let mut guard = LeaveGuard::default();
        assert!(guard.request_leave());
        assert!(!guard.asked);
        guard.unsaved = true;
        assert!(!guard.request_leave());
        assert!(guard.asked);
    }
}
