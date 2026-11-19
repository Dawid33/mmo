//! The login service: its database ([`LoginStore`]) and the HTTP API
//! ([`router`] over an [`AppState`]; [`serve`] runs it from a [`LoginConfig`]).

pub mod api;
pub mod config;
pub mod ctl;
pub mod host_link;
pub mod metrics;
pub mod oidc;
pub mod store;

use std::io;
use std::sync::Arc;

pub use api::{AppState, router, system_clock};
pub use config::{LoginConfig, OidcConfig};
pub use store::{
    AccountId, AccountRow, DISPLAY_NAME_MAX_CHARS, LoginError, LoginStore, OidcUpsert, OpenSession, PendingKick, RefreshOutcome,
    WorldRow, normalize_email,
};

/// Connections per pool (Postgres / file SQLite).
pub const LOGIN_POOL_MAX: u32 = 8;

/// Run the login service until the process ends: open (and migrate) the database, load the
/// signing keys, bind `config.bind` and serve [`router`].
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
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    log::info!(
        "[login] listening on http://{} (signing kid {}, {} key(s) published)",
        listener.local_addr()?,
        signer.signing_key().kid(),
        signer.jwks().keys.len()
    );
    // The service token the worlds' coordinators are called with (worlds with a `host_link`).
    let service_token = match svc_link::service_token_from_env() {
        Ok(token) => Some(token),
        Err(why) => {
            log::info!("[login] no service token ({why}): worlds with a host link cannot be joined");
            None
        }
    };
    // A provider that cannot be reached at start is fatal: better than a login that never works.
    let verifier = match &config.oidc {
        Some(oidc) => {
            let verifier = oidc::OidcVerifier::discover(&oidc.issuer, &oidc.audience)
                .await
                .map_err(|e| io::Error::other(format!("identity provider {}: {e}", oidc.issuer)))?;
            log::info!("[login] identity-provider login on: issuer {}, audience {}", oidc.issuer, oidc.audience);
            Some(verifier)
        }
        None => None,
    };
    let mut state = AppState::new(
        store,
        Arc::new(signer),
        Arc::new(config),
        system_clock(),
    )
    .with_service_token(service_token);
    if let Some(verifier) = verifier {
        state = state.with_oidc(Arc::new(verifier));
    }
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
}

/// Resolves on ctrl-c or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    log::info!("[login] shutting down");
}
