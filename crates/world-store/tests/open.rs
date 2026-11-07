use sqlx::Row;
use world_store::{Dials, Pool, StoreError, StoreUrl, open_pool};

use crate::common;

fn sqlite_of(p: &Pool) -> &sqlx::SqlitePool {
    match p {
        Pool::Sqlite(p) => p,
        Pool::Pg(_) => panic!("expected sqlite"),
    }
}

fn temp_url(tag: &str) -> (std::path::PathBuf, StoreUrl) {
    let dir = std::env::temp_dir().join(format!("mmo_ws_{tag}_{}_{}", std::process::id(), std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    let url = StoreUrl::Sqlite(dir.join("world.sqlite"));
    (dir, url)
}

#[test]
fn parse_urls() {
    assert_eq!(StoreUrl::parse("sqlite::memory:").unwrap(), StoreUrl::Memory);
    assert!(matches!(StoreUrl::parse("postgres://a/b").unwrap(), StoreUrl::Postgres(_)));
    assert!(matches!(StoreUrl::parse("postgresql://a/b").unwrap(), StoreUrl::Postgres(_)));
    assert_eq!(StoreUrl::parse("worlds/x.sqlite").unwrap(), StoreUrl::Sqlite("worlds/x.sqlite".into()));
}

#[tokio::test]
async fn fresh_open_writes_world_row() {
    let (pool, _g) = open_pool(&StoreUrl::Memory, Dials::BUILT).await.unwrap();
    let row = sqlx::query("SELECT format_version, region_blocks, chunk_blocks FROM world WHERE id = 1")
        .fetch_one(sqlite_of(&pool))
        .await
        .unwrap();
    assert_eq!(row.get::<i64, _>(0), 1);
    assert_eq!(row.get::<i64, _>(1), Dials::BUILT.region_blocks as i64);
    assert_eq!(row.get::<i64, _>(2), Dials::BUILT.chunk_blocks as i64);
}

#[tokio::test]
async fn fresh_open_on_postgres() {
    let Some(admin) = common::pg_admin_url() else { return common::skip_msg() };
    let db = common::PgTestDb::create(&admin).await;
    {
        let (pool, _g) = open_pool(&StoreUrl::Postgres(db.url.clone()), Dials::BUILT).await.unwrap();
        let Pool::Pg(p) = &pool else { panic!() };
        let v: i64 = sqlx::query("SELECT format_version FROM world WHERE id = 1").fetch_one(p).await.unwrap().get(0);
        assert_eq!(v, 1);
        // A second process on the same database is refused.
        let second = open_pool(&StoreUrl::Postgres(db.url.clone()), Dials::BUILT).await;
        assert!(matches!(second, Err(StoreError::Locked(_))));
        p.close().await;
    }
    db.drop_db().await;
}

#[tokio::test]
async fn other_dials_refused() {
    let (dir, url) = temp_url("dials");
    let seeded = Dials { region_blocks: 512, chunk_blocks: Dials::BUILT.chunk_blocks };
    drop(open_pool(&url, seeded).await.unwrap());
    let err = open_pool(&url, Dials::BUILT).await.err().unwrap();
    match &err {
        StoreError::OtherDials { stored, .. } => assert_eq!(stored.region_blocks, 512),
        e => panic!("{e:?}"),
    }
    assert!(err.to_string().contains("region_blocks=512"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn second_open_is_locked() {
    let (dir, url) = temp_url("lock");
    let (pool, _guard) = open_pool(&url, Dials::BUILT).await.unwrap();
    let second = open_pool(&url, Dials::BUILT).await;
    assert!(matches!(second, Err(StoreError::Locked(_))), "{:?}", second.err());
    // The first stays usable.
    let n: i64 = sqlx::query("SELECT COUNT(*) FROM world").fetch_one(sqlite_of(&pool)).await.unwrap().get(0);
    assert_eq!(n, 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn newer_format_refused() {
    let (dir, url) = temp_url("format");
    {
        let (pool, _g) = open_pool(&url, Dials::BUILT).await.unwrap();
        sqlx::query("UPDATE world SET format_version = 2").execute(sqlite_of(&pool)).await.unwrap();
        sqlite_of(&pool).close().await;
    }
    let err = open_pool(&url, Dials::BUILT).await.err().unwrap();
    assert_eq!(err, StoreError::OtherFormat { found: 2 });
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn sqlite_dollar_placeholders_bind_in_order() {
    let (pool, _g) = open_pool(&StoreUrl::Memory, Dials::BUILT).await.unwrap();
    let p = sqlite_of(&pool);
    sqlx::query("INSERT INTO history (id, bytes) VALUES ($1, $2)")
        .bind(7_i64)
        .bind(vec![1_u8, 2, 3])
        .execute(p)
        .await
        .unwrap();
    let row = sqlx::query("SELECT id, bytes FROM history WHERE id = $1").bind(7_i64).fetch_one(p).await.unwrap();
    assert_eq!(row.get::<i64, _>(0), 7);
    assert_eq!(row.get::<Vec<u8>, _>(1), vec![1, 2, 3]);
}
