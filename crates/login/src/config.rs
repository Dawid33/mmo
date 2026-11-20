//! [`LoginConfig`]: the login service's settings, from the environment.

use std::net::SocketAddr;
use std::path::PathBuf;

/// `VOXEL_LOGIN_BIND` unset.
pub const DEFAULT_BIND: &str = "127.0.0.1:8790";
/// `VOXEL_LOGIN_DB` unset: a SQLite file for local dev.
pub const DEFAULT_DB: &str = "target/dev-login/login.sqlite";
/// `VOXEL_LOGIN_KEYS_DIR` unset.
pub const DEFAULT_KEYS_DIR: &str = "target/dev-login/keys";
/// `VOXEL_LOGIN_CORS_ORIGINS` unset: the dev browser build's page (`just web`), by IP or as
/// `localhost` (browsers treat the two as different origins).
pub const DEFAULT_CORS_ORIGINS: &str = "http://127.0.0.1:1334,http://localhost:1334";

/// `VOXEL_OIDC_AUDIENCE` unset.
pub const DEFAULT_OIDC_AUDIENCE: &str = "mmo-login";
/// `VOXEL_OIDC_CLIENT_ID` unset.
pub const DEFAULT_OIDC_CLIENT_ID: &str = "game";

/// The external identity provider (OpenID Connect) the service logs players in with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OidcConfig {
    /// The provider's issuer URL (`VOXEL_OIDC_ISSUER`); its discovery document is fetched at start.
    pub issuer: String,
    /// The `aud` an access token must carry (`VOXEL_OIDC_AUDIENCE`).
    pub audience: String,
    /// The public client id the game logs in as (`VOXEL_OIDC_CLIENT_ID`), told to clients by `/v1/config`.
    pub client_id: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct LoginConfig {
    pub bind: SocketAddr,
    /// A `postgres://` URL, `sqlite::memory:`, `sqlite:<path>` or a bare SQLite path.
    pub db_url: String,
    /// The PKCS#8 PEM signing keys (`login keygen` writes them).
    pub keys_dir: PathBuf,
    /// Exact origins a browser may call the API from.
    pub cors_origins: Vec<String>,
    /// `None` (`VOXEL_OIDC_ISSUER` unset): no identity-provider login.
    pub oidc: Option<OidcConfig>,
}

impl LoginConfig {
    /// From the process environment (see [`LoginConfig::from_lookup`]).
    pub fn from_env() -> Result<LoginConfig, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// From `lookup(name)`; an unset or empty variable takes its default.
    /// `VOXEL_LOGIN_BIND`, `VOXEL_LOGIN_DB`, `VOXEL_LOGIN_KEYS_DIR`,
    /// `VOXEL_LOGIN_CORS_ORIGINS` (comma list), and
    /// `VOXEL_OIDC_ISSUER` (unset = off), `VOXEL_OIDC_AUDIENCE` (`mmo-login`), `VOXEL_OIDC_CLIENT_ID` (`game`).
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<LoginConfig, String> {
        let get = |k: &str, default: &str| {
            lookup(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        let bind_s = get("VOXEL_LOGIN_BIND", DEFAULT_BIND);
        let bind = bind_s
            .parse()
            .map_err(|e| format!("VOXEL_LOGIN_BIND={bind_s:?}: {e} (want host:port)"))?;
        let cors_origins = get("VOXEL_LOGIN_CORS_ORIGINS", DEFAULT_CORS_ORIGINS)
            .split(',')
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let oidc = Some(get("VOXEL_OIDC_ISSUER", ""))
            .filter(|i| !i.is_empty())
            .map(|issuer| OidcConfig {
                issuer,
                audience: get("VOXEL_OIDC_AUDIENCE", DEFAULT_OIDC_AUDIENCE),
                client_id: get("VOXEL_OIDC_CLIENT_ID", DEFAULT_OIDC_CLIENT_ID),
            });
        Ok(LoginConfig {
            bind,
            db_url: get("VOXEL_LOGIN_DB", DEFAULT_DB),
            keys_dir: PathBuf::from(get("VOXEL_LOGIN_KEYS_DIR", DEFAULT_KEYS_DIR)),
            cors_origins,
            oidc,
        })
    }
}

/// Shown in place of a database URL's password.
pub const REDACTED: &str = "<redacted>";

/// `url` with the password of its userinfo replaced by [`REDACTED`]
/// (`postgres://u:pw@h/db` -> `postgres://u:<redacted>@h/db`); URLs without one are unchanged.
pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((userinfo, host)) => match userinfo.split_once(':') {
            Some((user, _)) => format!("{scheme}://{user}:{REDACTED}@{host}{tail}"),
            None => url.to_string(),
        },
        None => url.to_string(),
    }
}

impl std::fmt::Debug for LoginConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginConfig")
            .field("bind", &self.bind)
            .field("db_url", &redact_url(&self.db_url))
            .field("keys_dir", &self.keys_dir)
            .field("cors_origins", &self.cors_origins)
            .field("oidc", &self.oidc)
            .finish()
    }
}
