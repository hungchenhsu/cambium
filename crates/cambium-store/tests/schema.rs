use cambium_store::{SqliteStore, StoreError, TokenSettings};
use sqlx::{Connection, Row, SqliteConnection, sqlite::SqliteConnectOptions};

#[tokio::test]
async fn every_tenant_table_and_unique_index_has_a_tenant_prefix() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("metadata.sqlite");
    let store = SqliteStore::open(&path, TokenSettings::default())
        .await
        .unwrap();
    let mut db = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
        .await
        .unwrap();
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(&mut db)
    .await
    .unwrap();
    let deployment_tables = ["tenants", "_sqlx_migrations"];
    for table in tables {
        if deployment_tables.contains(&table.as_str()) {
            continue;
        }
        let columns = sqlx::query("SELECT name, \"notnull\" FROM pragma_table_info(?)")
            .bind(&table)
            .fetch_all(&mut db)
            .await
            .unwrap();
        assert!(
            columns
                .iter()
                .any(|c| c.get::<String, _>("name") == "tenant_id" && c.get::<bool, _>("notnull")),
            "{table}"
        );
        let indexes: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_index_list(?) WHERE \"unique\" = 1")
                .bind(&table)
                .fetch_all(&mut db)
                .await
                .unwrap();
        for index in indexes {
            let first: String =
                sqlx::query_scalar("SELECT name FROM pragma_index_info(?) WHERE seqno = 0")
                    .bind(&index)
                    .fetch_one(&mut db)
                    .await
                    .unwrap();
            assert_eq!(first, "tenant_id", "{table}.{index}");
        }
        for actor_column in ["created_by", "created_by_kind"] {
            assert!(
                columns
                    .iter()
                    .any(|c| c.get::<String, _>("name") == actor_column),
                "{table}.{actor_column}"
            );
        }
    }
    assert_eq!(
        sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
            .fetch_one(&mut db)
            .await
            .unwrap(),
        "wal"
    );
    store.close().await;
}

#[tokio::test]
async fn a_binary_refuses_unknown_or_modified_migrations() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("metadata.sqlite");
    let store = SqliteStore::open(&path, TokenSettings::default())
        .await
        .unwrap();
    store.close().await;
    let mut db = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
        .await
        .unwrap();
    sqlx::query("INSERT INTO _sqlx_migrations (version, description, installed_on, success, checksum, execution_time) VALUES (9999, 'future', CURRENT_TIMESTAMP, 1, X'00', 1)").execute(&mut db).await.unwrap();
    assert!(matches!(
        SqliteStore::open(&path, TokenSettings::default()).await,
        Err(StoreError::Migration(_))
    ));
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 9999")
        .execute(&mut db)
        .await
        .unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET checksum = X'00' WHERE version = 1")
        .execute(&mut db)
        .await
        .unwrap();
    assert!(matches!(
        SqliteStore::open(&path, TokenSettings::default()).await,
        Err(StoreError::Migration(_))
    ));
}

/// The Postgres schema follows the same tenant rules and migration refusal.
/// Runs only when CAMBIUM_TEST_POSTGRES_URL names a test server.
#[tokio::test]
async fn postgres_schema_has_tenant_prefixes_and_refuses_unknown_migrations() {
    let Some(server) = std::env::var("CAMBIUM_TEST_POSTGRES_URL")
        .ok()
        .filter(|url| !url.is_empty())
    else {
        eprintln!("skipped: CAMBIUM_TEST_POSTGRES_URL is not set");
        return;
    };
    let name = format!(
        "cambium_schema_{}",
        ulid::Ulid::new().to_string().to_ascii_lowercase()
    );
    let mut admin = sqlx::PgConnection::connect(&server).await.unwrap();
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&mut admin)
        .await
        .unwrap();
    let url = format!("{}/{name}", server.rsplit_once('/').unwrap().0);
    let store = cambium_store::PostgresStore::open(&url, TokenSettings::default())
        .await
        .unwrap();
    // A second writer is refused at once while the first holds the advisory lock.
    let started = std::time::Instant::now();
    match cambium_store::PostgresStore::open(&url, TokenSettings::default()).await {
        Err(StoreError::Request(error)) => {
            assert_eq!(error.code, cambium_protocol::ErrorCode::InvalidRequest);
            assert_eq!(error.details["field"], "database_url");
        }
        Err(other) => panic!("unexpected error: {other:?}"),
        Ok(_) => panic!("a second writer opened the store"),
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let mut db = sqlx::PgConnection::connect(&url).await.unwrap();
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'public' AND table_type = 'BASE TABLE'",
    )
    .fetch_all(&mut db)
    .await
    .unwrap();
    assert_eq!(tables.len(), 12, "{tables:?}");
    for table in tables {
        if ["tenants", "_sqlx_migrations"].contains(&table.as_str()) {
            continue;
        }
        let nullable: String = sqlx::query_scalar("SELECT is_nullable::text FROM information_schema.columns WHERE table_schema = 'public' AND table_name = $1 AND column_name = 'tenant_id'")
            .bind(&table)
            .fetch_one(&mut db)
            .await
            .unwrap_or_else(|_| panic!("{table} has no tenant_id"));
        assert_eq!(nullable, "NO", "{table}");
        for actor_column in ["created_by", "created_by_kind"] {
            let present: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_schema = 'public' AND table_name = $1 AND column_name = $2)")
                .bind(&table)
                .bind(actor_column)
                .fetch_one(&mut db)
                .await
                .unwrap();
            assert!(present, "{table}.{actor_column}");
        }
        let firsts: Vec<(String, String)> = sqlx::query_as("SELECT i.relname::text, a.attname::text FROM pg_index x JOIN pg_class i ON i.oid = x.indexrelid JOIN pg_class t ON t.oid = x.indrelid JOIN pg_namespace n ON n.oid = t.relnamespace JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = x.indkey[0] WHERE n.nspname = 'public' AND t.relname = $1 AND x.indisunique")
            .bind(&table)
            .fetch_all(&mut db)
            .await
            .unwrap();
        assert!(!firsts.is_empty(), "{table}");
        for (index, first) in firsts {
            assert_eq!(first, "tenant_id", "{table}.{index}");
        }
    }
    store.close().await;
    sqlx::query("INSERT INTO _sqlx_migrations (version, description, installed_on, success, checksum, execution_time) VALUES (9999, 'future', now(), true, '\\x00', 1)")
        .execute(&mut db)
        .await
        .unwrap();
    assert!(matches!(
        cambium_store::PostgresStore::open(&url, TokenSettings::default()).await,
        Err(StoreError::Migration(_))
    ));
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 9999")
        .execute(&mut db)
        .await
        .unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET checksum = '\\x00' WHERE version = 1")
        .execute(&mut db)
        .await
        .unwrap();
    assert!(matches!(
        cambium_store::PostgresStore::open(&url, TokenSettings::default()).await,
        Err(StoreError::Migration(_))
    ));
}
