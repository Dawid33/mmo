//! Where the login refresh token is kept between runs (never logged):
//!
//! - native: the file `login_refresh` in the client's data directory
//!   (`$XDG_DATA_HOME/mmo/`, else `~/.local/share/mmo/`), owner-only, written
//!   atomically; [`REFRESH_FILE_ENV`] overrides the path;
//! - browser: `localStorage[`[`REFRESH_STORAGE_KEY`]`]`.

/// Overrides the native refresh-token file (a path; empty = unset).
pub const REFRESH_FILE_ENV: &str = "VOXEL_LOGIN_REFRESH_FILE";
/// The browser's `localStorage` key.
pub const REFRESH_STORAGE_KEY: &str = "mmo.login_refresh";
/// The native file's name, in `player_name::data_dir`.
pub const REFRESH_FILE_NAME: &str = "login_refresh";

/// The kept refresh token, if any.
pub fn load_refresh() -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        read_at(&refresh_path()?)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let storage = web_sys::window()?.local_storage().ok().flatten()?;
        storage.get_item(REFRESH_STORAGE_KEY).ok().flatten().map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
    }
}

/// Keeps `token` (replacing the old one). A failure is logged, never fatal.
pub fn save_refresh(token: &str) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(path) = refresh_path() else {
            log::warn!("[login] no home directory: the login is not kept");
            return;
        };
        if let Err(e) = write_at(&path, token) {
            log::warn!("[login] cannot keep the login in {}: {e}", path.display());
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let storage = web_sys::window().and_then(|w| w.local_storage().ok().flatten());
        match storage {
            Some(storage) if storage.set_item(REFRESH_STORAGE_KEY, token).is_ok() => {}
            _ => log::warn!("[login] localStorage unavailable: the login is not kept"),
        }
    }
}

/// Forgets the kept refresh token (logout, or the service said it is revoked).
pub fn clear_refresh() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(path) = refresh_path() {
            clear_at(&path);
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
            let _ = storage.remove_item(REFRESH_STORAGE_KEY);
        }
    }
}

/// The refresh file: [`REFRESH_FILE_ENV`], else `login_refresh` in the
/// client's data directory (`player_name::data_dir`).
#[cfg(not(target_arch = "wasm32"))]
pub fn refresh_path() -> Option<std::path::PathBuf> {
    let var = |name: &str| std::env::var(name).ok();
    refresh_path_from(
        var(REFRESH_FILE_ENV).as_deref(),
        var("XDG_DATA_HOME").as_deref(),
        var("HOME").as_deref(),
    )
}

/// [`refresh_path`] from the variables' values (empty counts as unset).
#[cfg(not(target_arch = "wasm32"))]
pub fn refresh_path_from(
    refresh_override: Option<&str>,
    xdg_data_home: Option<&str>,
    home: Option<&str>,
) -> Option<std::path::PathBuf> {
    let set = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(std::path::PathBuf::from);
    if let Some(file) = set(refresh_override) {
        return Some(file);
    }
    Some(crate::player_name::data_dir_from(xdg_data_home, home)?.join(REFRESH_FILE_NAME))
}

/// The token in `path`, trimmed; `None` when missing or empty.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_at(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Unix permissions of the refresh file (and its temp file, from creation:
/// the token is never readable by anyone else, not even for a moment).
#[cfg(all(unix, not(target_arch = "wasm32")))]
pub const REFRESH_FILE_MODE: u32 = 0o600;

/// Writes `token` to `path` atomically, owner-only: a fresh temp file created
/// with [`REFRESH_FILE_MODE`] (a stale one from a crashed write is removed
/// first), written, fsynced, then renamed over `path`.
#[cfg(not(target_arch = "wasm32"))]
pub fn write_at(path: &std::path::Path, token: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    match std::fs::remove_file(&tmp) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(REFRESH_FILE_MODE);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(format!("{token}\n").as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

/// Removes `path`; a missing file is fine.
#[cfg(not(target_arch = "wasm32"))]
pub fn clear_at(path: &std::path::Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log::warn!("[login] cannot remove {}: {e}", path.display());
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn refresh_round_trips_in_temp_dir() {
        let dir = std::env::temp_dir().join(format!("mmo-login-refresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join(REFRESH_FILE_NAME);
        assert_eq!(read_at(&path), None, "nothing kept yet");
        // A stale temp file (a crashed write), even a world-readable one, is
        // replaced, never reused.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let stale = path.with_file_name(format!("{REFRESH_FILE_NAME}.tmp"));
            std::fs::write(&stale, "stale").unwrap();
            std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        write_at(&path, "first-token").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, REFRESH_FILE_MODE);
            assert!(!path.with_file_name(format!("{REFRESH_FILE_NAME}.tmp")).exists(), "renamed away");
        }
        assert_eq!(read_at(&path).as_deref(), Some("first-token"));
        write_at(&path, "second-token").unwrap();
        assert_eq!(read_at(&path).as_deref(), Some("second-token"), "replaced");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        clear_at(&path);
        assert_eq!(read_at(&path), None);
        clear_at(&path); // already gone: fine
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_path_prefers_override_then_the_data_dir() {
        use std::path::PathBuf;
        assert_eq!(refresh_path_from(Some("/r/f"), Some("/x"), None), Some(PathBuf::from("/r/f")));
        assert_eq!(refresh_path_from(Some(" "), Some("/x"), Some("/h")), Some(PathBuf::from("/x/mmo/login_refresh")));
        assert_eq!(
            refresh_path_from(None, None, Some("/h")),
            Some(PathBuf::from("/h/.local/share/mmo/login_refresh"))
        );
        assert_eq!(refresh_path_from(None, None, None), None);
    }
}
