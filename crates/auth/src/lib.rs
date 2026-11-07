//! Join JWTs: EdDSA (Ed25519) signing on the login service, verification on the game hosts.
//!
//! The login service keeps its private keys as PKCS#8 PEM files in one directory ([`Signer::load_dir`]):
//! the newest file by name signs, all of them are published as a [`Jwks`] so a rotated-out key's
//! tokens still verify until they expire. A host builds a [`Verifier`] from the JWKS and checks a
//! [`JoinClaims`] JWT at connect with [`Verifier::verify`], with the time passed in.
//!
//! A key's `kid` is the first [`KID_HEX_CHARS`] lowercase hex characters of SHA-256 over its raw
//! 32-byte public key.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey, spki::der::pem::LineEnding};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use login_api::{HOST_AUDIENCE, JoinClaims};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Clock skew allowed past a JWT's `exp`.
pub const JWT_LEEWAY_SECS: u64 = 30;
/// Length of a key id: hex characters of the public key's SHA-256.
pub const KID_HEX_CHARS: usize = 16;
/// The file extension [`Signer::load_dir`] reads.
pub const KEY_FILE_EXTENSION: &str = "pem";

/// One Ed25519 private key with its `kid`.
pub struct SigningKey {
    key: ed25519_dalek::SigningKey,
    encoding: EncodingKey,
    kid: String,
}

impl SigningKey {
    /// Reads a PKCS#8 PEM private key.
    pub fn from_pkcs8_pem(pem: &str) -> io::Result<SigningKey> {
        let key = ed25519_dalek::SigningKey::from_pkcs8_pem(pem).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("not an Ed25519 PKCS#8 PEM key: {e}"),
            )
        })?;
        Self::from_dalek(key)
    }

    /// A new random key and its PKCS#8 PEM encoding (to write to the keys directory).
    pub fn generate() -> (SigningKey, String) {
        let key = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng);
        let pem = key
            .to_pkcs8_pem(LineEnding::LF)
            .expect("encode an Ed25519 key as PKCS#8")
            .to_string();
        (
            Self::from_dalek(key).expect("a freshly generated key encodes"),
            pem,
        )
    }

    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The public half, as a JWK.
    pub fn jwk(&self) -> Jwk {
        Jwk {
            kty: JWK_KTY.into(),
            crv: JWK_CRV.into(),
            x: URL_SAFE_NO_PAD.encode(self.key.verifying_key().as_bytes()),
            kid: self.kid.clone(),
            alg: JWK_ALG.into(),
            use_: JWK_USE.into(),
        }
    }

    fn from_dalek(key: ed25519_dalek::SigningKey) -> io::Result<SigningKey> {
        let der = key
            .to_pkcs8_der()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("encode key: {e}")))?;
        let encoding = EncodingKey::from_ed_der(der.as_bytes());
        let kid = kid_of(key.verifying_key().as_bytes());
        Ok(SigningKey { key, encoding, kid })
    }
}

/// `kid` of a raw 32-byte Ed25519 public key.
fn kid_of(public: &[u8; 32]) -> String {
    let digest = Sha256::digest(public);
    let mut hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex.truncate(KID_HEX_CHARS);
    hex
}

/// The login service's keys: the last signs, all are published.
pub struct Signer {
    keys: Vec<SigningKey>,
}

impl Signer {
    /// Reads every `*.pem` in `dir`, sorted by file name; the last one is the signing key.
    /// An empty directory is an error.
    pub fn load_dir(dir: &Path) -> io::Result<Signer> {
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_file() && path.extension().is_some_and(|e| e == KEY_FILE_EXTENSION) {
                paths.push(path);
            }
        }
        paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        let mut keys = Vec::with_capacity(paths.len());
        for path in &paths {
            let pem = std::fs::read_to_string(path)?;
            let key = SigningKey::from_pkcs8_pem(&pem)
                .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
            keys.push(key);
        }
        Self::new(keys).map_err(|_| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "no *.{KEY_FILE_EXTENSION} signing key in {} (run `login keygen`)",
                    dir.display()
                ),
            )
        })
    }

    /// A signer over `keys`, oldest first; the last signs. Empty is refused.
    pub fn new(keys: Vec<SigningKey>) -> Result<Signer, Vec<SigningKey>> {
        if keys.is_empty() {
            Err(keys)
        } else {
            Ok(Signer { keys })
        }
    }

    /// The key that signs.
    pub fn signing_key(&self) -> &SigningKey {
        self.keys.last().expect("a Signer holds at least one key")
    }

    /// Signs `claims` with the newest key, its `kid` in the header.
    pub fn sign(&self, claims: &JoinClaims) -> String {
        let key = self.signing_key();
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some(key.kid.clone());
        jsonwebtoken::encode(&header, claims, &key.encoding).expect("sign a join JWT")
    }

    /// Every key's public half, oldest first.
    pub fn jwks(&self) -> Jwks {
        Jwks {
            keys: self.keys.iter().map(SigningKey::jwk).collect(),
        }
    }
}

const JWK_KTY: &str = "OKP";
const JWK_CRV: &str = "Ed25519";
const JWK_ALG: &str = "EdDSA";
const JWK_USE: &str = "sig";

/// `GET /.well-known/jwks.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Jwks {
    pub keys: Vec<Jwk>,
}

/// An Ed25519 public key (RFC 8037).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Jwk {
    /// `"OKP"`.
    pub kty: String,
    /// `"Ed25519"`.
    pub crv: String,
    /// The raw public key, base64url without padding.
    pub x: String,
    pub kid: String,
    /// `"EdDSA"`.
    pub alg: String,
    /// `"sig"`.
    #[serde(rename = "use")]
    pub use_: String,
}

/// Why a join JWT was refused. `Display` is the refusal text sent to the client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyError {
    Malformed,
    UnknownKid(String),
    BadSignature,
    Expired,
    WrongAudience,
    WrongWorld { got: String },
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerifyError::Malformed => write!(f, "malformed login ticket"),
            VerifyError::UnknownKid(kid) => {
                write!(f, "login ticket signed by an unknown key ({kid})")
            }
            VerifyError::BadSignature => write!(f, "login ticket signature is invalid"),
            VerifyError::Expired => write!(f, "login ticket expired"),
            VerifyError::WrongAudience => write!(f, "login ticket is not for a game host"),
            VerifyError::WrongWorld { got } => {
                write!(f, "login ticket is for another world ({got})")
            }
        }
    }
}

impl std::error::Error for VerifyError {}

/// Checks join JWTs against the published keys.
pub struct Verifier {
    by_kid: BTreeMap<String, DecodingKey>,
}

impl Verifier {
    /// A verifier knowing every Ed25519 key of `jwks` (others are ignored).
    pub fn from_jwks(jwks: &Jwks) -> Verifier {
        let by_kid = jwks
            .keys
            .iter()
            .filter(|k| k.kty == JWK_KTY && k.crv == JWK_CRV)
            .filter_map(|k| {
                DecodingKey::from_ed_components(&k.x)
                    .ok()
                    .map(|d| (k.kid.clone(), d))
            })
            .collect();
        Verifier { by_kid }
    }

    /// Whether `kid` is known (a host refetches the JWKS when it is not).
    pub fn knows(&self, kid: &str) -> bool {
        self.by_kid.contains_key(kid)
    }

    /// Checks signature, `exp` (with [`JWT_LEEWAY_SECS`]) at `now_secs`, `aud` and `world`.
    pub fn verify(
        &self,
        jwt: &str,
        now_secs: u64,
        expect_world: &str,
    ) -> Result<JoinClaims, VerifyError> {
        let header = jsonwebtoken::decode_header(jwt).map_err(|_| VerifyError::Malformed)?;
        if header.alg != Algorithm::EdDSA {
            return Err(VerifyError::Malformed);
        }
        let kid = header.kid.ok_or(VerifyError::Malformed)?;
        let key = self
            .by_kid
            .get(&kid)
            .ok_or_else(|| VerifyError::UnknownKid(kid.clone()))?;
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.validate_aud = false;
        validation.required_spec_claims.clear();
        let claims = jsonwebtoken::decode::<JoinClaims>(jwt, key, &validation)
            .map_err(|e| match e.kind() {
                jsonwebtoken::errors::ErrorKind::InvalidSignature => VerifyError::BadSignature,
                _ => VerifyError::Malformed,
            })?
            .claims;
        if now_secs > claims.exp.saturating_add(JWT_LEEWAY_SECS) {
            return Err(VerifyError::Expired);
        }
        if claims.aud != HOST_AUDIENCE {
            return Err(VerifyError::WrongAudience);
        }
        if claims.world != expect_world {
            return Err(VerifyError::WrongWorld { got: claims.world });
        }
        Ok(claims)
    }
}
