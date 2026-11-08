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

fn write_secret(path: &Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        f.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

/// The listener's identity. With a `hash_file` the cert and key persist beside it
/// (`<file>.cert`, `<file>.key`, key mode 0600) so a restarted listener keeps the pin its
/// dialers hold; without one the identity is ephemeral.
pub(crate) fn load_or_generate(hash_file: Option<&Path>) -> io::Result<LinkIdentity> {
    if let Some(file) = hash_file {
        let (cert_path, key_path) = (sibling(file, ".cert"), sibling(file, ".key"));
        if let (Ok(cert_der), Ok(key_der)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
            std::fs::write(file, cert_sha256_hex(&cert_der))?;
            return Ok(LinkIdentity { cert_der, key_der });
        }
    }
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .map_err(|e| io::Error::other(format!("link certificate: {e}")))?;
    let id = LinkIdentity { cert_der: cert.cert.der().to_vec(), key_der: cert.signing_key.serialize_der() };
    if let Some(file) = hash_file {
        write_secret(&sibling(file, ".key"), &id.key_der)?;
        std::fs::write(sibling(file, ".cert"), &id.cert_der)?;
        std::fs::write(file, cert_sha256_hex(&id.cert_der))?;
    }
    Ok(id)
}
