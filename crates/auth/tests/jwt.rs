use std::path::PathBuf;

use auth::{JWT_LEEWAY_SECS, Jwks, Signer, SigningKey, Verifier, VerifyError};
use login_api::{HOST_AUDIENCE, JOIN_JWT_TTL_SECS, JoinClaims};

const NOW: u64 = 1_800_000_000;
const WORLD: &str = "alpha";

fn claims() -> JoinClaims {
    JoinClaims {
        sub: "42".into(),
        world: WORLD.into(),
        player_token: "00112233445566778899aabbccddeeff".into(),
        session_epoch: 3,
        aud: HOST_AUDIENCE.into(),
        iat: NOW,
        exp: NOW + JOIN_JWT_TTL_SECS,
    }
}

/// A fresh, unique temp dir (removed by the caller's guard).
struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("auth-test-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn signer_with(pems: &[&str]) -> (Signer, TempDir) {
    let dir = TempDir::new("keys");
    for (i, pem) in pems.iter().enumerate() {
        std::fs::write(dir.0.join(format!("{:04}.pem", i + 1)), pem).unwrap();
    }
    (Signer::load_dir(&dir.0).unwrap(), dir)
}

fn one_signer() -> (Signer, TempDir) {
    let (_, pem) = SigningKey::generate();
    signer_with(&[&pem])
}

fn header_kid(jwt: &str) -> String {
    jsonwebtoken::decode_header(jwt).unwrap().kid.unwrap()
}

#[test]
fn sign_then_verify_round_trips() {
    let (signer, _dir) = one_signer();
    let jwt = signer.sign(&claims());
    let verifier = Verifier::from_jwks(&signer.jwks());
    assert_eq!(verifier.verify(&jwt, NOW, WORLD).unwrap(), claims());
}

#[test]
fn expired_is_refused() {
    let (signer, _dir) = one_signer();
    let jwt = signer.sign(&claims());
    let verifier = Verifier::from_jwks(&signer.jwks());
    let exp = claims().exp;
    assert!(verifier.verify(&jwt, exp + JWT_LEEWAY_SECS, WORLD).is_ok());
    assert_eq!(
        verifier.verify(&jwt, exp + JWT_LEEWAY_SECS + 1, WORLD),
        Err(VerifyError::Expired)
    );
}

#[test]
fn wrong_world_is_refused() {
    let (signer, _dir) = one_signer();
    let jwt = signer.sign(&claims());
    let verifier = Verifier::from_jwks(&signer.jwks());
    assert_eq!(
        verifier.verify(&jwt, NOW, "beta"),
        Err(VerifyError::WrongWorld { got: WORLD.into() })
    );
}

#[test]
fn wrong_audience_is_refused() {
    let (signer, _dir) = one_signer();
    let jwt = signer.sign(&JoinClaims {
        aud: "someone-else".into(),
        ..claims()
    });
    let verifier = Verifier::from_jwks(&signer.jwks());
    assert_eq!(
        verifier.verify(&jwt, NOW, WORLD),
        Err(VerifyError::WrongAudience)
    );
}

#[test]
fn other_key_is_bad_signature() {
    let (signer_a, _da) = one_signer();
    let (signer_b, _db) = one_signer();
    let jwt = signer_a.sign(&claims());
    // Key B published under key A's kid.
    let mut jwks = signer_b.jwks();
    jwks.keys[0].kid = header_kid(&jwt);
    let verifier = Verifier::from_jwks(&jwks);
    assert_eq!(
        verifier.verify(&jwt, NOW, WORLD),
        Err(VerifyError::BadSignature)
    );
}

#[test]
fn unknown_kid() {
    let (signer_a, _da) = one_signer();
    let (signer_b, _db) = one_signer();
    let jwt = signer_a.sign(&claims());
    let verifier = Verifier::from_jwks(&signer_b.jwks());
    assert_eq!(
        verifier.verify(&jwt, NOW, WORLD),
        Err(VerifyError::UnknownKid(header_kid(&jwt)))
    );
}

#[test]
fn malformed_is_refused() {
    let (signer, _dir) = one_signer();
    let verifier = Verifier::from_jwks(&signer.jwks());
    assert_eq!(
        verifier.verify("not-a-jwt", NOW, WORLD),
        Err(VerifyError::Malformed)
    );
    assert_eq!(
        verifier.verify("a.b.c", NOW, WORLD),
        Err(VerifyError::Malformed)
    );
}

#[test]
fn jwks_round_trips_json() {
    let (signer, _dir) = one_signer();
    let json = serde_json::to_string(&signer.jwks()).unwrap();
    assert!(json.contains("\"use\":\"sig\""), "{json}");
    assert!(
        json.contains("\"kty\":\"OKP\"")
            && json.contains("\"crv\":\"Ed25519\"")
            && json.contains("\"alg\":\"EdDSA\"")
    );
    let parsed: Jwks = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, signer.jwks());
    let jwt = signer.sign(&claims());
    assert_eq!(
        Verifier::from_jwks(&parsed)
            .verify(&jwt, NOW, WORLD)
            .unwrap(),
        claims()
    );
}

#[test]
fn load_dir_signs_with_newest() {
    let (key1, pem1) = SigningKey::generate();
    let (key2, pem2) = SigningKey::generate();
    let (signer, _dir) = signer_with(&[&pem1, &pem2]);
    let jwt = signer.sign(&claims());
    assert_eq!(header_kid(&jwt), key2.kid());
    // Both keys stay published, so tokens of the older key still verify.
    let kids: Vec<_> = signer.jwks().keys.into_iter().map(|k| k.kid).collect();
    assert_eq!(kids, vec![key1.kid().to_string(), key2.kid().to_string()]);
}

#[test]
fn load_dir_refuses_empty_dir() {
    let dir = TempDir::new("empty");
    assert!(Signer::load_dir(&dir.0).is_err());
}

#[test]
fn kid_is_sha256_prefix() {
    let (key, pem) = SigningKey::generate();
    let kid = key.kid();
    assert_eq!(kid.len(), 16);
    assert!(
        kid.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{kid}"
    );
    // Stable across a PEM round trip.
    assert_eq!(SigningKey::from_pkcs8_pem(&pem).unwrap().kid(), kid);
}
