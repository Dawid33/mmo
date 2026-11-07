//! Both migration sets must describe the same tables, columns, logical types, nullability, keys.

use std::collections::BTreeSet;

use sqlx::Row;
use login::LoginStore;

use crate::common;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Ty {
    Int,
    Text,
    Bytes,
}
enum Pool {
    Sqlite(sqlx::SqlitePool),
    Pg(sqlx::PgPool),
}
type Col = (String, String, Ty, bool, bool); // table, column, type, nullable, pk

fn ty(raw: &str) -> Ty {
    match raw.to_ascii_uppercase().as_str() {
        "BIGINT" | "INTEGER" => Ty::Int,
        "TEXT" => Ty::Text,
        "BYTEA" | "BLOB" => Ty::Bytes,
        other => panic!("type outside the shared subset: {other}"),
    }
}

async fn introspect(pool: &Pool) -> BTreeSet<Col> {
    let mut out = BTreeSet::new();
    match pool {
        Pool::Sqlite(p) => {
            let tables = sqlx::query(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE '\\_sqlx%' ESCAPE '\\' AND name NOT LIKE 'sqlite_%'",
            )
            .fetch_all(p)
            .await
            .unwrap();
            for t in tables {
                let t: String = t.get(0);
                let cols = sqlx::query(&format!("SELECT name, type, \"notnull\", pk FROM pragma_table_info('{t}')"))
                    .fetch_all(p)
                    .await
                    .unwrap();
                for c in cols {
                    out.insert((t.clone(), c.get(0), ty(&c.get::<String, _>(1)), c.get::<i64, _>(2) == 0, c.get::<i64, _>(3) > 0));
                }
            }
        }
        Pool::Pg(p) => {
            let cols = sqlx::query(
                "SELECT c.table_name::text, c.column_name::text, c.data_type::text, c.is_nullable::text, \
                 EXISTS (SELECT 1 FROM information_schema.key_column_usage k \
                    JOIN information_schema.table_constraints tc ON tc.constraint_name = k.constraint_name \
                      AND tc.table_schema = k.table_schema \
                    WHERE tc.constraint_type = 'PRIMARY KEY' AND k.table_schema = c.table_schema \
                      AND k.table_name = c.table_name AND k.column_name = c.column_name) \
                 FROM information_schema.columns c \
                 WHERE c.table_schema = 'public' AND c.table_name NOT LIKE '\\_sqlx%'",
            )
            .fetch_all(p)
            .await
            .unwrap();
            for c in cols {
                let data: String = c.get(2);
                let t = match data.as_str() {
                    "bigint" => Ty::Int,
                    "text" => Ty::Text,
                    "bytea" => Ty::Bytes,
                    o => panic!("pg type outside the shared subset: {o}"),
                };
                out.insert((c.get(0), c.get(1), t, c.get::<String, _>(3) == "YES", c.get(4)));
            }
        }
    }
    out
}

type Idx = (String, Vec<String>, bool); // table, columns in order, unique

/// Every non-primary-key index / UNIQUE constraint, per engine.
async fn indexes(pool: &Pool) -> BTreeSet<Idx> {
    let mut out = BTreeSet::new();
    match pool {
        Pool::Sqlite(p) => {
            let tables = sqlx::query(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE '\\_sqlx%' ESCAPE '\\' AND name NOT LIKE 'sqlite_%'",
            )
            .fetch_all(p)
            .await
            .unwrap();
            for t in tables {
                let t: String = t.get(0);
                let idx = sqlx::query(&format!("SELECT name, \"unique\" FROM pragma_index_list('{t}') WHERE origin != 'pk'"))
                    .fetch_all(p)
                    .await
                    .unwrap();
                for i in idx {
                    let name: String = i.get(0);
                    let cols = sqlx::query(&format!("SELECT name FROM pragma_index_info('{name}') ORDER BY seqno"))
                        .fetch_all(p)
                        .await
                        .unwrap();
                    out.insert((t.clone(), cols.iter().map(|c| c.get::<String, _>(0)).collect(), i.get::<i64, _>(1) != 0));
                }
            }
        }
        Pool::Pg(p) => {
            let rows = sqlx::query(
                "SELECT t.relname::text, i.indisunique, array_agg(a.attname::text ORDER BY k.ord) \
                 FROM pg_index i JOIN pg_class t ON t.oid = i.indrelid \
                 JOIN pg_namespace n ON n.oid = t.relnamespace \
                 CROSS JOIN LATERAL unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord) \
                 JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
                 WHERE n.nspname = 'public' AND NOT i.indisprimary AND t.relname NOT LIKE '\\_sqlx%' \
                 GROUP BY i.indexrelid, t.relname, i.indisunique",
            )
            .fetch_all(p)
            .await
            .unwrap();
            for r in rows {
                out.insert((r.get(0), r.get::<Vec<String>, _>(2), r.get(1)));
            }
        }
    }
    out
}

fn expected_indexes() -> BTreeSet<Idx> {
    let i = |t: &str, c: &[&str], u| (t.to_string(), c.iter().map(|s| s.to_string()).collect(), u);
    BTreeSet::from([
        i("accounts", &["email"], true),
        i("refresh_tokens", &["account_id"], false),
        i("refresh_tokens", &["family"], false),
        i("world_players", &["player_token"], true),
    ])
}

/// table: columns as `name:I|T|B` with `?` = nullable, `*` = primary key.
const EXPECTED: &[(&str, &str)] = &[
    ("accounts", "account_id:I*, email:T, password_hash:T, email_verified_at:I?, created_at:I"),
    ("refresh_tokens", "token_hash:B*, account_id:I, family:B, expires_at:I, revoked_at:I?"),
    ("email_tokens", "token_hash:B*, account_id:I, kind:I, expires_at:I, used_at:I?"),
    ("worlds", "world_id:T*, name:T, status:I, quic:T?, webtransport:T?, cert_hash:T?, created_at:I"),
    ("world_players", "account_id:I*, world_id:T*, player_token:T, created_at:I"),
    ("sessions", "account_id:I*, world_id:T*, session_epoch:I, started_at:I"),
];

fn expected() -> BTreeSet<Col> {
    let mut out = BTreeSet::new();
    for (t, cols) in EXPECTED {
        for c in cols.split(", ") {
            let (name, spec) = c.split_once(':').unwrap();
            let t_ = match &spec[..1] {
                "I" => Ty::Int,
                "T" => Ty::Text,
                _ => Ty::Bytes,
            };
            out.insert((t.to_string(), name.to_string(), t_, spec.contains('?'), spec.contains('*')));
        }
    }
    out
}

#[tokio::test]
async fn schemas_match() {
    let lite = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("migrations/sqlite").run(&lite).await.unwrap();
    let lite_cols = introspect(&Pool::Sqlite(lite.clone())).await;
    let lite_pool = lite;
    assert_eq!(lite_cols, expected(), "SQLite schema differs from the checked-in list");

    let lite_idx = indexes(&Pool::Sqlite(lite_pool)).await;
    assert_eq!(lite_idx, expected_indexes(), "SQLite indexes differ from the checked-in list");

    let Some(admin) = common::pg_admin_url() else { return common::skip_msg() };
    let db = common::PgTestDb::create(&admin).await;
    {
        let _store = LoginStore::open(&db.url).await.unwrap(); // migrates
        let pg = sqlx::PgPool::connect(&db.url).await.unwrap();
        assert_eq!(introspect(&Pool::Pg(pg.clone())).await, lite_cols, "Postgres schema differs from SQLite");
        assert_eq!(indexes(&Pool::Pg(pg.clone())).await, lite_idx, "Postgres indexes differ from SQLite");
        pg.close().await;
    }
    db.drop_db().await;
}
