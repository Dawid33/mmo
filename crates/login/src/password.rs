//! Argon2id password hashing, the password policy and email normalization.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use login_api::{EMAIL_MAX_CHARS, PASSWORD_MAX_CHARS, PASSWORD_MIN_CHARS};

/// Argon2 memory cost of [`Hasher::for_tests`], in KiB.
pub const TEST_MEMORY_KIB: u32 = 8;

/// Argon2id with fixed cost parameters; the PHC string it writes carries them, so
/// verification works for hashes made with other parameters.
pub struct Hasher {
    params: Params,
}

impl Default for Hasher {
    /// The `argon2` crate defaults (m = 19 MiB, t = 2, p = 1).
    fn default() -> Hasher {
        Hasher { params: Params::default() }
    }
}

impl Hasher {
    /// Cheap parameters (m = 8 KiB, t = 1, p = 1) for tests.
    pub fn for_tests() -> Hasher {
        Hasher { params: Params::new(TEST_MEMORY_KIB, 1, 1, None).expect("valid test params") }
    }

    fn argon(&self) -> Argon2<'_> {
        Argon2::new(Algorithm::Argon2id, Version::V0x13, self.params.clone())
    }

    /// The PHC string of `password` under a fresh random salt.
    pub fn hash(&self, password: &str) -> String {
        let salt = SaltString::generate(&mut OsRng);
        self.argon()
            .hash_password(password.as_bytes(), &salt)
            .expect("argon2 hashing with valid parameters")
            .to_string()
    }

    /// Whether `password` matches the PHC string `phc` (false for an unparsable one).
    pub fn verify(&self, password: &str, phc: &str) -> bool {
        match PasswordHash::new(phc) {
            Ok(parsed) => Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok(),
            Err(_) => false,
        }
    }
}

/// The password policy: `PASSWORD_MIN_CHARS ..= PASSWORD_MAX_CHARS` characters.
pub fn check_password(password: &str) -> Result<(), &'static str> {
    let n = password.chars().count();
    if n < PASSWORD_MIN_CHARS {
        Err("password too short")
    } else if n > PASSWORD_MAX_CHARS {
        Err("password too long")
    } else {
        Ok(())
    }
}

/// Trim and lowercase an email; it must be at most `EMAIL_MAX_CHARS` and have exactly
/// one `@` with a non-empty part on each side.
pub fn normalize_email(email: &str) -> Result<String, &'static str> {
    let e = email.trim().to_lowercase();
    if e.chars().count() > EMAIL_MAX_CHARS {
        return Err("email too long");
    }
    let mut parts = e.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(l), Some(d), None) if !l.is_empty() && !d.is_empty() => Ok(e),
        _ => Err("invalid email address"),
    }
}
