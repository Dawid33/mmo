//! Both migration sets must describe the same tables, columns, logical types, nullability, keys.

use std::collections::BTreeSet;

use sqlx::Row;
use world_store::{Dials, Pool, StoreUrl, open_pool};

use crate::common;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Ty {
    Int,
    Text,
    Bytes,
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

/// table: columns as `name:I|T|B` with `?` = nullable, `*` = primary key.
const EXPECTED: &[(&str, &str)] = &[
    ("world", "id:I*, format_version:I, region_blocks:I, chunk_blocks:I, content_crc:I, world_tick:I, super_region_regions:I, created_utc:T"),
    ("forged_archive", "id:I*, bytes:B"),
    ("forged_meta", "id:I*, format_version:I, bytes:B"),
    ("forged_tiles", "tx:I*, tz:I*, bytes:B"),
    ("forged_world_map", "id:I*, bytes:B"),
    ("regions", "rx:I*, rz:I*, saved_tick:I, terrain_version:I, meta:B, ground:B"),
    ("chunks", "rx:I*, rz:I*, cx:I*, cy:I*, cz:I*, data:B"),
    ("entities", "birth_rx:I*, birth_rz:I*, serial:I*, rx:I, rz:I, saved_tick:I, kind:I, bundle:B"),
    ("players", "player_id:I*, token:T, record:B?, saved_tick:I?"),
    ("history", "id:I*, bytes:B"),
    ("in_flight", "seq:I*, kind:I, target_rx:I, target_rz:I, stamp:I, payload:B, host:I"),
    ("super_regions", "sx:I*, sz:I*, owner:I, epoch:I"),
    ("coordinator", "id:I*, epoch:I, state:B?, saved_utc:T?"),
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
    let (sqlite, _g) = open_pool(&StoreUrl::Memory, Dials::BUILT).await.unwrap();
    let lite = introspect(&sqlite).await;
    assert_eq!(lite, expected(), "SQLite schema differs from the checked-in list");

    let Some(admin) = common::pg_admin_url() else { return common::skip_msg() };
    let db = common::PgTestDb::create(&admin).await;
    {
        let (pg, _g) = open_pool(&StoreUrl::Postgres(db.url.clone()), Dials::BUILT).await.unwrap();
        assert_eq!(introspect(&pg).await, lite, "Postgres schema differs from SQLite");
        if let Pool::Pg(p) = &pg {
            p.close().await;
        }
    }
    db.drop_db().await;
}
