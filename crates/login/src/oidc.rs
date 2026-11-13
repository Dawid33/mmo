//! Verifying the identity provider's (Keycloak's) access token: RS256 against the provider's
//! published keys (JWKS), then issuer, audience, expiry and a verified email.
//!
//! A token that should be refused never comes back `Ok`: the algorithm is fixed to RS256 (never
//! taken from the token's header), the signature is checked before any claim is trusted, and every
//! claim check runs.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use auth::JWT_LEEWAY_SECS;
use base64::Engine;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;

use crate::store::normalize_email;

/// The keys are refetched (on an unknown `kid`) at most this often.
pub const JWKS_REFETCH_MIN_SECS: u64 = 30;
/// Timeout of each request to the identity provider.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Who a verified token belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The provider's stable user id (`sub`).
    pub subject: String,
    /// Normalized (trimmed, lowercased), and verified by the provider.
    pub email: String,
    /// `preferred_username`.
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OidcError {
    Malformed,
    UnknownKid,
    BadSignature,
    Expired,
    WrongIssuer,
    WrongAudience,
    EmailUnverified,
    MissingClaim(&'static str),
    Fetch(String),
}

impl std::fmt::Display for OidcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingClaim(c) => write!(f, "token lacks claim {c}"),
            Self::Fetch(why) => write!(f, "fetching the provider's keys: {why}"),
            other => write!(f, "{other:?}"),
        }
    }
}

impl std::error::Error for OidcError {}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct Claims {
    iss: Option<String>,
    aud: Option<Audience>,
    exp: Option<u64>,
    sub: Option<String>,
    email: Option<String>,
    email_verified: Option<bool>,
    preferred_username: Option<String>,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    kid: Option<String>,
    alg: Option<String>,
    #[serde(rename = "use")]
    use_: Option<String>,
    n: Option<String>,
    e: Option<String>,
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
}

struct Keys {
    by_kid: HashMap<String, DecodingKey>,
    /// When the keys were last fetched (or a fetch last started): the refetch rate limit.
    last_fetch: Instant,
}

pub struct OidcVerifier {
    issuer: String,
    audience: String,
    /// Where to refetch the keys from; `None` for keys given directly ([`Self::from_jwks`]).
    jwks_uri: Option<String>,
    /// Never held across an await.
    keys: RwLock<Keys>,
}

fn parse_jwks(jwks_json: &str) -> Result<HashMap<String, DecodingKey>, OidcError> {
    let jwks: Jwks = serde_json::from_str(jwks_json).map_err(|e| OidcError::Fetch(format!("keys document: {e}")))?;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let mut by_kid = HashMap::new();
    for key in jwks.keys {
        // Keycloak also publishes encryption keys; only RSA signature keys count.
        if key.kty != "RSA" || key.use_.as_deref().is_some_and(|u| u != "sig") || key.alg.as_deref().is_some_and(|a| a != "RS256") {
            continue;
        }
        let (Some(kid), Some(n), Some(e)) = (key.kid, key.n, key.e) else { continue };
        // Validate the encoding ourselves: `from_rsa_components` takes the base64url text as is.
        if ![&n, &e].iter().all(|part| b64.decode(part.as_bytes()).is_ok()) {
            return Err(OidcError::Fetch(format!("key {kid}: bad base64url")));
        }
        let decoding = DecodingKey::from_rsa_components(&n, &e).map_err(|e| OidcError::Fetch(format!("key {kid}: {e}")))?;
        by_kid.insert(kid, decoding);
    }
    Ok(by_kid)
}

fn http() -> Result<reqwest::Client, OidcError> {
    reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| OidcError::Fetch(e.to_string()))
}

async fn get_text(client: &reqwest::Client, url: &str) -> Result<String, OidcError> {
    let fail = |e: reqwest::Error| OidcError::Fetch(format!("{url}: {e}"));
    client.get(url).send().await.map_err(fail)?.error_for_status().map_err(fail)?.text().await.map_err(fail)
}

impl OidcVerifier {
    /// A verifier over a fixed key set (tests; it can never refetch).
    pub fn from_jwks(issuer: &str, audience: &str, jwks_json: &str) -> Result<Self, OidcError> {
        Ok(Self::with_keys(issuer, audience, None, parse_jwks(jwks_json)?))
    }

    fn with_keys(issuer: &str, audience: &str, jwks_uri: Option<String>, by_kid: HashMap<String, DecodingKey>) -> Self {
        Self {
            issuer: issuer.to_string(),
            audience: audience.to_string(),
            jwks_uri,
            keys: RwLock::new(Keys { by_kid, last_fetch: Instant::now() }),
        }
    }

    /// Fetch `{issuer}/.well-known/openid-configuration`, then the keys at its `jwks_uri`.
    pub async fn discover(issuer: &str, audience: &str) -> Result<Self, OidcError> {
        let client = http()?;
        let url = format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'));
        let config: Discovery = serde_json::from_str(&get_text(&client, &url).await?)
            .map_err(|e| OidcError::Fetch(format!("{url}: {e}")))?;
        if config.issuer != issuer {
            return Err(OidcError::Fetch(format!("{url}: names issuer {}", config.issuer)));
        }
        let by_kid = parse_jwks(&get_text(&client, &config.jwks_uri).await?)?;
        Ok(Self::with_keys(issuer, audience, Some(config.jwks_uri), by_kid))
    }

    /// Check `access_token` at time `now_secs`.
    pub fn verify(&self, access_token: &str, now_secs: u64) -> Result<Identity, OidcError> {
        let header = jsonwebtoken::decode_header(access_token).map_err(|_| OidcError::Malformed)?;
        if header.alg != Algorithm::RS256 {
            return Err(OidcError::Malformed);
        }
        let kid = header.kid.ok_or(OidcError::Malformed)?;
        // The algorithm comes from here, never from the token.
        let mut validation = Validation::new(Algorithm::RS256);
        // `exp` is checked below against `now_secs` (the library would read the real clock, and
        // tests need to pass the time in); issuer and audience likewise, for distinct errors.
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.validate_aud = false;
        validation.required_spec_claims.clear();
        let claims = {
            let keys = self.keys.read().unwrap_or_else(|e| e.into_inner());
            let key = keys.by_kid.get(&kid).ok_or(OidcError::UnknownKid)?;
            jsonwebtoken::decode::<Claims>(access_token, key, &validation)
                .map_err(|e| match e.kind() {
                    jsonwebtoken::errors::ErrorKind::InvalidSignature => OidcError::BadSignature,
                    _ => OidcError::Malformed,
                })?
                .claims
        };
        let exp = claims.exp.ok_or(OidcError::MissingClaim("exp"))?;
        if now_secs > exp.saturating_add(JWT_LEEWAY_SECS) {
            return Err(OidcError::Expired);
        }
        if claims.iss.as_deref() != Some(self.issuer.as_str()) {
            return Err(OidcError::WrongIssuer);
        }
        let audience_ok = match &claims.aud {
            Some(Audience::One(a)) => *a == self.audience,
            Some(Audience::Many(list)) => list.iter().any(|a| *a == self.audience),
            None => false,
        };
        if !audience_ok {
            return Err(OidcError::WrongAudience);
        }
        if claims.email_verified != Some(true) {
            return Err(OidcError::EmailUnverified);
        }
        let present = |v: Option<String>, name: &'static str| match v {
            Some(s) if !s.trim().is_empty() => Ok(s),
            _ => Err(OidcError::MissingClaim(name)),
        };
        let subject = present(claims.sub, "sub")?;
        let email = normalize_email(&present(claims.email, "email")?).map_err(|_| OidcError::MissingClaim("email"))?;
        let name = present(claims.preferred_username, "preferred_username")?;
        Ok(Identity { subject, email, name })
    }

    /// [`Self::verify`]; on an unknown `kid` the provider's keys are refetched (at most once per
    /// [`JWKS_REFETCH_MIN_SECS`]) and the token is checked again.
    pub async fn verify_refreshing(&self, access_token: &str, now_secs: u64) -> Result<Identity, OidcError> {
        match self.verify(access_token, now_secs) {
            Err(OidcError::UnknownKid) => {}
            other => return other,
        }
        let Some(uri) = &self.jwks_uri else { return Err(OidcError::UnknownKid) };
        {
            // Claim the refetch before the request, so a failing provider is not hammered either.
            let mut keys = self.keys.write().unwrap_or_else(|e| e.into_inner());
            if keys.last_fetch.elapsed() < Duration::from_secs(JWKS_REFETCH_MIN_SECS) {
                return Err(OidcError::UnknownKid);
            }
            keys.last_fetch = Instant::now();
        }
        let by_kid = parse_jwks(&get_text(&http()?, uri).await?)?;
        self.keys.write().unwrap_or_else(|e| e.into_inner()).by_kid = by_kid;
        self.verify(access_token, now_secs)
    }
}
