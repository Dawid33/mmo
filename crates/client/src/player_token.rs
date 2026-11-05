//! The player's identity on this machine: the [`PlayerToken`] every connection
//! sends first (`ClientPacket::Hello`), so the server knows it is the same
//! player again (`game::player`). Made once and kept:
//!
//! - native: the file [`token_path`] — `$XDG_DATA_HOME/mmo/player_token`, else
//!   `~/.local/share/mmo/player_token`; [`PLAYER_TOKEN_FILE_ENV`] overrides it
//!   (`just run-two`'s second client keeps its own);
//! - browser: `localStorage[`[`PLAYER_TOKEN_STORAGE_KEY`]`]`.
//!
//! When neither can be read or written the session gets a fresh token that is
//! not kept (a new player each time), with a warning.

use game::PlayerToken;

/// Overrides the native token file (a path; empty = unset).
pub const PLAYER_TOKEN_FILE_ENV: &str = "VOXEL_PLAYER_TOKEN_FILE";
/// The browser's `localStorage` key.
pub const PLAYER_TOKEN_STORAGE_KEY: &str = "mmo.player_token";

/// This machine's player token: the kept one, or a new one (kept from now on).
pub fn load_or_create_token() -> PlayerToken {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(path) = token_path() else {
            log::warn!("[client] no home directory for the player token: playing as a new player");
            return PlayerToken::generate(random_bytes());
        };
        match load_or_create_at(&path) {
            Ok(token) => token,
            Err(e) => {
                log::warn!("[client] player token {}: {e}; playing as a new player", path.display());
                PlayerToken::generate(random_bytes())
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        web::load_or_create()
    }
}

/// The token file: [`PLAYER_TOKEN_FILE_ENV`], else under `XDG_DATA_HOME`,
/// else under `HOME`. `None` with none of them set.
#[cfg(not(target_arch = "wasm32"))]
pub fn token_path() -> Option<std::path::PathBuf> {
    let var = |name: &str| std::env::var(name).ok();
    token_path_from(var(PLAYER_TOKEN_FILE_ENV).as_deref(), var("XDG_DATA_HOME").as_deref(), var("HOME").as_deref())
}

/// [`token_path`] from the variables' values (empty counts as unset).
#[cfg(not(target_arch = "wasm32"))]
pub fn token_path_from(
    override_file: Option<&str>,
    xdg_data_home: Option<&str>,
    home: Option<&str>,
) -> Option<std::path::PathBuf> {
    let set = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(std::path::PathBuf::from);
    if let Some(file) = set(override_file) {
        return Some(file);
    }
    let data = set(xdg_data_home).or_else(|| set(home).map(|home| home.join(".local/share")))?;
    Some(data.join("mmo").join("player_token"))
}

/// The token in `path`, or a new one written there (atomically, owner-only on
/// unix). A file that does not hold a valid token is replaced.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_or_create_at(path: &std::path::Path) -> std::io::Result<PlayerToken> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let token = PlayerToken(text.trim().to_string());
            if token.is_valid() {
                return Ok(token);
            }
            log::warn!("[client] {} does not hold a player token; making a new one", path.display());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let token = PlayerToken::generate(random_bytes());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, format!("{}\n", token.0))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    log::info!("[client] new player token in {}", path.display());
    Ok(token)
}

#[cfg(not(target_arch = "wasm32"))]
fn random_bytes() -> [u8; game::PLAYER_TOKEN_BYTES] {
    let mut bytes = [0; game::PLAYER_TOKEN_BYTES];
    getrandom::fill(&mut bytes).expect("the OS random number generator failed");
    bytes
}

#[cfg(target_arch = "wasm32")]
mod web {
    use super::*;

    /// `localStorage`'s token, or a new one stored there. Runs on the page
    /// (the WebTransport connection lives there); a worker has no storage.
    pub(super) fn load_or_create() -> PlayerToken {
        let Some(window) = web_sys::window() else {
            log::warn!("[client] no window for the player token: playing as a new player");
            return fresh(None);
        };
        let storage = window.local_storage().ok().flatten();
        if let Some(storage) = &storage {
            if let Ok(Some(text)) = storage.get_item(PLAYER_TOKEN_STORAGE_KEY) {
                let token = PlayerToken(text.trim().to_string());
                if token.is_valid() {
                    return token;
                }
            }
        }
        let token = fresh(Some(&window));
        match &storage {
            Some(storage) if storage.set_item(PLAYER_TOKEN_STORAGE_KEY, &token.0).is_ok() => {
                log::info!("[client] new player token in localStorage");
            }
            _ => log::warn!("[client] localStorage unavailable: playing as a new player"),
        }
        token
    }

    fn fresh(window: Option<&web_sys::Window>) -> PlayerToken {
        let mut bytes = [0u8; game::PLAYER_TOKEN_BYTES];
        let filled = window
            .and_then(|w| w.crypto().ok())
            .is_some_and(|crypto| crypto.get_random_values_with_u8_array(&mut bytes).is_ok());
        if !filled {
            // No Web Crypto: Math.random, which is enough for a bearer token
            // that is not kept.
            for b in &mut bytes {
                *b = (js_sys::Math::random() * 256.0) as u8;
            }
        }
        PlayerToken::generate(bytes)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn creates_once_then_reuses() {
        let dir = std::env::temp_dir().join(format!("mmo-player-token-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("player_token");
        // The env override is this test's alone (no other test reads it).
        std::env::set_var(PLAYER_TOKEN_FILE_ENV, &path);
        let first = load_or_create_token();
        std::env::remove_var(PLAYER_TOKEN_FILE_ENV);
        assert!(first.is_valid());
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), first.0, "kept in the file");
        assert_eq!(load_or_create_at(&path).unwrap(), first, "the same token next time");

        std::fs::write(&path, "garbage").unwrap();
        let replaced = load_or_create_at(&path).unwrap();
        assert!(replaced.is_valid());
        assert_ne!(replaced, first);
        assert_eq!(load_or_create_at(&path).unwrap(), replaced);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_path_prefers_the_override_then_xdg_then_home() {
        use std::path::PathBuf;
        assert_eq!(token_path_from(Some("/t/tok"), Some("/x"), Some("/h")), Some(PathBuf::from("/t/tok")));
        assert_eq!(token_path_from(Some(" "), Some("/x"), Some("/h")), Some(PathBuf::from("/x/mmo/player_token")));
        assert_eq!(token_path_from(None, Some(""), Some("/h")), Some(PathBuf::from("/h/.local/share/mmo/player_token")));
        assert_eq!(token_path_from(None, None, None), None);
    }
}
