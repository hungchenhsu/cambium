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
