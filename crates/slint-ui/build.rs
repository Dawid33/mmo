//! Compiles the game's screens (every component `ui/index.slint` exports) against the `widgets` library, which exports
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
    slint_build::compile_with_config("ui/index.slint", config).unwrap();
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        redirect_embedded_images();
    }
}

/// wasm32: Slint's `load_image_from_embedded_data` hands the PNG bytes to the browser
/// (`HTMLImage`), which the software renderer cannot draw ("not implemented: The image cannot
/// be rendered"). Point every call in the generated code at `crate::wasm_images::load_embedded`
/// (same signature), which decodes in Rust as native does. (Compile-time embedding,
/// `EmbedForSoftwareRenderer`, is no way out: its texture pass asserts on 1-px-wide images,
/// and a third of the widgets' nine-slice sprites are.)
fn redirect_embedded_images() {
    const SLINT_CALL: &str = "sp :: load_image_from_embedded_data (";
    const OURS: &str = "crate :: wasm_images :: load_embedded (";
    let generated = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("index.rs");
    let code = std::fs::read_to_string(&generated).expect("slint-build wrote OUT_DIR/index.rs");
    // A Slint upgrade that changes how the call is spelled must fail the build here,
    // not bring the browser panic back.
    assert!(
        code.contains(SLINT_CALL),
        "slint-ui build.rs: no `{SLINT_CALL}` in {} — Slint's generated code changed; update the redirect",
        generated.display()
    );
    std::fs::write(&generated, code.replace(SLINT_CALL, OURS)).unwrap();
}
