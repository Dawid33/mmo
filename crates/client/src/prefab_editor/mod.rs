//! The prefab editor: a world session on the server's empty `prefab-editor` world
//! with one prefab opened into it.
//!
//! A Build prefab is edited with the ordinary stud build mode (same keys, same
//! palette, no cost). Opening one registers it and stamps it at the editor's anchor
//! with the runtime stamp command, no turn, so the bricks read back on save sit
//! exactly where the file put them. Native only (the library is files on disk).
//!
//! Everything here is per session: [`session::EditorRequest`] and
//! [`session::EditorSession`] are removed when the session ends, and build mode
//! (`StudBuild`) is session state already.

pub mod session;

pub use session::{prefab_dir, start_editor, EditorRequest, EditorSession, PrefabEditorPlugin, PrefabSource};
