//! Slint-generated screens, split out of `slint-ui` so they compile in their
//! own crate (see crates/slint-ui/build_common.rs). Draw them with
//! `slint_ui::Overlay`.
#[cfg(target_arch = "wasm32")]
#[path = "../../slint-ui/src/wasm_images.rs"]
mod wasm_images;

slint::include_modules!();

// The dependency is for the build script (`DEP_WIDGETS_UI_DIR`); naming it keeps
// cargo's `unused_dependencies` lint quiet.
use widgets as _;
