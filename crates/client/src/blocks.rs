//! Loads the shared block registry on the client. Prefers the runtime
//! manifest under `assets/blocks/` on native (edit + restart, no rebuild);
//! falls back to the copy embedded at build time, which is also what the
//! browser (no filesystem) uses. Both the renderer (textures) and the offline
//! `LocalServer` (worldgen material ids) read through here, so a single repo
//! file drives client identity.

use game::BlockRegistry;

/// The manifest embedded at build time — identical bytes to the on-disk file.
const EMBEDDED_MANIFEST: &str = include_str!("../../../assets/blocks/blocks.ron");

fn manifest_src() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = crate::renderer::resolve_blocks_dir().join("blocks.ron");
        if let Ok(s) = std::fs::read_to_string(&path) {
            return s;
        }
    }
    EMBEDDED_MANIFEST.to_string()
}

/// Parse the block registry, panicking on a malformed manifest (a fatal
/// startup misconfiguration).
pub fn load_registry() -> BlockRegistry {
    BlockRegistry::from_ron(&manifest_src()).expect("invalid block manifest")
}
