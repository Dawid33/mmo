//! The client side of the login service (`crates/login`, R2a): the HTTP client
//! ([`api::LoginApi`]), where the refresh token is kept ([`store`]) and where
//! the service lives ([`api::login_url`]). The client depends only on the
//! serde DTOs (`login-api`), never on the service itself.

pub mod api;
pub mod flow;
pub mod store;

pub use api::{login_url, LoginApi, LoginFailure};
