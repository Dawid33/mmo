//! The one certificate a host shows its players: the QUIC listener (native clients) and the
//! WebTransport ingress (browsers) both serve it, so the hash a host advertises
//! (`game::HostEndpoint::cert_hash`) pins either connection.
//!
//! Two sources: with neither [`CLIENT_CERT_ENV`] nor [`CLIENT_KEY_ENV`] set, a self-signed
//! certificate minted once per process (ECDSA P-256, valid 14 days — what a browser's
//! `serverCertificateHashes` accepts), whose SHA-256 the host advertises; with both, a PEM
//! chain and key from files (a CA's certificate: no hash), re-read every [`CERT_POLL`] so a
//! renewal (cert-manager rewriting a mounted Secret) is served without a restart.
//!
//! *Known limit:* a self-signed host running longer than 14 days is refused by browsers.
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use quinn::rustls::{
    self,
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};

/// PEM certificate chain served to players (set together with [`CLIENT_KEY_ENV`]).
pub const CLIENT_CERT_ENV: &str = "VOXEL_CLIENT_CERT";
/// PEM private key of [`CLIENT_CERT_ENV`]'s leaf.
pub const CLIENT_KEY_ENV: &str = "VOXEL_CLIENT_KEY";
/// The older name of [`CLIENT_CERT_ENV`] (WebTransport only, before QUIC shared it): still accepted.
pub const WT_CERT_ENV: &str = "VOXEL_WT_CERT";
/// The older name of [`CLIENT_KEY_ENV`]: still accepted.
pub const WT_KEY_ENV: &str = "VOXEL_WT_KEY";

/// How often certificate files are checked for a renewal ([`ClientCert::reload_if_changed`]).
pub const CERT_POLL: Duration = Duration::from_secs(60);

/// How often a host waiting for its certificate files at start looks again
/// ([`ClientCert::wait_from_env`]); it warns at most every [`CERT_POLL`].
pub const CERT_WAIT_RETRY: Duration = Duration::from_secs(2);

/// How long a reload that found a pair it could not load pauses before its one re-read
/// ([`ClientCert::reload_with_retry`]): a read that straddled a Secret's symlink swap.
pub const RELOAD_RETRY: Duration = Duration::from_secs(1);

/// The names a self-signed certificate is minted for.
const SELF_SIGNED_NAMES: [&str; 2] = ["localhost", "127.0.0.1"];

/// The certificate both player-facing listeners serve. Cloning shares it: a reload is seen
/// by every listener.
#[derive(Clone)]
pub struct ClientCert {
    shared: Arc<Shared>,
}

struct Shared {
    /// The pair handed to every new handshake.
    current: RwLock<Arc<CertifiedKey>>,
    source: Source,
}

enum Source {
    /// Minted at start; its leaf's SHA-256, lowercase hex.
    SelfSigned { sha256_hex: String },
    /// Read from files; `loaded` = the bytes of the pair being served.
    Files { cert: PathBuf, key: PathBuf, loaded: Mutex<(Vec<u8>, Vec<u8>)> },
}

impl ClientCert {
    /// The certificate from the environment (`lookup`): both files (the new names or their
    /// old aliases) or neither (empty counts as unset); neither = a self-signed certificate,
    /// minted here. Files that do not load are an error.
    pub fn from_env(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        match files_from_env(&lookup)? {
            None => Self::self_signed(),
            Some((cert, key)) => Self::load_files(cert, key).map_err(|e| e.to_string()),
        }
    }

    /// A fresh self-signed certificate (ECDSA P-256, valid 14 days: wtransport's minting,
    /// which a browser's `serverCertificateHashes` accepts).
    pub fn self_signed() -> Result<Self, String> {
        let identity = wtransport::Identity::self_signed(SELF_SIGNED_NAMES)
            .map_err(|e| format!("minting the client certificate: {e}"))?;
        let chain: Vec<CertificateDer<'static>> =
            identity.certificate_chain().as_slice().iter().map(|c| CertificateDer::from(c.der().to_vec())).collect();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.private_key().secret_der().to_vec()));
        let leaf = identity.certificate_chain().as_slice().first().ok_or("a self-signed identity has a certificate")?;
        let sha256_hex = leaf.hash().as_ref().iter().map(|b| format!("{b:02x}")).collect();
        let pair = certified(chain, key).map_err(|e| format!("the minted client certificate: {e}"))?;
        Ok(Self::new(pair, Source::SelfSigned { sha256_hex }))
    }

    /// The pair in files `cert` / `key`, loaded and checked now.
    fn load_files(cert: PathBuf, key: PathBuf) -> io::Result<Self> {
        let bytes = read_pair(&cert, &key).map_err(|e| files_error(&cert, &key, &e))?;
        let pair = parse_pair(&bytes).map_err(|e| files_error(&cert, &key, &e))?;
        Ok(Self::new(pair, Source::Files { cert, key, loaded: Mutex::new(bytes) }))
    }

    fn new(pair: CertifiedKey, source: Source) -> Self {
        Self { shared: Arc::new(Shared { current: RwLock::new(Arc::new(pair)), source }) }
    }

    /// The leaf's SHA-256, lowercase hex, when self-signed (what a client pins); `None` for a
    /// certificate from files (a CA's: verified by name).
    pub fn sha256_hex(&self) -> Option<String> {
        match &self.shared.source {
            Source::SelfSigned { sha256_hex } => Some(sha256_hex.clone()),
            Source::Files { .. } => None,
        }
    }

    /// Whether the certificate comes from files (and so wants [`Self::reload_if_changed`]).
    pub fn is_from_files(&self) -> bool {
        matches!(self.shared.source, Source::Files { .. })
    }

    /// A resolver that hands every handshake the current pair.
    pub fn rustls_resolver(&self) -> Arc<dyn ResolvesServerCert> {
        self.resolver()
    }

    fn resolver(&self) -> Arc<Resolver> {
        Arc::new(Resolver(self.shared.clone()))
    }

    /// Re-reads the files (a self-signed certificate: `Ok(false)`): `Ok(true)` when a changed
    /// pair is now served, `Ok(false)` when nothing changed, `Err` when the files do not load
    /// as a matching chain and key — the old pair is kept (a renewal caught half-written is
    /// loaded on a later call).
    pub fn reload_if_changed(&self) -> io::Result<bool> {
        let Source::Files { cert, key, loaded } = &self.shared.source else {
            return Ok(false);
        };
        let bytes = read_pair(cert, key).map_err(|e| files_error(cert, key, &e))?;
        let mut loaded = loaded.lock().unwrap_or_else(|e| e.into_inner());
        if *loaded == bytes {
            return Ok(false);
        }
        let pair = parse_pair(&bytes).map_err(|e| files_error(cert, key, &e))?;
        *self.shared.current.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(pair);
        *loaded = bytes;
        Ok(true)
    }

    /// [`Self::from_env`], but files that are not there yet (or do not load as a matching
    /// pair) are waited for; `Ok(None)` = `stop` was set while waiting. A configuration error
    /// (one variable alone, the new and old name disagreeing) is an `Err` at once.
    pub fn wait_from_env(
        lookup: impl Fn(&str) -> Option<String>,
        stop: Option<&AtomicBool>,
    ) -> Result<Option<Self>, String> {
        let Some((cert, key)) = files_from_env(&lookup)? else {
            return Self::self_signed().map(Some);
        };
        Ok(wait_for_files(&cert, &key, stop, CERT_WAIT_RETRY, || Self::load_files(cert.clone(), key.clone())))
    }

    /// [`Self::reload_if_changed`]; an `Err` is re-read once after `pause`.
    pub async fn reload_with_retry(&self, pause: Duration) -> io::Result<bool> {
        match self.reload_if_changed() {
            Err(_) => {
                tokio::time::sleep(pause).await;
                self.reload_if_changed()
            }
            done => done,
        }
    }

    /// The pair being served now.
    fn current(&self) -> Arc<CertifiedKey> {
        self.shared.current()
    }

    /// The DER of the leaf served now.
    pub fn leaf_der(&self) -> Vec<u8> {
        self.current().cert.first().map(|c| c.to_vec()).unwrap_or_default()
    }
}

impl Shared {
    fn current(&self) -> Arc<CertifiedKey> {
        self.current.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[derive(Debug)]
struct Resolver(Arc<Shared>);

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientCert").finish_non_exhaustive()
    }
}

impl ResolvesServerCert for Resolver {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.current())
    }
}

/// Calls `try_load` until it loads (`Some`) or `stop` is set (`None`), every `retry`; warns
/// at start and then at most every [`CERT_POLL`].
fn wait_for_files(
    cert: &Path,
    key: &Path,
    stop: Option<&AtomicBool>,
    retry: Duration,
    try_load: impl FnMut() -> io::Result<ClientCert>,
) -> Option<ClientCert> {
    let stopped = || stop.is_some_and(|s| s.load(Ordering::Relaxed));
    let mut warned: Option<std::time::Instant> = None;
    let mut try_load = try_load;
    while !stopped() {
        match try_load() {
            Ok(loaded) => {
                if warned.is_some() {
                    log::info!("the client certificate files are there: {} / {}", cert.display(), key.display());
                }
                return Some(loaded);
            }
            Err(e) => {
                if warned.is_none_or(|at| at.elapsed() >= CERT_POLL) {
                    log::warn!(
                        "the client certificate files are not there yet: waiting ({} / {}: {e})",
                        cert.display(),
                        key.display()
                    );
                    warned = Some(std::time::Instant::now());
                }
            }
        }
        std::thread::sleep(retry);
    }
    None
}

/// The certificate files the environment names (`None`: neither, a self-signed certificate):
/// both or neither, by the new names or their old aliases (empty counts as unset).
fn files_from_env(lookup: &impl Fn(&str) -> Option<String>) -> Result<Option<(PathBuf, PathBuf)>, String> {
    let cert = named(lookup, CLIENT_CERT_ENV, WT_CERT_ENV)?;
    let key = named(lookup, CLIENT_KEY_ENV, WT_KEY_ENV)?;
    match (cert, key) {
        (None, None) => Ok(None),
        (Some(cert), Some(key)) => Ok(Some((cert.into(), key.into()))),
        _ => Err(format!("set both {CLIENT_CERT_ENV} and {CLIENT_KEY_ENV} (or {WT_CERT_ENV} and {WT_KEY_ENV}), or neither")),
    }
}

/// A file variable by its new name or its old alias: both set to different values is an error.
fn named(lookup: &impl Fn(&str) -> Option<String>, name: &str, alias: &str) -> Result<Option<String>, String> {
    let get = |n: &str| lookup(n).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    match (get(name), get(alias)) {
        (Some(a), Some(b)) if a != b => Err(format!("{name} ({a}) and its old name {alias} ({b}) disagree: set one")),
        (a, b) => Ok(a.or(b)),
    }
}

fn read_pair(cert: &Path, key: &Path) -> io::Result<(Vec<u8>, Vec<u8>)> {
    Ok((std::fs::read(cert)?, std::fs::read(key)?))
}

/// A PEM chain (leaf first) and the leaf's PEM key, checked to belong together.
fn parse_pair((cert, key): &(Vec<u8>, Vec<u8>)) -> io::Result<CertifiedKey> {
    let invalid = |what: String| io::Error::new(io::ErrorKind::InvalidData, what);
    let chain = CertificateDer::pem_slice_iter(cert)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| invalid(format!("certificate chain: {e}")))?;
    if chain.is_empty() {
        return Err(invalid("no certificate in the chain file".into()));
    }
    let key = PrivateKeyDer::from_pem_slice(key).map_err(|e| invalid(format!("private key: {e}")))?;
    certified(chain, key).map_err(|e| invalid(e.to_string()))
}

/// The pair, its key loaded by ring and matched against the leaf.
fn certified(chain: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>) -> Result<CertifiedKey, rustls::Error> {
    CertifiedKey::from_der(chain, key, &rustls::crypto::ring::default_provider())
}

fn files_error(cert: &Path, key: &Path, e: &io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("client certificate {} / {}: {e}", cert.display(), key.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory for one test's files.
    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("client-cert-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A self-signed pair as PEM (chain, key) and its leaf's DER.
    fn pair(name: &str) -> (String, String, Vec<u8>) {
        let minted = rcgen::generate_simple_self_signed(vec![name.to_string()]).unwrap();
        let der = minted.cert.der().to_vec();
        (pem("CERTIFICATE", &der), pem("PRIVATE KEY", &minted.signing_key.serialize_der()), der)
    }

    fn pem(label: &str, der: &[u8]) -> String {
        use base64::Engine;
        let body = base64::engine::general_purpose::STANDARD.encode(der);
        let lines: Vec<&str> = body.as_bytes().chunks(64).map(|l| std::str::from_utf8(l).unwrap()).collect();
        format!("-----BEGIN {label}-----\n{}\n-----END {label}-----\n", lines.join("\n"))
    }

    fn write(dir: &Path, (cert, key, _): &(String, String, Vec<u8>)) -> (String, String) {
        let (cert_path, key_path) = (dir.join("tls.crt"), dir.join("tls.key"));
        std::fs::write(&cert_path, cert).unwrap();
        std::fs::write(&key_path, key).unwrap();
        (cert_path.display().to_string(), key_path.display().to_string())
    }

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: Vec<(String, String)> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    #[test]
    fn neither_var_means_self_signed_with_a_hash() {
        let cert = ClientCert::from_env(env(&[])).unwrap();
        let hex = cert.sha256_hex().expect("a self-signed certificate is pinned by its hash");
        assert_eq!(hex.len(), 64, "{hex}");
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{hex}");
        let leaf = wtransport::tls::Certificate::from_der(cert.leaf_der()).unwrap();
        let leaf_hex: String = leaf.hash().as_ref().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, leaf_hex, "the hash is the served leaf's");
        assert!(!cert.is_from_files());
        assert_eq!(cert.reload_if_changed().unwrap(), false, "nothing to reload");
        // Empty counts as unset.
        assert!(ClientCert::from_env(env(&[(CLIENT_CERT_ENV, " "), (CLIENT_KEY_ENV, "")])).unwrap().sha256_hex().is_some());
    }

    #[test]
    fn both_vars_mean_files_and_no_hash() {
        let dir = scratch("both");
        let a = pair("a.example");
        let (c, k) = write(&dir, &a);
        let cert = ClientCert::from_env(env(&[(CLIENT_CERT_ENV, &c), (CLIENT_KEY_ENV, &k)])).unwrap();
        assert_eq!(cert.sha256_hex(), None, "a CA's certificate is verified by name, not pinned");
        assert!(cert.is_from_files());
        assert_eq!(cert.leaf_der(), a.2);
        assert_eq!(cert.reload_if_changed().unwrap(), false, "unchanged files");
    }

    #[test]
    fn one_var_alone_is_refused() {
        for vars in [[(CLIENT_CERT_ENV, "/c.crt")], [(CLIENT_KEY_ENV, "/c.key")], [(WT_CERT_ENV, "/c.crt")]] {
            let err = ClientCert::from_env(env(&vars)).err().expect("refused");
            assert!(err.contains(CLIENT_CERT_ENV) && err.contains(CLIENT_KEY_ENV), "{err}");
        }
        // Files that do not exist are refused too (a typo must not run a host nobody reaches).
        assert!(ClientCert::from_env(env(&[(CLIENT_CERT_ENV, "/nonexistent/c.crt"), (CLIENT_KEY_ENV, "/nonexistent/c.key")])).is_err());
    }

    #[test]
    fn the_old_wt_names_still_work() {
        let dir = scratch("old-names");
        let a = pair("a.example");
        let (c, k) = write(&dir, &a);
        let cert = ClientCert::from_env(env(&[(WT_CERT_ENV, &c), (WT_KEY_ENV, &k)])).unwrap();
        assert_eq!(cert.sha256_hex(), None);
        assert_eq!(cert.leaf_der(), a.2);
        // Mixed, and the same value under both names: fine.
        assert!(ClientCert::from_env(env(&[(CLIENT_CERT_ENV, &c), (WT_KEY_ENV, &k)])).is_ok());
        assert!(ClientCert::from_env(env(&[(CLIENT_CERT_ENV, &c), (WT_CERT_ENV, &c), (CLIENT_KEY_ENV, &k)])).is_ok());
        // Different values under the new and the old name: refused.
        let err = ClientCert::from_env(env(&[(CLIENT_CERT_ENV, &c), (WT_CERT_ENV, "/other.crt"), (CLIENT_KEY_ENV, &k)]))
            .err()
            .expect("refused");
        assert!(err.contains(CLIENT_CERT_ENV) && err.contains(WT_CERT_ENV), "{err}");
    }

    #[test]
    fn a_changed_file_is_served_after_reload() {
        let dir = scratch("changed");
        let (a, b) = (pair("a.example"), pair("b.example"));
        let (c, k) = write(&dir, &a);
        let cert = ClientCert::from_env(env(&[(CLIENT_CERT_ENV, &c), (CLIENT_KEY_ENV, &k)])).unwrap();
        let resolver = cert.resolver();
        write(&dir, &b);
        assert_eq!(cert.reload_if_changed().unwrap(), true);
        assert_eq!(cert.leaf_der(), b.2);
        assert_eq!(cert.reload_if_changed().unwrap(), false, "B is loaded already");
        // The resolver handed out before the reload serves B too (it reads the shared pair).
        assert_eq!(resolver.0.current().cert[0].as_ref(), b.2.as_slice());
    }

    #[test]
    fn an_unreadable_renewal_keeps_the_old_pair() {
        let dir = scratch("unreadable");
        let (a, b) = (pair("a.example"), pair("b.example"));
        let (c, k) = write(&dir, &a);
        let cert = ClientCert::from_env(env(&[(CLIENT_CERT_ENV, &c), (CLIENT_KEY_ENV, &k)])).unwrap();
        // Truncated chain.
        std::fs::write(&c, &a.0[..a.0.len() / 2]).unwrap();
        assert!(cert.reload_if_changed().is_err());
        assert_eq!(cert.leaf_der(), a.2);
        // Half a renewal: B's chain beside A's key.
        std::fs::write(&c, &b.0).unwrap();
        assert!(cert.reload_if_changed().is_err(), "a chain and a key that do not match");
        assert_eq!(cert.leaf_der(), a.2);
        // The key arrives: B is served.
        std::fs::write(&k, &b.1).unwrap();
        assert_eq!(cert.reload_if_changed().unwrap(), true);
        assert_eq!(cert.leaf_der(), b.2);
        // A missing file: an error, B kept.
        std::fs::remove_file(&k).unwrap();
        assert!(cert.reload_if_changed().is_err());
        assert_eq!(cert.leaf_der(), b.2);
    }

    #[test]
    fn the_wait_returns_once_the_files_load() {
        let (dir, a) = (scratch("wait-loads"), pair("a.example"));
        let (c, k) = write(&dir, &a);
        let mut attempts = 0;
        let loaded = wait_for_files(Path::new(&c), Path::new(&k), None, Duration::from_millis(1), || {
            attempts += 1;
            match attempts {
                1 | 2 => Err(io::Error::new(io::ErrorKind::NotFound, "not there yet")),
                _ => ClientCert::load_files(c.clone().into(), k.clone().into()),
            }
        });
        assert_eq!(loaded.expect("loaded").leaf_der(), a.2);
        assert_eq!(attempts, 3, "it stops trying once the pair loads");
    }

    #[test]
    fn a_stop_ends_the_wait() {
        let stop = AtomicBool::new(true);
        let mut attempts = 0;
        let loaded = wait_for_files(Path::new("/nonexistent/c"), Path::new("/nonexistent/k"), Some(&stop), Duration::from_millis(1), || {
            attempts += 1;
            Err(io::Error::new(io::ErrorKind::NotFound, "never"))
        });
        assert!(loaded.is_none(), "stopped");
        assert!(attempts <= 1, "{attempts}");
    }

    /// A first issue caught half-written (a chain whose key is not there yet) is waited for, not fatal.
    #[test]
    fn an_invalid_pair_at_start_is_waited_for() {
        let dir = scratch("wait-invalid");
        let (a, b) = (pair("a.example"), pair("b.example"));
        let (c, k) = write(&dir, &a);
        std::fs::write(&c, &b.0).unwrap(); // B's chain beside A's key
        let fix = {
            let (k, key) = (k.clone(), b.1.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                std::fs::write(k, key).unwrap();
            })
        };
        let loaded = wait_for_files(Path::new(&c), Path::new(&k), None, Duration::from_millis(2), || {
            ClientCert::load_files(c.clone().into(), k.clone().into())
        });
        fix.join().unwrap();
        assert_eq!(loaded.expect("loaded").leaf_der(), b.2);
    }

    #[test]
    fn configuration_errors_are_not_waited_for() {
        let err = ClientCert::wait_from_env(env(&[(CLIENT_CERT_ENV, "/c.crt")]), None).err().expect("refused");
        assert!(err.contains(CLIENT_KEY_ENV), "{err}");
        let conflict = env(&[(CLIENT_CERT_ENV, "/c.crt"), (WT_CERT_ENV, "/d.crt"), (CLIENT_KEY_ENV, "/c.key")]);
        assert!(ClientCert::wait_from_env(conflict, None).is_err());
        let self_signed = ClientCert::wait_from_env(env(&[]), None).unwrap().expect("no files: no wait");
        assert!(self_signed.sha256_hex().is_some());
        let dir = scratch("wait-env");
        let a = pair("a.example");
        let (c, k) = write(&dir, &a);
        let files = ClientCert::wait_from_env(env(&[(WT_CERT_ENV, &c), (WT_KEY_ENV, &k)]), None).unwrap().expect("loads");
        assert_eq!(files.leaf_der(), a.2);
    }

    /// A reload whose read straddled a symlink swap (new chain, old key) reads again after a
    /// short pause instead of waiting a whole poll.
    #[test]
    fn a_mismatched_reload_is_read_again_once() {
        let dir = scratch("reload-retry");
        let (a, b) = (pair("a.example"), pair("b.example"));
        let (c, k) = write(&dir, &a);
        let cert = ClientCert::from_env(env(&[(CLIENT_CERT_ENV, &c), (CLIENT_KEY_ENV, &k)])).unwrap();
        std::fs::write(&c, &b.0).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let reloaded = rt.block_on(async {
            let (k, key) = (k.clone(), b.1.clone());
            let swap = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                std::fs::write(k, key).unwrap();
            });
            let reloaded = cert.reload_with_retry(Duration::from_millis(50)).await;
            swap.await.unwrap();
            reloaded
        });
        assert_eq!(reloaded.unwrap(), true);
        assert_eq!(cert.leaf_der(), b.2);
        // Still mismatched after the re-read: an error, the old pair kept.
        std::fs::write(&c, &a.0).unwrap();
        let again = rt.block_on(cert.reload_with_retry(Duration::from_millis(1)));
        assert!(again.is_err());
        assert_eq!(cert.leaf_der(), b.2);
    }
}
