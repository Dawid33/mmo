//! [`LoginConfig`]: the login service's settings, from the environment.

use std::net::SocketAddr;
use std::path::PathBuf;

/// `VOXEL_LOGIN_BIND` unset.
pub const DEFAULT_BIND: &str = "127.0.0.1:8790";
/// `VOXEL_LOGIN_DB` unset: a SQLite file for local dev.
pub const DEFAULT_DB: &str = "target/dev-login/login.sqlite";
/// `VOXEL_LOGIN_KEYS_DIR` unset.
pub const DEFAULT_KEYS_DIR: &str = "target/dev-login/keys";
/// `VOXEL_LOGIN_PUBLIC_URL` unset: where mailed links point.
pub const DEFAULT_PUBLIC_URL: &str = "http://127.0.0.1:8790";
/// `VOXEL_LOGIN_CORS_ORIGINS` unset: the dev browser build's page (`just web`), by IP or as
/// `localhost` (browsers treat the two as different origins).
pub const DEFAULT_CORS_ORIGINS: &str = "http://127.0.0.1:1334,http://localhost:1334";

/// How mail goes out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MailerKind {
    /// `LogMailer`: logs the link. Dev only — the link carries a single-use token.
    Log,
    /// Amazon SES v2, sending as `from` (`VOXEL_MAIL_FROM`).
    Ses { from: String },
}

#[derive(Clone, PartialEq, Eq)]
pub struct LoginConfig {
    pub bind: SocketAddr,
    /// A `postgres://` URL, `sqlite::memory:`, `sqlite:<path>` or a bare SQLite path.
    pub db_url: String,
    /// The PKCS#8 PEM signing keys (`login keygen` writes them).
    pub keys_dir: PathBuf,
    /// The service's public base URL, for links in mail.
    pub public_url: String,
    /// Exact origins a browser may call the API from.
    pub cors_origins: Vec<String>,
    /// Refuse login / refresh / join until the email is verified.
    pub require_verified: bool,
    pub mailer: MailerKind,
}

impl LoginConfig {
    /// From the process environment (see [`LoginConfig::from_lookup`]).
    pub fn from_env() -> Result<LoginConfig, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// From `lookup(name)`; an unset or empty variable takes its default.
    /// `VOXEL_LOGIN_BIND`, `VOXEL_LOGIN_DB`, `VOXEL_LOGIN_KEYS_DIR`, `VOXEL_LOGIN_PUBLIC_URL`,
    /// `VOXEL_LOGIN_CORS_ORIGINS` (comma list), `VOXEL_LOGIN_REQUIRE_VERIFIED` (`1` = on),
    /// `VOXEL_MAILER` (`log` | `ses`) and `VOXEL_MAIL_FROM` (required with `ses`).
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
        let require_verified = get("VOXEL_LOGIN_REQUIRE_VERIFIED", "0") == "1";
        let mailer = match get("VOXEL_MAILER", "log").as_str() {
            "log" => MailerKind::Log,
            "ses" => {
                let from = get("VOXEL_MAIL_FROM", "");
                if from.is_empty() {
                    return Err("VOXEL_MAILER=ses needs VOXEL_MAIL_FROM (the sender address)".into());
                }
                MailerKind::Ses { from }
            }
            other => return Err(format!("VOXEL_MAILER={other:?}: want `log` or `ses`")),
        };
        Ok(LoginConfig {
            bind,
            db_url: get("VOXEL_LOGIN_DB", DEFAULT_DB),
            keys_dir: PathBuf::from(get("VOXEL_LOGIN_KEYS_DIR", DEFAULT_KEYS_DIR)),
            public_url: get("VOXEL_LOGIN_PUBLIC_URL", DEFAULT_PUBLIC_URL),
            cors_origins,
            require_verified,
            mailer,
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
            .field("public_url", &self.public_url)
            .field("cors_origins", &self.cors_origins)
            .field("require_verified", &self.require_verified)
            .field("mailer", &self.mailer)
            .finish()
    }
}

#[cfg(test)]
mod redact_tests {
    use super::*;

    #[test]
    fn redacts_password_only() {
        assert_eq!(redact_url("postgres://u:s3cret@h:5432/db?x=1"), "postgres://u:<redacted>@h:5432/db?x=1");
        assert_eq!(redact_url("postgres://u:p@w@h/db"), "postgres://u:<redacted>@h/db");
        assert_eq!(redact_url("postgres://h/db"), "postgres://h/db");
        assert_eq!(redact_url("postgres://u@h/db"), "postgres://u@h/db");
        assert_eq!(redact_url("sqlite:target/x.sqlite"), "sqlite:target/x.sqlite");
        assert!(!format!("{:?}", LoginConfig::from_lookup(|k| (k == "VOXEL_LOGIN_DB").then(|| "postgres://u:s3cret@h/d".into())).unwrap()).contains("s3cret"));
    }
}
