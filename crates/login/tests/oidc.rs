use jsonwebtoken::{Algorithm, EncodingKey, Header};
use login::oidc::{Identity, OidcError, OidcVerifier};
use serde_json::{Value, json};

const ISSUER: &str = "https://auth.example/realms/mmo";
const AUDIENCE: &str = "mmo-login";
const NOW: u64 = 1_800_000_000;

const KEY_PEM: &str = include_str!("fixtures/oidc_rsa.pem");
const JWKS: &str = include_str!("fixtures/oidc_jwks.json");

fn verifier() -> OidcVerifier {
    OidcVerifier::from_jwks(ISSUER, AUDIENCE, JWKS).unwrap()
}

fn claims() -> Value {
    json!({
        "iss": ISSUER, "aud": AUDIENCE, "exp": NOW + 300, "sub": "u-1",
        "email": " Ann@X.io ", "email_verified": true, "preferred_username": "ann",
    })
}

fn with(key: &str, value: Value) -> Value {
    let mut c = claims();
    c[key] = value;
    c
}

/// Signs `claims` with the fixture key (RS256) under key id `kid`.
fn token(claims: Value, kid: &str) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.to_string());
    jsonwebtoken::encode(&header, &claims, &EncodingKey::from_rsa_pem(KEY_PEM.as_bytes()).unwrap()).unwrap()
}

fn check(claims: Value) -> Result<Identity, OidcError> {
    verifier().verify(&token(claims, "test-1"), NOW)
}

#[test]
fn accepts_a_good_token() {
    assert_eq!(
        check(claims()).unwrap(),
        Identity { subject: "u-1".into(), email: "ann@x.io".into(), name: "ann".into() }
    );
}

#[test]
fn refuses_wrong_audience() {
    assert_eq!(check(with("aud", json!("account"))), Err(OidcError::WrongAudience));
    let mut c = claims();
    c.as_object_mut().unwrap().remove("aud");
    assert_eq!(check(c), Err(OidcError::WrongAudience));
}

#[test]
fn accepts_audience_array() {
    assert!(check(with("aud", json!(["account", "mmo-login"]))).is_ok());
    assert_eq!(check(with("aud", json!(["account"]))), Err(OidcError::WrongAudience));
}

#[test]
fn refuses_wrong_issuer() {
    assert_eq!(check(with("iss", json!("https://evil.example/realms/mmo"))), Err(OidcError::WrongIssuer));
}

#[test]
fn expiry_has_thirty_seconds_of_leeway() {
    assert_eq!(check(with("exp", json!(NOW - 31))), Err(OidcError::Expired));
    assert!(check(with("exp", json!(NOW - 29))).is_ok());
    assert!(check(with("exp", json!(NOW - 30))).is_ok());
}

#[test]
fn refuses_unverified_email() {
    assert_eq!(check(with("email_verified", json!(false))), Err(OidcError::EmailUnverified));
    let mut c = claims();
    c.as_object_mut().unwrap().remove("email_verified");
    assert_eq!(check(c), Err(OidcError::EmailUnverified));
}

#[test]
fn refuses_missing_claims() {
    for name in ["sub", "email", "preferred_username", "exp"] {
        let mut c = claims();
        c.as_object_mut().unwrap().remove(name);
        let want = match name {
            "sub" => "sub",
            "email" => "email",
            "exp" => "exp",
            _ => "preferred_username",
        };
        assert_eq!(check(c), Err(OidcError::MissingClaim(want)), "{name}");
    }
    assert_eq!(check(with("preferred_username", json!(""))), Err(OidcError::MissingClaim("preferred_username")));
}

#[test]
fn refuses_hs256_token() {
    // The classic confusion attack: the public modulus as an HMAC secret.
    let n = serde_json::from_str::<Value>(JWKS).unwrap()["keys"][0]["n"].as_str().unwrap().to_string();
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some("test-1".into());
    let forged = jsonwebtoken::encode(&header, &claims(), &EncodingKey::from_secret(n.as_bytes())).unwrap();
    let got = verifier().verify(&forged, NOW);
    assert!(matches!(got, Err(OidcError::Malformed | OidcError::BadSignature)), "{got:?}");
}

#[test]
fn refuses_a_tampered_token() {
    let t = token(claims(), "test-1");
    let mut parts: Vec<&str> = t.split('.').collect();
    let other = token(with("sub", json!("u-2")), "test-1");
    let other_payload = other.split('.').nth(1).unwrap();
    parts[1] = other_payload;
    assert_eq!(verifier().verify(&parts.join("."), NOW), Err(OidcError::BadSignature));
}

#[test]
fn refuses_garbage_and_missing_kid() {
    assert_eq!(verifier().verify("not a token", NOW), Err(OidcError::Malformed));
    let no_kid = jsonwebtoken::encode(
        &Header::new(Algorithm::RS256),
        &claims(),
        &EncodingKey::from_rsa_pem(KEY_PEM.as_bytes()).unwrap(),
    )
    .unwrap();
    assert_eq!(verifier().verify(&no_kid, NOW), Err(OidcError::Malformed));
}

#[test]
fn unknown_kid_is_reported() {
    assert_eq!(verifier().verify(&token(claims(), "test-2"), NOW), Err(OidcError::UnknownKid));
}

#[tokio::test]
async fn a_fixed_key_set_never_refetches() {
    let got = verifier().verify_refreshing(&token(claims(), "test-2"), NOW).await;
    assert_eq!(got, Err(OidcError::UnknownKid));
}
