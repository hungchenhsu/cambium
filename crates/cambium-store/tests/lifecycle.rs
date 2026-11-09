//! Branch lifecycle and compaction bookkeeping at the store boundary, so every
//! store implementation runs the statements that order by insertion.

use cambium_protocol::{BranchId, ObjectId, Principal};
use cambium_store::{Bootstrap, TokenHash, TokenSettings, lifecycle::BranchCreation};

mod common;
use common::TestDatabase;

const NOW: i64 = 1_800_000_000_000;

#[tokio::test]
async fn expiry_name_reuse_and_compaction_follow_insertion_order() {
    let database = TestDatabase::new().await;
    let store = database.open(TokenSettings::default()).await;
    let principal: Principal = store
        .deployment_bootstrap(
            &Bootstrap {
                tenant_slug: "lifecycle".into(),
                human_name: "Operator".into(),
                token_hash: TokenHash([1; 32]),
                expires_at_ms: NOW + 86_400_000,
            },
            NOW,
        )
        .await
        .unwrap();
    let tenant = principal.tenant_id.clone();
    let repository = store
        .reserve_repository(&principal, "project", "main", NOW)
        .await
        .unwrap();
    store
        .finish_repository_creation(&tenant, &repository.id, NOW)
        .await
        .unwrap();
    let start: ObjectId = "a".repeat(40).parse().unwrap();
    let branch = |expires_at_ms| BranchCreation {
        id: BranchId::new(),
        repository_id: repository.id.clone(),
        name: "scratch".into(),
        start_commit: start.clone(),
        expires_at_ms,
    };

    let first = store
        .reserve_branch(&principal, &branch(NOW + 1_000), NOW)
        .await
        .unwrap();
    assert_eq!(
        store
            .blocked_branch_names(&tenant, &repository.id, NOW)
            .await
            .unwrap(),
        ["scratch"],
        "a branch being created blocks its name"
    );
    store
        .finish_branch_creation(&tenant, &first.id, true, NOW)
        .await
        .unwrap();
    assert!(
        store
            .blocked_branch_names(&tenant, &repository.id, NOW)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .list_branches(&tenant, &repository.id, None, 10)
            .await
            .unwrap()
            .len(),
        1
    );

    // Past its deadline, the branch is pending and blocked.
    let pending = store
        .pending_branches(&tenant, &repository.id, NOW + 1_000)
        .await
        .unwrap();
    assert_eq!(
        pending.iter().map(|b| &b.id).collect::<Vec<_>>(),
        [&first.id]
    );
    store
        .begin_branch_expiry(&tenant, &first.id, NOW + 1_001)
        .await
        .unwrap();
    store
        .finish_branch_expiry(&tenant, &first.id, NOW + 1_002)
        .await
        .unwrap();
    assert_eq!(
        store
            .expired_branch_names(&tenant, &repository.id)
            .await
            .unwrap(),
        ["scratch"]
    );
    assert_eq!(
        store
            .blocked_branch_names(&tenant, &repository.id, NOW + 1_003)
            .await
            .unwrap(),
        ["scratch"],
        "an unreclaimed expiry blocks its name"
    );

    // A newer live branch with the same name unblocks it.
    let second = store
        .reserve_branch(&principal, &branch(NOW + 5_000), NOW + 1_004)
        .await
        .unwrap();
    store
        .finish_branch_creation(&tenant, &second.id, true, NOW + 1_004)
        .await
        .unwrap();
    assert!(
        store
            .blocked_branch_names(&tenant, &repository.id, NOW + 1_005)
            .await
            .unwrap()
            .is_empty(),
        "the newer live branch owns the name"
    );
    assert_eq!(
        store
            .pending_branches(&tenant, &repository.id, NOW + 5_000)
            .await
            .unwrap()
            .iter()
            .map(|b| &b.id)
            .collect::<Vec<_>>(),
        [&second.id]
    );

    // Compactions: the latest is the most recently started one.
    assert!(
        store
            .latest_compaction(&tenant, &repository.id)
            .await
            .unwrap()
            .is_none()
    );
    let compaction = store
        .start_compaction(&tenant, &repository.id, Some(&principal), None, NOW + 1_006)
        .await
        .unwrap();
    assert_eq!(
        store
            .latest_compaction(&tenant, &repository.id)
            .await
            .unwrap()
            .map(|c| c.id),
        Some(compaction.id.clone())
    );
    let fenced = store
        .fence_compaction(&compaction, NOW + 1_007)
        .await
        .unwrap();
    assert_eq!(fenced.stage, "catching_up");
    assert_eq!(
        fenced.state["excluded_names"],
        serde_json::json!([]),
        "the reused name belongs to the live branch"
    );
    assert_eq!(fenced.state["expiry_ids"], serde_json::json!([first.id]));
}
