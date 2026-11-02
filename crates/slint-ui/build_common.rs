// Shared by the build scripts of slint-ui, slint-ui-frontend and slint-ui-tools
// (`include!`d): each compiles ONE entry .slint file from crates/slint-ui/ui
// against the `widgets` library. The screens are split over three crates so a
// .slint edit only regenerates and recompiles the crate whose roots it touches
// (the Slint compiler emits several MB of Rust per crate), and the crates
// build in parallel.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Compiles `entry` (relative to the calling crate) with the widgets library,
/// which exports its .slint directory through the `links` metadata
/// (`DEP_WIDGETS_UI_DIR`) — as a single-theme overlay (`single_theme_overlay`).
/// Otherwise the same configuration as `~/Projects/ui`'s showcase and reftest,
/// so the pixels match what reftest measures.
fn compile_ui(entry: &str) {
    let widgets_ui =
        PathBuf::from(std::env::var("DEP_WIDGETS_UI_DIR").expect("widgets crate exports its ui dir"));
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let widgets = single_theme_overlay(&widgets_ui, &out.join("widgets-fortress"));
    let config = slint_build::CompilerConfiguration::new()
        .with_library_paths(HashMap::from([("widgets".to_string(), widgets.join("index.slint"))]));
    slint_build::compile_with_config(entry, config).unwrap();
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        redirect_embedded_images();
    }
}

/// The widgets' generated `styles.slint` holds BOTH themes (`FactorioStyles`, `FortressStyles`,
/// picked at run time by `StylesFor.x(t) = t == Theme.fortress ? Fortress… : Factorio…`), so every
/// crate compiled the two style tables and all ~7,000 sprites although the game only ever sets
/// `Theme.fortress` (ui/theme.slint). This writes a copy of the widgets' .slint files into `dst`
/// whose `generated/styles.slint` is Fortress only: the Factorio globals dropped, each ternary cut
/// to its Fortress branch (the `Theme` enum and `ActiveTheme` stay, so the API is unchanged but
/// the theme no longer switches). Images and fonts are referenced in place by absolute path.
/// Measured 2026-11-02: rustc 8.1 → 5.5 s (game crate), 11.4 → 7.6 s (frontend); the build
/// script's own time and memory halve. Returns `dst`.
///
/// Every rewrite is asserted, so a change to the widgets' codegen fails the build here rather
/// than silently compiling both themes again.
fn single_theme_overlay(src: &Path, dst: &Path) -> PathBuf {
    const STYLES: &str = "generated/styles.slint";
    const PICK: &str = "t == Theme.fortress ? ";
    std::fs::create_dir_all(dst.join("generated")).unwrap();
    // Cargo re-runs us when the real widgets change; slint-build only reports the overlay.
    println!("cargo::rerun-if-changed={}", src.join(STYLES).display());
    let mut copied = 0;
    for entry in std::fs::read_dir(src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|x| x == "slint") {
            println!("cargo::rerun-if-changed={}", path.display());
            write_if_changed(&dst.join(path.file_name().unwrap()), &std::fs::read_to_string(&path).unwrap());
            copied += 1;
        }
    }
    assert!(copied > 0, "slint-ui build_common.rs: no .slint files in {}", src.display());

    let generated = src.join("generated");
    let source = std::fs::read_to_string(src.join(STYLES)).unwrap();
    let mut styles = String::with_capacity(source.len() / 2);
    let (mut dropped, mut picked) = (0, 0);
    let mut skipping = false;
    for line in source.lines() {
        if line.starts_with("global Factorio") {
            skipping = true;
            dropped += 1;
        }
        if skipping {
            skipping = line != "}";
            continue;
        }
        let line = match line.find(PICK) {
            // `{ t == Theme.fortress ? FortressStyles.x : FactorioStyles.x }` -> `{ FortressStyles.x }`
            Some(at) => {
                let rest = &line[at + PICK.len()..];
                let colon = rest.find(" : Factorio").expect("theme ternary has a Factorio branch");
                let close = rest[colon..].find(" }").expect("theme ternary closes the function body");
                picked += 1;
                format!("{}{}{}", &line[..at], &rest[..colon], &rest[colon + close..])
            }
            None => line.to_string(),
        };
        let line = line
            .replace("@image-url(\"images/", &format!("@image-url(\"{}/images/", generated.display()))
            .replace("import \"fonts/", &format!("import \"{}/fonts/", generated.display()));
        styles.push_str(&line);
        styles.push('\n');
    }
    assert!(
        dropped >= 2 && picked > 0,
        "slint-ui build_common.rs: the widgets' {STYLES} changed shape (dropped {dropped} Factorio globals, \
         cut {picked} theme ternaries); update single_theme_overlay"
    );
    // (Line 1 is the "@generated … from the local Factorio install" header.)
    assert!(
        !styles.lines().skip(1).any(|l| l.contains("Factorio")),
        "slint-ui build_common.rs: a Factorio reference survived the single-theme rewrite of {STYLES}"
    );
    write_if_changed(&dst.join(STYLES), &styles);
    dst.to_path_buf()
}

/// Leaves an unchanged file's mtime alone (slint-build reports every file it reads to cargo).
fn write_if_changed(path: &Path, contents: &str) {
    if std::fs::read_to_string(path).ok().as_deref() != Some(contents) {
        std::fs::write(path, contents).unwrap();
    }
}

/// wasm32: Slint's `load_image_from_embedded_data` hands the PNG bytes to the browser
/// (`HTMLImage`), which the software renderer cannot draw ("not implemented: The image cannot
/// be rendered"). Point every call in the generated code at `crate::wasm_images::load_embedded`
/// (same signature; each crate mounts crates/slint-ui/src/wasm_images.rs there), which decodes
/// in Rust as native does. (Compile-time embedding, `EmbedForSoftwareRenderer`, is no way out:
/// its texture pass asserts on 1-px-wide images, and a third of the widgets' nine-slice
/// sprites are.)
fn redirect_embedded_images() {
    const SLINT_CALL: &str = "sp :: load_image_from_embedded_data (";
    const OURS: &str = "crate :: wasm_images :: load_embedded (";
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let generated = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "rs"))
        .expect("slint-build wrote its .rs into OUT_DIR");
    let code = std::fs::read_to_string(&generated).unwrap();
    // A Slint upgrade that changes how the call is spelled must fail the build here,
    // not bring the browser panic back.
    assert!(
        code.contains(SLINT_CALL),
        "slint-ui build_common.rs: no `{SLINT_CALL}` in {} — Slint's generated code changed; update the redirect",
        generated.display()
    );
    std::fs::write(&generated, code.replace(SLINT_CALL, OURS)).unwrap();
}
