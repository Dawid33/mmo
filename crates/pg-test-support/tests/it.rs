use pg_test_support::{PgTestDb, SKIP_MSG};
use sqlx::{Connection, PgConnection};

#[tokio::test]
#[ignore = "slow: >100ms — run via just test-full"]
async fn create_and_drop() {
    let Some(db) = PgTestDb::create_async("mmo_support").await else {
        eprintln!("{SKIP_MSG}");
        return;
    };
    assert!(db.name().starts_with("mmo_support_"));
    let mut c = PgConnection::connect(&db.url).await.unwrap();
    let one: i32 = sqlx::query_scalar("SELECT 1").fetch_one(&mut c).await.unwrap();
    assert_eq!(one, 1);
    c.close().await.unwrap();

    let name = db.name().to_string();
    let admin = pg_test_support::admin_url().unwrap();
    drop(db); // inside a running runtime: must not panic
    let mut a = PgConnection::connect(&admin).await.unwrap();
    let row: Option<i32> = sqlx::query_scalar("SELECT 1 FROM pg_database WHERE datname = $1")
        .bind(&name)
        .fetch_optional(&mut a)
        .await
        .unwrap();
    assert!(row.is_none(), "database {name} survived the drop");
}
