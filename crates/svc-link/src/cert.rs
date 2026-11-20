//! The link's self-signed certificate, its SHA-256 pin, and the shared service token.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

/// Env var holding the shared service token itself.
pub const SERVICE_TOKEN_ENV: &str = "VOXEL_SERVICE_TOKEN";
/// Env var naming a file holding the shared service token.
pub const SERVICE_TOKEN_FILE_ENV: &str = "VOXEL_SERVICE_TOKEN_FILE";

/// Lowercase hex SHA-256 of a DER certificate: the value dialers pin.
pub fn cert_sha256_hex(cert_der: &[u8]) -> String {
    Sha256::digest(cert_der)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A valid pin: 64 hex digits.
pub(crate) fn normalize_hash(raw: &str) -> Result<String, String> {
    let h = raw.trim().to_ascii_lowercase();
    if h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(h)
    } else {
        Err("a certificate pin is 64 hex digits (SHA-256)".to_string())
    }
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// The pin stored beside `hash_file`: its content if it exists, else the SHA-256 (hex) of
/// `<hash_file>.cert` if that exists (a dialer given only the listener's read-only certificate,
/// e.g. mounted from a Secret), else `None`.
pub fn pin_beside(hash_file: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(hash_file) {
        Ok(raw) => return Ok(Some(raw)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    match std::fs::read(sibling(hash_file, ".cert")) {
        Ok(der) => Ok(Some(cert_sha256_hex(&der))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The pin to dial with: `env_var` if set and non-empty, else [`pin_beside`] `file`
/// (`NotFound` when neither the pin file nor the certificate is there).
pub fn read_cert_hash(env_var: &str, file: &Path) -> io::Result<String> {
    let raw = match std::env::var(env_var) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => pin_beside(file)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "no pin {} and no certificate {}.cert",
                    file.display(),
                    file.display()
                ),
            )
        })?,
    };
    normalize_hash(&raw).map_err(invalid)
}

/// The shared service token: `VOXEL_SERVICE_TOKEN`, else the file named by
/// `VOXEL_SERVICE_TOKEN_FILE`. An empty token is refused.
pub fn service_token_from_env() -> Result<String, String> {
    let token = match std::env::var(SERVICE_TOKEN_ENV) {
        Ok(v) => v,
        Err(_) => {
            let file = std::env::var(SERVICE_TOKEN_FILE_ENV)
                .map_err(|_| format!("set {SERVICE_TOKEN_ENV} or {SERVICE_TOKEN_FILE_ENV}"))?;
            std::fs::read_to_string(&file)
                .map_err(|e| format!("{SERVICE_TOKEN_FILE_ENV} {file}: {e}"))?
        }
    };
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err("the service token is empty".to_string());
    }
    Ok(token)
}

/// Compare without an early exit on the first differing byte.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u64;
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= (x ^ y) as u64;
    }
    diff == 0
}

pub(crate) struct LinkIdentity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

fn sibling(hash_file: &Path, ext: &str) -> PathBuf {
    let mut name = hash_file.as_os_str().to_owned();
    name.push(ext);
    PathBuf::from(name)
}

/// Write `bytes` to `path` via a temp file in the same directory and a rename, so a reader
/// never sees a truncated file. `secret` files end up mode 0600 (set explicitly: the mode on
/// create does not fix an existing file).
fn write_atomic(path: &Path, bytes: &[u8], secret: bool) -> io::Result<()> {
    let tmp = sibling(path, &format!(".tmp{}", std::process::id()));
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        if secret {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let _ = std::fs::remove_file(&tmp);
        opts.open(&tmp)?.write_all(bytes)?;
    }
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = secret;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Whether the read-only pin-file line was logged already (once per process).
static READ_ONLY_PIN_SAID: AtomicBool = AtomicBool::new(false);

/// How a failed pin-file write is logged. A directory that refuses new files (read-only
/// filesystem or no write permission — the pin file is written through a temp file and a
/// rename, so only the directory's own permission can refuse it) is the designed pod layout
/// (the identity mounted read-only from a Secret; [`pin_beside`] derives the pin from the
/// certificate): `Info`, the first time only (`said`), then `None`. Any other failure, on a
/// directory that takes files, is a `Warn` each time.
fn pin_write_level(e: &io::Error, said: &AtomicBool) -> Option<log::Level> {
    match e.kind() {
        io::ErrorKind::ReadOnlyFilesystem | io::ErrorKind::PermissionDenied => {
            (!said.swap(true, Ordering::Relaxed)).then_some(log::Level::Info)
        }
        _ => Some(log::Level::Warn),
    }
}

/// A stored cert and key that still belong together.
fn stored_pair_valid(cert_der: &[u8], key_der: &[u8]) -> bool {
    let Ok(pair) = rcgen::KeyPair::try_from(key_der) else {
        return false;
    };
    // The cert's public key must be the key's.
    cert_der
        .windows(pair.public_key_raw().len())
        .any(|w| w == pair.public_key_raw())
}

/// The listener's identity. With a `hash_file` the cert and key persist beside it
/// (`<file>.cert`, `<file>.key`, key mode 0600) so a restarted listener keeps the pin its
/// dialers hold; without one the identity is ephemeral. A stored pair that fails to load or
/// does not match is regenerated. The pin file is rewritten only if missing or different, and
/// failing to write it is logged ([`pin_write_level`]: once at info for a read-only directory, the
/// designed pod layout; a warning otherwise); a stored pair that is invalid is regenerated,
/// which on a read-only directory fails with an error — an identity is never silently ephemeral
/// when a file was asked for.
pub(crate) fn load_or_generate(hash_file: Option<&Path>) -> io::Result<LinkIdentity> {
    if let Some(file) = hash_file {
        let (cert_path, key_path) = (sibling(file, ".cert"), sibling(file, ".key"));
        if let (Ok(cert_der), Ok(key_der)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
            if stored_pair_valid(&cert_der, &key_der) {
                let pin = cert_sha256_hex(&cert_der);
                if std::fs::read_to_string(file).ok().as_deref() != Some(pin.as_str()) {
                    if let Err(e) = write_atomic(file, pin.as_bytes(), false) {
                        match pin_write_level(&e, &READ_ONLY_PIN_SAID) {
                            Some(log::Level::Info) => log::info!(
                                "[svc-link] the pin file {} is not written: its directory is read-only ({e}); the pin \
                                 is derived from the certificate",
                                file.display()
                            ),
                            Some(_) => log::warn!(
                                "[svc-link] cannot write the pin file {}: {e}",
                                file.display()
                            ),
                            None => {}
                        }
                    }
                }
                return Ok(LinkIdentity { cert_der, key_der });
            }
            log::warn!(
                "[svc-link] stored link cert/key under {} do not match; regenerating",
                file.display()
            );
        }
    }
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .map_err(|e| io::Error::other(format!("link certificate: {e}")))?;
    let id = LinkIdentity {
        cert_der: cert.cert.der().to_vec(),
        key_der: cert.signing_key.serialize_der(),
    };
    if let Some(file) = hash_file {
        write_atomic(&sibling(file, ".key"), &id.key_der, true)?;
        write_atomic(&sibling(file, ".cert"), &id.cert_der, false)?;
        write_atomic(file, cert_sha256_hex(&id.cert_der).as_bytes(), false)?;
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("svc-link-cert-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn set_mode(dir: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Whether the directory refuses a new file (root ignores the mode bits).
    #[cfg(unix)]
    fn dir_is_read_only(dir: &Path) -> bool {
        let probe = dir.join("probe");
        let writable = std::fs::write(&probe, b"x").is_ok();
        let _ = std::fs::remove_file(&probe);
        !writable
    }

    /// Regression: Read-only pin-file mount logged '[WARN] [svc-link] cannot write the pin file ... Read-only file system' on every coordinator/host start; seen IN POD LOGS of twin-check run check-20261115T122246Z (.superpowers/sdd/2026-11-15-r5-k8s/task-13-report.md), fixed in dd60fbf4
    #[cfg(unix)]
    #[test]
    fn a_pin_file_a_read_only_directory_refuses_is_said_once_at_info() {
        let dir = temp_dir("ro-pin");
        let file = dir.join("link.pin");
        set_mode(&dir, 0o555);
        let skip = !dir_is_read_only(&dir);
        let refused = write_atomic(&file, b"pin", false).err();
        set_mode(&dir, 0o755);
        let _ = std::fs::remove_dir_all(&dir);
        if skip {
            eprintln!("skipped: running as a user the directory mode does not bind (root)");
            return;
        }
        let refused = refused.expect("a read-only directory refuses the pin file");
        let said = AtomicBool::new(false);
        assert_eq!(
            pin_write_level(&refused, &said),
            Some(log::Level::Info),
            "the designed pod layout"
        );
        assert_eq!(pin_write_level(&refused, &said), None, "said once");
    }
}
