use cambium_protocol::{ErrorCode, RequestId};
use cambium_store::{
    Bootstrap, SqliteStore, Store, TokenHash, TokenSettings,
    operations::{Claim, Completion, OperationKey, OperationsStore},
};
use serde_json::json;
use sqlx::{Connection as _, SqliteConnection, sqlite::SqliteConnectOptions};

const NOW: i64 = 1_800_000_000_000;

#[tokio::test]
async fn reservations_survive_restart_and_stale_runs_cannot_overwrite_a_retry() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("metadata.sqlite");
    let store = SqliteStore::open(&path, TokenSettings::default())
        .await
        .unwrap();
    let principal = store
        .deployment_bootstrap(
            &Bootstrap {
                tenant_slug: "operations".into(),
                human_name: "Operator".into(),
                token_hash: TokenHash([1; 32]),
                expires_at_ms: NOW + 172_800_000,
            },
            NOW,
        )
        .await
        .unwrap();
    let key = OperationKey {
        key: "write-1".into(),
        fingerprint: [7; 32],
    };
    let first = match store.claim_operation(&principal, &key, NOW).await.unwrap() {
        Claim::Run {
            run_id,
            reservation: None,
        } => run_id,
        _ => panic!("expected first claim"),
    };
    assert_eq!(
        store
            .claim_operation(&principal, &key, NOW + 1)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::IdempotencyKeyInUse
    );
    let value = json!({"expected": "0000000000000000000000000000000000000001"});
    store
        .reserve_operation(&principal, &key, &first, &value, NOW + 2)
        .await
        .unwrap();
    let different = OperationKey {
        key: key.key.clone(),
        fingerprint: [8; 32],
    };
    assert_eq!(
        store
            .claim_operation(&principal, &different, NOW + 3)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::IdempotencyKeyReused
    );
    // Even an overdue run is still protected while the process may be writing.
    store
        .deployment_purge_expired_responses(NOW + 86_400_001)
        .await
        .unwrap();
    assert_eq!(
        store
            .claim_operation(&principal, &key, NOW + 86_400_001)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::IdempotencyKeyInUse
    );
    store.close().await;

    let store = SqliteStore::open(&path, TokenSettings::default())
        .await
        .unwrap();
    store.deployment_recover_operations(NOW + 4).await.unwrap();
    let second = match store
        .claim_operation(&principal, &key, NOW + 5)
        .await
        .unwrap()
    {
        Claim::Run {
            run_id,
            reservation: Some(saved),
        } => {
            assert_eq!(saved, value);
            run_id
        }
        _ => panic!("expected retained reservation"),
    };
    assert_ne!(first, second);
    let completion = Completion {
        status: 200,
        encrypted_response: "opaque-envelope".into(),
    };
    assert_eq!(
        store
            .finish_operation(&principal, &key, &first, Some(&completion), NOW + 6)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::IdempotencyKeyInUse
    );
    assert_eq!(
        store
            .reserve_operation(&principal, &key, &RequestId::new(), &json!({}), NOW + 6)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::IdempotencyKeyInUse
    );
    store
        .finish_operation(&principal, &key, &second, Some(&completion), NOW + 7)
        .await
        .unwrap();
    match store
        .claim_operation(&principal, &key, NOW + 8)
        .await
        .unwrap()
    {
        Claim::Replay {
            status,
            encrypted_response,
        } => {
            assert_eq!(status, 200);
            assert_eq!(encrypted_response, "opaque-envelope");
        }
        _ => panic!("expected replay"),
    }
    store
        .revoke_token(&principal, &principal.token_id, NOW + 9)
        .await
        .unwrap();
    assert_eq!(
        store
            .claim_operation(&principal, &key, NOW + 10)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Unauthenticated
    );
    store
        .deployment_purge_expired_responses(NOW + 86_400_001)
        .await
        .unwrap();
    let mut database = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&path))
        .await
        .unwrap();
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency")
        .fetch_one(&mut database)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
}

#[tokio::test]
async fn a_key_and_tombstoned_repository_remain_scoped_to_their_tenant() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(
        directory.path().join("metadata.sqlite"),
        TokenSettings::default(),
    )
    .await
    .unwrap();
    let mut users = Vec::new();
    for (slug, byte) in [("a", 1), ("b", 2)] {
        users.push(
            store
                .deployment_bootstrap(
                    &Bootstrap {
                        tenant_slug: slug.into(),
                        human_name: "Operator".into(),
                        token_hash: TokenHash([byte; 32]),
                        expires_at_ms: NOW + 86_400_000,
                    },
                    NOW,
                )
                .await
                .unwrap(),
        );
    }
    let key = OperationKey {
        key: "same".into(),
        fingerprint: [1; 32],
    };
    for user in &users {
        assert!(matches!(
            store.claim_operation(user, &key, NOW).await.unwrap(),
            Claim::Run { .. }
        ));
    }
    let repository = store
        .reserve_repository(&users[0], "project", "refs/heads/main", NOW)
        .await
        .unwrap();
    store
        .finish_repository_creation(&users[0].tenant_id, &repository.id, NOW + 1)
        .await
        .unwrap();
    store
        .begin_repository_deletion(&users[0], &repository.id, NOW + 2)
        .await
        .unwrap();
    store
        .finish_repository_deletion(&users[0].tenant_id, &repository.id, NOW + 3)
        .await
        .unwrap();
    assert_eq!(
        store
            .get_repository(&users[0].tenant_id, &repository.id)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::NotFound
    );
    assert_eq!(
        store
            .get_repository_including_deleted(&users[0].tenant_id, &repository.id)
            .await
            .unwrap()
            .state,
        "deleted"
    );
    assert_eq!(
        store
            .get_repository_including_deleted(&users[1].tenant_id, &repository.id)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::NotFound
    );
    assert!(
        store
            .generation_ids(&users[0].tenant_id, &repository.id)
            .await
            .unwrap()
            .is_empty()
    );
}
