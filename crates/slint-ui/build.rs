//! Compiles the game's screens against the `widgets` library, which exports
//! its .slint directory through the `links` metadata (`DEP_WIDGETS_UI_DIR`).
//! Same configuration as `~/Projects/ui`'s showcase and reftest, so the
//! pixels match what reftest measures.
use std::collections::HashMap;
use std::path::PathBuf;

fn main() {
    let widgets_ui =
        PathBuf::from(std::env::var("DEP_WIDGETS_UI_DIR").expect("widgets crate exports its ui dir"));
    let config = slint_build::CompilerConfiguration::new()
        .with_library_paths(HashMap::from([("widgets".to_string(), widgets_ui.join("index.slint"))]));
    slint_build::compile_with_config("ui/game.slint", config).unwrap();
}
