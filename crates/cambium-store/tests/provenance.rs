use cambium_protocol::{ErrorCode, IdentityKind, ObjectId, Principal, Scope};
use cambium_store::{
    Bootstrap, CreateIdentity, MintToken, StoreError, TokenHash, TokenSettings,
    compaction::CompactionStore, operations::ProvenanceClaims,
};
use std::sync::Arc;

mod common;
use common::TestDatabase;

const NOW: i64 = 1_800_000_000_000;

async fn database() -> (TestDatabase, Arc<dyn CompactionStore>) {
    let database = TestDatabase::new().await;
    let store = database.open(TokenSettings::default()).await;
    (database, store)
}

async fn bootstrap(store: &dyn CompactionStore, slug: &str, hash: u8) -> Principal {
    store
        .deployment_bootstrap(
            &Bootstrap {
                tenant_slug: slug.into(),
                human_name: "Operator".into(),
                token_hash: TokenHash([hash; 32]),
                expires_at_ms: NOW + 86_400_000,
            },
            NOW,
        )
        .await
        .unwrap()
}

fn commit(digit: char) -> ObjectId {
    digit.to_string().repeat(40).parse().unwrap()
}

fn claims(model: &str) -> ProvenanceClaims {
    ProvenanceClaims {
        model: Some(model.into()),
        session_id: Some("session-1".into()),
    }
}

fn assert_code<T: std::fmt::Debug>(result: Result<T, StoreError>, expected: ErrorCode) {
    match result {
        Ok(value) => panic!("expected {expected:?}, got {value:?}"),
        Err(error) => assert_eq!(error.code(), expected, "{error:?}"),
    }
}

#[tokio::test]
async fn provenance_is_attested_tenant_scoped_first_wins_and_deleted_with_the_repository() {
    let (_directory, store) = database().await;
    let a = bootstrap(&*store, "a", 1).await;
    let b = bootstrap(&*store, "b", 2).await;
    let repository = store
        .reserve_repository(&a, "project", "main", NOW)
        .await
        .unwrap();
    store
        .finish_repository_creation(&a.tenant_id, &repository.id, NOW)
        .await
        .unwrap();

    // Nothing is recorded until a write records it.
    assert_code(
        store
            .get_commit_provenance(&a.tenant_id, &repository.id, &commit('a'))
            .await,
        ErrorCode::NotFound,
    );
    assert!(
        store
            .record_commit_provenance(&a, &repository.id, &commit('a'), &claims("first"), NOW + 1)
            .await
            .unwrap()
    );
    // The first record for a commit ID wins, and a later write learns that it did not record.
    assert!(
        !store
            .record_commit_provenance(&a, &repository.id, &commit('a'), &claims("second"), NOW + 2)
            .await
            .unwrap()
    );
    let record = store
        .get_commit_provenance(&a.tenant_id, &repository.id, &commit('a'))
        .await
        .unwrap();
    assert_eq!(record.commit_id, commit('a'));
    assert_eq!(record.claims, claims("first"));
    assert_eq!(record.recorded_at_ms, NOW + 1);
    assert_eq!(record.recorded_by, a.actor);
    assert_eq!(record.recorded_by.kind, IdentityKind::Human);

    // Absent claims stay absent.
    store
        .record_commit_provenance(
            &a,
            &repository.id,
            &commit('b'),
            &ProvenanceClaims::default(),
            NOW + 3,
        )
        .await
        .map(|recorded| assert!(recorded))
        .unwrap();
    let bare = store
        .get_commit_provenance(&a.tenant_id, &repository.id, &commit('b'))
        .await
        .unwrap();
    assert_eq!(bare.claims, ProvenanceClaims::default());

    // Another tenant can neither read nor write it, even with the same IDs.
    assert_code(
        store
            .get_commit_provenance(&b.tenant_id, &repository.id, &commit('a'))
            .await,
        ErrorCode::NotFound,
    );
    assert_code(
        store
            .record_commit_provenance(&b, &repository.id, &commit('c'), &claims("x"), NOW + 4)
            .await,
        ErrorCode::NotFound,
    );

    // A read-only token cannot record, and a revoked token is rejected.
    let reader = store
        .mint_token(
            &a,
            &MintToken {
                identity_id: a.actor.identity_id.clone(),
                token_hash: TokenHash([3; 32]),
                scope: Scope::RepoRead,
                repositories: None,
                expires_at_ms: a.expires_at_ms,
            },
            NOW,
        )
        .await
        .unwrap();
    assert_code(
        store
            .record_commit_provenance(&reader, &repository.id, &commit('d'), &claims("x"), NOW + 5)
            .await,
        ErrorCode::PermissionDenied,
    );
    let writer = store
        .mint_token(
            &a,
            &MintToken {
                identity_id: a.actor.identity_id.clone(),
                token_hash: TokenHash([4; 32]),
                scope: Scope::RepoWrite,
                repositories: None,
                expires_at_ms: a.expires_at_ms,
            },
            NOW,
        )
        .await
        .unwrap();
    store
        .revoke_token(&a, &writer.token_id, NOW + 6)
        .await
        .unwrap();
    assert_code(
        store
            .record_commit_provenance(&writer, &repository.id, &commit('e'), &claims("x"), NOW + 7)
            .await,
        ErrorCode::Unauthenticated,
    );
    for digit in ['c', 'd', 'e'] {
        assert_code(
            store
                .get_commit_provenance(&a.tenant_id, &repository.id, &commit(digit))
                .await,
            ErrorCode::NotFound,
        );
    }

    // The attested kind is the agent's own, taken from its token.
    let agent = store
        .create_identity(
            &a,
            &CreateIdentity {
                kind: IdentityKind::Agent,
                name: "Writer agent".into(),
                owner_id: Some(a.actor.identity_id.clone()),
            },
            NOW,
        )
        .await
        .unwrap();
    let agent_token = store
        .mint_token(
            &a,
            &MintToken {
                identity_id: agent.id.clone(),
                token_hash: TokenHash([5; 32]),
                scope: Scope::RepoWrite,
                repositories: Some(vec![repository.id.clone()]),
                expires_at_ms: NOW + 3_600_000,
            },
            NOW,
        )
        .await
        .unwrap();
    assert!(
        store
            .record_commit_provenance(
                &agent_token,
                &repository.id,
                &commit('9'),
                &claims("agent"),
                NOW + 7
            )
            .await
            .unwrap()
    );
    let by_agent = store
        .get_commit_provenance(&a.tenant_id, &repository.id, &commit('9'))
        .await
        .unwrap();
    assert_eq!(by_agent.recorded_by.identity_id, agent.id);
    assert_eq!(by_agent.recorded_by.kind, IdentityKind::Agent);

    // A token restricted to another repository cannot record here.
    let elsewhere = store
        .reserve_repository(&a, "elsewhere", "main", NOW)
        .await
        .unwrap();
    let restricted = store
        .mint_token(
            &a,
            &MintToken {
                identity_id: a.actor.identity_id.clone(),
                token_hash: TokenHash([6; 32]),
                scope: Scope::RepoWrite,
                repositories: Some(vec![elsewhere.id.clone()]),
                expires_at_ms: a.expires_at_ms,
            },
            NOW,
        )
        .await
        .unwrap();
    assert_code(
        store
            .record_commit_provenance(
                &restricted,
                &repository.id,
                &commit('8'),
                &claims("x"),
                NOW + 7,
            )
            .await,
        ErrorCode::NotFound,
    );
    assert_code(
        store
            .get_commit_provenance(&a.tenant_id, &repository.id, &commit('8'))
            .await,
        ErrorCode::NotFound,
    );

    // Repository deletion removes the rows.
    store
        .begin_repository_deletion(&a, &repository.id, NOW + 8)
        .await
        .unwrap();
    // A deleting repository accepts no new records.
    assert_code(
        store
            .record_commit_provenance(&a, &repository.id, &commit('f'), &claims("x"), NOW + 9)
            .await,
        ErrorCode::NotFound,
    );
    assert_code(
        store
            .get_commit_provenance(&a.tenant_id, &repository.id, &commit('f'))
            .await,
        ErrorCode::NotFound,
    );
    assert!(
        store
            .get_commit_provenance(&a.tenant_id, &repository.id, &commit('a'))
            .await
            .is_ok()
    );
    store
        .finish_repository_deletion(&a.tenant_id, &repository.id, NOW + 10)
        .await
        .unwrap();
    for digit in ['a', 'b', '9'] {
        assert_code(
            store
                .get_commit_provenance(&a.tenant_id, &repository.id, &commit(digit))
                .await,
            ErrorCode::NotFound,
        );
    }
}
