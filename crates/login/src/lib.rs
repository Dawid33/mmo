//! The login service: its database ([`LoginStore`]), password handling, mail and the HTTP API
//! ([`router`] over an [`AppState`]; [`serve`] runs it from a [`LoginConfig`]).

pub mod api;
pub mod config;
pub mod mail;
pub mod pages;
pub mod password;
pub mod store;

use std::io;
use std::sync::Arc;

pub use api::{AppState, router, system_clock};
pub use config::{LoginConfig, MailerKind};
pub use password::{Hasher, check_password, normalize_email};
pub use store::{AccountId, AccountRow, LoginError, LoginStore, RefreshOutcome, WorldRow};

/// Kind of an email token: verify the address.
pub const EMAIL_TOKEN_VERIFY: i64 = 0;
/// Kind of an email token: reset the password.
pub const EMAIL_TOKEN_RESET: i64 = 1;
/// Connections per pool (Postgres / file SQLite).
pub const LOGIN_POOL_MAX: u32 = 8;

/// Run the login service until the process ends: open (and migrate) the database, load the
/// signing keys, pick the mailer, bind `config.bind` and serve [`router`].
pub async fn serve(config: LoginConfig) -> io::Result<()> {
    let store = LoginStore::open(&config.db_url)
        .await
        .map_err(|e| io::Error::other(format!("login database: {e}")))?;
    let signer = auth::Signer::load_dir(&config.keys_dir).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("signing keys in {}: {e} (run `login keygen` to make one)", config.keys_dir.display()),
        )
    })?;
    let mailer: Arc<dyn mail::Mailer> = match &config.mailer {
        MailerKind::Log => {
            log::warn!("[login] VOXEL_MAILER=log: mail links (with their tokens) go to the log — dev only");
            Arc::new(mail::LogMailer::default())
        }
        MailerKind::Ses { from } => Arc::new(mail::SesMailer::from_env(from.clone()).await),
    };
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    log::info!(
        "[login] listening on http://{} (signing kid {}, {} key(s) published, require_verified={})",
        listener.local_addr()?,
        signer.signing_key().kid(),
        signer.jwks().keys.len(),
        config.require_verified
    );
    let state = AppState::new(
        store,
        Arc::new(signer),
        Hasher::default(),
        mailer,
        Arc::new(config),
        system_clock(),
    );
    axum::serve(listener, router(state)).await
}
