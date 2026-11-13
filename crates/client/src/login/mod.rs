//! The client side of the login service (`crates/login`, R2a): the HTTP client
//! ([`api::LoginApi`]), the identity provider's device sign-in ([`oidc`]), where the refresh token is kept ([`store`]) and where
//! the service lives ([`api::login_url`]). The client depends only on the
//! serde DTOs (`login-api`), never on the service itself.

pub mod api;
pub mod flow;
pub mod oidc;
pub mod store;

pub use api::{login_url, LoginApi, LoginFailure};
