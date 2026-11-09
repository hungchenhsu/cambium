//! One store test suite for every implementation (ADR-0010, ADR-0020).
//!
//! Tests run against SQLite. When `CAMBIUM_TEST_POSTGRES_URL` names a Postgres
//! server that the tests may create databases on, they run against a fresh
//! Postgres database instead.

#![allow(dead_code)]

use std::sync::Arc;

use cambium_store::{PostgresStore, SqliteStore, TokenSettings, compaction::CompactionStore};
use sqlx::Connection as _;

pub struct TestDatabase {
    kind: Kind,
}

enum Kind {
    Sqlite(tempfile::TempDir),
    Postgres(String),
}

impl TestDatabase {
    pub async fn new() -> Self {
        let kind = match std::env::var("CAMBIUM_TEST_POSTGRES_URL")
            .ok()
            .filter(|url| !url.is_empty())
        {
            Some(server) => {
                let name = format!(
                    "cambium_test_{}",
                    ulid::Ulid::new().to_string().to_ascii_lowercase()
                );
                let mut admin = sqlx::PgConnection::connect(&server).await.unwrap();
                sqlx::query(&format!("CREATE DATABASE {name}"))
                    .execute(&mut admin)
                    .await
                    .unwrap();
                let (base, _) = server.rsplit_once('/').expect("a URL with a database name");
                Kind::Postgres(format!("{base}/{name}"))
            }
            None => {
                assert!(
                    std::env::var_os("CAMBIUM_TEST_REQUIRE_POSTGRES").is_none(),
                    "CAMBIUM_TEST_REQUIRE_POSTGRES is set but CAMBIUM_TEST_POSTGRES_URL is not"
                );
                Kind::Sqlite(tempfile::tempdir().unwrap())
            }
        };
        Self { kind }
    }

    pub fn is_postgres(&self) -> bool {
        matches!(self.kind, Kind::Postgres(_))
    }

    pub async fn open(&self, settings: TokenSettings) -> Arc<dyn CompactionStore> {
        self.try_open(settings).await.unwrap()
    }

    pub async fn try_open(
        &self,
        settings: TokenSettings,
    ) -> cambium_store::StoreResult<Arc<dyn CompactionStore>> {
        Ok(match &self.kind {
            Kind::Sqlite(directory) => Arc::new(
                SqliteStore::open(directory.path().join("metadata.sqlite"), settings).await?,
            ),
            Kind::Postgres(url) => Arc::new(PostgresStore::open(url, settings).await?),
        })
    }

    /// Counts rows directly, bypassing the store.
    pub async fn count(&self, table: &str) -> i64 {
        let query = format!("SELECT count(*) FROM {table}");
        match &self.kind {
            Kind::Sqlite(directory) => {
                let mut connection = sqlx::SqliteConnection::connect_with(
                    &sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(directory.path().join("metadata.sqlite")),
                )
                .await
                .unwrap();
                sqlx::query_scalar(&query)
                    .fetch_one(&mut connection)
                    .await
                    .unwrap()
            }
            Kind::Postgres(url) => {
                let mut connection = sqlx::PgConnection::connect(url).await.unwrap();
                sqlx::query_scalar(&query)
                    .fetch_one(&mut connection)
                    .await
                    .unwrap()
            }
        }
    }
}
