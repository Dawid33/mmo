//! The link's self-signed certificate, its SHA-256 pin, and the shared service token.

use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Env var holding the shared service token itself.
pub const SERVICE_TOKEN_ENV: &str = "VOXEL_SERVICE_TOKEN";
/// Env var naming a file holding the shared service token.
pub const SERVICE_TOKEN_FILE_ENV: &str = "VOXEL_SERVICE_TOKEN_FILE";

/// Lowercase hex SHA-256 of a DER certificate: the value dialers pin.
pub fn cert_sha256_hex(cert_der: &[u8]) -> String {
    Sha256::digest(cert_der).iter().map(|b| format!("{b:02x}")).collect()
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

/// The pin to dial with: `env_var` if set and non-empty, else the contents of `file`.
pub fn read_cert_hash(env_var: &str, file: &Path) -> io::Result<String> {
    let raw = match std::env::var(env_var) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => std::fs::read_to_string(file)?,
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
            std::fs::read_to_string(&file).map_err(|e| format!("{SERVICE_TOKEN_FILE_ENV} {file}: {e}"))?
        }
    };
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err("the service token is empty".to_string());
    }
    Ok(token)
}

/// Compare without an early exit on the first differing byte.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
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
    std::fs::write(&tmp, bytes)?;
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

/// A stored cert and key that still belong together.
fn stored_pair_valid(cert_der: &[u8], key_der: &[u8]) -> bool {
    let Ok(pair) = rcgen::KeyPair::try_from(key_der) else { return false };
    // The cert's public key must be the key's.
    cert_der.windows(pair.public_key_raw().len()).any(|w| w == pair.public_key_raw())
}

/// The listener's identity. With a `hash_file` the cert and key persist beside it
/// (`<file>.cert`, `<file>.key`, key mode 0600) so a restarted listener keeps the pin its
/// dialers hold; without one the identity is ephemeral. A stored pair that fails to load or
/// does not match is regenerated.
pub(crate) fn load_or_generate(hash_file: Option<&Path>) -> io::Result<LinkIdentity> {
    if let Some(file) = hash_file {
        let (cert_path, key_path) = (sibling(file, ".cert"), sibling(file, ".key"));
        if let (Ok(cert_der), Ok(key_der)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
            if stored_pair_valid(&cert_der, &key_der) {
                write_atomic(file, cert_sha256_hex(&cert_der).as_bytes(), false)?;
                return Ok(LinkIdentity { cert_der, key_der });
            }
            log::warn!("[svc-link] stored link cert/key under {} do not match; regenerating", file.display());
        }
    }
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .map_err(|e| io::Error::other(format!("link certificate: {e}")))?;
    let id = LinkIdentity { cert_der: cert.cert.der().to_vec(), key_der: cert.signing_key.serialize_der() };
    if let Some(file) = hash_file {
        write_atomic(&sibling(file, ".key"), &id.key_der, true)?;
        write_atomic(&sibling(file, ".cert"), &id.cert_der, false)?;
        write_atomic(file, cert_sha256_hex(&id.cert_der).as_bytes(), false)?;
    }
    Ok(id)
}
