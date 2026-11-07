//! Shared test helpers: a throwaway Postgres database per test.
#![allow(dead_code)]

use sqlx::{Connection, PgConnection};

/// The admin URL (a database to connect to for CREATE/DROP DATABASE), if pg tests are on.
pub fn pg_admin_url() -> Option<String> {
    std::env::var("MMO_TEST_DATABASE_URL").ok().filter(|s| !s.is_empty())
}

pub fn skip_msg() {
    eprintln!("skipped: MMO_TEST_DATABASE_URL unset");
}

/// A `mmo_test_<random>` database, created from `MMO_TEST_DATABASE_URL`. Call `drop_db().await`
/// at the end (after closing every pool/connection; it uses WITH (FORCE) anyway).
pub struct PgTestDb {
    admin_url: String,
    pub name: String,
    /// URL of the throwaway database.
    pub url: String,
}

impl PgTestDb {
    pub async fn create(admin_url: &str) -> PgTestDb {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("mmo_test_{:x}_{:x}", std::process::id(), nanos);
        let mut c = PgConnection::connect(admin_url).await.expect("connect to MMO_TEST_DATABASE_URL");
        sqlx::query(&format!("CREATE DATABASE {name}")).execute(&mut c).await.expect("create database");
        let (base, _) = admin_url.rsplit_once('/').expect("url with a database name");
        PgTestDb { admin_url: admin_url.to_string(), url: format!("{base}/{name}"), name }
    }

    pub async fn drop_db(self) {
        let mut c = PgConnection::connect(&self.admin_url).await.expect("connect");
        let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name))
            .execute(&mut c)
            .await;
    }
}
