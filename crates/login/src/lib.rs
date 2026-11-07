//! The login service's core: its database ([`LoginStore`]) and password handling.

pub mod mail;
pub mod password;
pub mod store;

pub use password::{Hasher, check_password, normalize_email};
pub use store::{AccountId, AccountRow, LoginError, LoginStore, RefreshOutcome, WorldRow};

/// Kind of an email token: verify the address.
pub const EMAIL_TOKEN_VERIFY: i64 = 0;
/// Kind of an email token: reset the password.
pub const EMAIL_TOKEN_RESET: i64 = 1;
/// Connections per pool (Postgres / file SQLite).
pub const LOGIN_POOL_MAX: u32 = 8;
