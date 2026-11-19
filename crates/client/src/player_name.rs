//! Who the player says they are on a local server (single player, LAN, dev —
//! Factorio's model): a free [`PlayerName`], sent in every connection's
//! `Hello` as `Credential::Local`. No secret: the server takes the name as
//! given. Online servers ignore it (they take a login ticket instead).
//!
//! The first valid one of, in order:
//! - native: [`PLAYER_NAME_ENV`] (`just user=bob run`); the saved name (the
//!   file [`PLAYER_NAME_FILE`] in [`data_dir`]); the OS user (`USER`, else
//!   `USERNAME`); else [`DEFAULT_PLAYER_NAME`];
//! - browser: the page URL's `?name=`; the saved name
//!   (`localStorage[`[`PLAYER_NAME_STORAGE_KEY`]`]`); else a generated
//!   `player-<4 hex>`, saved so the same browser stays the same player.
//!
//! A source holding an invalid name (`PlayerName::parse`) is skipped with a
//! warning; an empty one counts as unset.

use game::PlayerName;

/// Overrides every other source (empty = unset).
pub const PLAYER_NAME_ENV: &str = "VOXEL_PLAYER_NAME";
/// The saved name's native file, in [`data_dir`].
pub const PLAYER_NAME_FILE: &str = "player_name";
/// The browser's `localStorage` key of the saved name.
pub const PLAYER_NAME_STORAGE_KEY: &str = "mmo.player_name";
/// The page URL's query parameter (`?name=bob`).
pub const PLAYER_NAME_QUERY: &str = "name";
/// The name when no source has one.
pub const DEFAULT_PLAYER_NAME: &str = "player";

/// The name this client plays under on local servers.
pub fn player_name() -> PlayerName {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let var = |name: &str| std::env::var(name).ok();
        let saved = data_dir().and_then(|dir| std::fs::read_to_string(dir.join(PLAYER_NAME_FILE)).ok());
        first_valid([
            (PLAYER_NAME_ENV, var(PLAYER_NAME_ENV)),
            ("the saved player name", saved),
            ("USER", var("USER")),
            ("USERNAME", var("USERNAME")),
        ])
        .unwrap_or_else(default_name)
    }
    #[cfg(target_arch = "wasm32")]
    {
        web::player_name()
    }
}

/// The first source whose value is a valid name; an invalid non-empty value
/// is skipped with a warning naming its source.
pub fn first_valid(sources: impl IntoIterator<Item = (&'static str, Option<String>)>) -> Option<PlayerName> {
    sources.into_iter().find_map(|(source, value)| {
        let value = value?;
        if value.trim().is_empty() {
            return None;
        }
        match PlayerName::parse(&value) {
            Ok(name) => Some(name),
            Err(why) => {
                log::warn!("[client] {source}: {value:?} is not a player name ({why}); trying the next source");
                None
            }
        }
    })
}

#[cfg(any(not(target_arch = "wasm32"), test))]
fn default_name() -> PlayerName {
    PlayerName(DEFAULT_PLAYER_NAME.to_string())
}

/// The client's data directory (the saved name, the login refresh token):
/// `$XDG_DATA_HOME/mmo`, else `~/.local/share/mmo`. `None` with neither set.
#[cfg(not(target_arch = "wasm32"))]
pub fn data_dir() -> Option<std::path::PathBuf> {
    let var = |name: &str| std::env::var(name).ok();
    data_dir_from(var("XDG_DATA_HOME").as_deref(), var("HOME").as_deref())
}

/// [`data_dir`] from the variables' values (empty counts as unset).
#[cfg(not(target_arch = "wasm32"))]
pub fn data_dir_from(xdg_data_home: Option<&str>, home: Option<&str>) -> Option<std::path::PathBuf> {
    let set = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(std::path::PathBuf::from);
    let data = set(xdg_data_home).or_else(|| set(home).map(|home| home.join(".local/share")))?;
    Some(data.join("mmo"))
}

/// The generated browser name for two random bytes: `player-<4 hex>`.
pub fn generated_name(bytes: [u8; 2]) -> PlayerName {
    PlayerName(format!("{DEFAULT_PLAYER_NAME}-{:02x}{:02x}", bytes[0], bytes[1]))
}

/// A query value as typed in a URL: `+` is a space, `%XX` escapes decoded
/// by `decode` (the browser's `decodeURIComponent`; the raw text if it fails).
pub fn query_value(raw: &str, decode: impl Fn(&str) -> Option<String>) -> String {
    let spaced = raw.replace('+', " ");
    decode(&spaced).unwrap_or(spaced)
}

#[cfg(target_arch = "wasm32")]
mod web {
    use super::*;

    /// `?name=`, else the saved name, else a new generated one (saved).
    /// Runs on the page (the WebTransport connection lives there); a worker
    /// has no storage.
    pub(super) fn player_name() -> PlayerName {
        let window = web_sys::window();
        let query = window
            .as_ref()
            .and_then(|w| w.location().search().ok())
            .and_then(|search| {
                crate::world_scale::query_param(&search, PLAYER_NAME_QUERY).map(|raw| {
                    query_value(raw, |s| js_sys::decode_uri_component(s).ok().and_then(|v| v.as_string()))
                })
            });
        let storage = window.as_ref().and_then(|w| w.local_storage().ok().flatten());
        let saved = storage.as_ref().and_then(|s| s.get_item(PLAYER_NAME_STORAGE_KEY).ok().flatten());
        if let Some(name) = first_valid([("?name=", query), ("the saved player name", saved)]) {
            return name;
        }
        let name = generated_name(random_bytes(window.as_ref()));
        match &storage {
            Some(storage) if storage.set_item(PLAYER_NAME_STORAGE_KEY, &name.0).is_ok() => {
                log::info!("[client] new player name {:?} in localStorage", name.0);
            }
            _ => log::warn!("[client] localStorage unavailable: playing as {:?} this time only", name.0),
        }
        name
    }

    fn random_bytes(window: Option<&web_sys::Window>) -> [u8; 2] {
        let mut bytes = [0u8; 2];
        let filled = window
            .and_then(|w| w.crypto().ok())
            .is_some_and(|crypto| crypto.get_random_values_with_u8_array(&mut bytes).is_ok());
        if !filled {
            // No Web Crypto: Math.random is plenty for a name suffix.
            for b in &mut bytes {
                *b = (js_sys::Math::random() * 256.0) as u8;
            }
        }
        bytes
    }
}

