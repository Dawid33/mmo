//! The prefab editor: a world session on the server's empty `prefab-editor` world
//! with one prefab opened into it.
//!
//! A Build prefab is edited with the ordinary stud build mode (same keys, same
//! palette, no cost). Opening one registers it and stamps it at the editor's anchor
//! with the runtime stamp command, no turn, so the bricks read back on save sit
//! exactly where the file put them. Native only (the library is files on disk).
//!
//! Saving (Ctrl+S, `save.rs`) reads the world's bricks around the anchor back into the
//! file: everything that is not the baseplate.
//!
//! Around it: the library screen of the frontend (`library_ui.rs`: the player's
//! prefabs and, in dev builds, the game's; new, open, duplicate, delete, import and
//! export by clipboard) and the in-world panel (`panel.rs`: what is open, Save, Exit,
//! and the question before leaving with unsaved edits).
//!
//! Everything here is per session: [`session::EditorRequest`] and
//! [`session::EditorSession`] are removed when the session ends, and build mode
//! (`StudBuild`) is session state already.

pub mod library_ui;
pub mod panel;
pub mod save;
pub mod session;

pub use session::{prefab_dir, start_editor, EditorRequest, EditorSession, PrefabEditorPlugin, PrefabSource};
