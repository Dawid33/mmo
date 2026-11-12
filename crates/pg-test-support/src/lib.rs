//! Throwaway Postgres databases for tests. Dev-dependency only.
//!
//! The admin URL comes from `MMO_TEST_DATABASE_URL`; each [`PgTestDb`] is a fresh
//! `<prefix>_<pid>_<nanos>` database, dropped (`WITH (FORCE)`) when the value drops.

use sqlx::{Connection, PgConnection};

pub const ADMIN_URL_ENV: &str = "MMO_TEST_DATABASE_URL";
pub const SKIP_MSG: &str = "skipped: MMO_TEST_DATABASE_URL unset";

/// The admin URL (a database to connect to for CREATE/DROP DATABASE), if pg tests are on.
pub fn admin_url() -> Option<String> {
    std::env::var(ADMIN_URL_ENV).ok().filter(|s| !s.is_empty())
}

pub struct PgTestDb {
    admin_url: String,
    name: String,
    /// URL of the throwaway database.
    pub url: String,
}

impl PgTestDb {
    /// Blocking create (own thread + runtime, so it is safe anywhere). `None` when the env var is unset.
    pub fn create(prefix: &str) -> Option<PgTestDb> {
        let prefix = prefix.to_string();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(Self::create_async(&prefix))
        })
        .join()
        .expect("create thread")
    }

    /// Async create. `None` when the env var is unset.
    pub async fn create_async(prefix: &str) -> Option<PgTestDb> {
        let admin_url = admin_url()?;
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let name = format!("{prefix}_{:x}_{nanos}", std::process::id());
        let mut c = PgConnection::connect(&admin_url).await.expect("connect to MMO_TEST_DATABASE_URL");
        sqlx::query(&format!("CREATE DATABASE {name}")).execute(&mut c).await.expect("create database");
        let (base, _) = admin_url.rsplit_once('/').expect("a url with a database name");
        Some(PgTestDb { url: format!("{base}/{name}"), admin_url, name })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for PgTestDb {
    fn drop(&mut self) {
        let admin = self.admin_url.clone();
        let name = self.name.clone();
        // A fresh thread: dropping inside an async test must not start a runtime on its thread.
        let done = std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => return eprintln!("pg-test-support: runtime for dropping {name}: {e}"),
            };
            rt.block_on(async {
                let mut c = match PgConnection::connect(&admin).await {
                    Ok(c) => c,
                    Err(e) => return eprintln!("pg-test-support: connect to drop {name}: {e}"),
                };
                if let Err(e) =
                    sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)")).execute(&mut c).await
                {
                    eprintln!("pg-test-support: dropping {name}: {e}");
                }
            });
        })
        .join();
        if done.is_err() {
            eprintln!("pg-test-support: drop thread panicked for {}", self.name);
        }
    }
}
