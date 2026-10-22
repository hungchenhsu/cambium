use cambium_protocol::{ErrorCode, IdentityKind, Principal, Scope};
use cambium_store::{
    Bootstrap, CreateIdentity, MintToken, SqliteStore, Store, StoreError, TokenHash, TokenSettings,
};
use tempfile::TempDir;

const NOW: i64 = 1_800_000_000_000;

async fn database() -> (TempDir, SqliteStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(
        directory.path().join("metadata.sqlite"),
        TokenSettings::default(),
    )
    .await
    .unwrap();
    (directory, store)
}

async fn bootstrap(store: &dyn Store, slug: &str, hash: u8) -> Principal {
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

fn mint(principal: &Principal, hash: u8) -> MintToken {
    MintToken {
        identity_id: principal.actor.identity_id.clone(),
        token_hash: TokenHash([hash; 32]),
        scope: principal.scope,
        repositories: principal.repositories.clone(),
        expires_at_ms: principal.expires_at_ms,
    }
}

fn assert_code<T>(result: Result<T, StoreError>, expected: ErrorCode) {
    match result {
        Ok(_) => panic!("expected {expected:?}"),
        Err(error) => assert_eq!(error.code(), expected, "{error:?}"),
    }
}

#[tokio::test]
async fn tenant_scoped_methods_do_not_read_or_change_another_tenant() {
    let (_directory, store) = database().await;
    let a = bootstrap(&store, "a", 1).await;
    let b = bootstrap(&store, "b", 2).await;
    let repo = store
        .reserve_repository(&b, "project", "main", NOW)
        .await
        .unwrap();
    assert_code(
        store.get_identity(&a.tenant_id, &b.actor.identity_id).await,
        ErrorCode::NotFound,
    );
    assert_code(
        store.get_repository(&a.tenant_id, &repo.id).await,
        ErrorCode::NotFound,
    );
    assert_code(
        store.mark_repository_ready(&a, &repo.id, NOW).await,
        ErrorCode::NotFound,
    );
    assert_code(
        store.disable_identity(&a, &b.actor.identity_id, NOW).await,
        ErrorCode::NotFound,
    );
    assert_code(
        store.revoke_token(&a, &b.token_id, NOW).await,
        ErrorCode::NotFound,
    );
    let mut input = mint(&a, 3);
    input.identity_id = b.actor.identity_id.clone();
    assert_code(store.mint_token(&a, &input, NOW).await, ErrorCode::NotFound);
    assert_code(
        store
            .deployment_issue_human_token(&a.tenant_id, &input, NOW)
            .await,
        ErrorCode::NotFound,
    );
    assert_code(
        store
            .create_identity(
                &a,
                &CreateIdentity {
                    kind: IdentityKind::Agent,
                    name: "Agent".into(),
                    owner_id: Some(b.actor.identity_id.clone()),
                },
                NOW,
            )
            .await,
        ErrorCode::NotFound,
    );
    let mut input = mint(&a, 4);
    input.repositories = Some(vec![repo.id.clone()]);
    assert_code(store.mint_token(&a, &input, NOW).await, ErrorCode::NotFound);
    assert!(
        store
            .deployment_authenticate(TokenHash([2; 32]), NOW + 1)
            .await
            .is_ok()
    );
    assert_eq!(
        store
            .get_repository(&b.tenant_id, &repo.id)
            .await
            .unwrap()
            .state,
        "creating"
    );
}

#[tokio::test]
async fn delegation_cannot_expand_scope_repositories_or_expiry() {
    let (_directory, store) = database().await;
    let root = bootstrap(&store, "tenant", 1).await;
    let one = store
        .reserve_repository(&root, "one", "main", NOW)
        .await
        .unwrap();
    let two = store
        .reserve_repository(&root, "two", "main", NOW)
        .await
        .unwrap();
    let mut input = mint(&root, 2);
    input.scope = Scope::RepoWrite;
    input.repositories = Some(vec![one.id.clone()]);
    input.expires_at_ms = NOW + 10_000;
    let child = store.mint_token(&root, &input, NOW).await.unwrap();
    let mut input = mint(&child, 3);
    input.scope = Scope::RepoAdmin;
    assert_code(
        store.mint_token(&child, &input, NOW).await,
        ErrorCode::PermissionDenied,
    );
    input.scope = Scope::RepoRead;
    input.expires_at_ms += 1;
    assert_code(
        store.mint_token(&child, &input, NOW).await,
        ErrorCode::PermissionDenied,
    );
    input.expires_at_ms -= 1;
    input.repositories = None;
    assert_code(
        store.mint_token(&child, &input, NOW).await,
        ErrorCode::PermissionDenied,
    );
    input.repositories = Some(vec![two.id.clone()]);
    assert_code(
        store.mint_token(&child, &input, NOW).await,
        ErrorCode::PermissionDenied,
    );
    input.repositories = Some(vec![]);
    let grandchild = store.mint_token(&child, &input, NOW).await.unwrap();
    assert!(!grandchild.can_see(&one.id));
    assert_eq!(
        grandchild
            .require_repository(&one.id, Scope::RepoAdmin)
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        child
            .require_repository(&one.id, Scope::RepoAdmin)
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_code(
        store.reserve_repository(&child, "three", "main", NOW).await,
        ErrorCode::PermissionDenied,
    );
    assert_code(
        store
            .mint_token(
                &root,
                &MintToken {
                    expires_at_ms: NOW,
                    ..mint(&root, 4)
                },
                NOW,
            )
            .await,
        ErrorCode::InvalidRequest,
    );
}

#[tokio::test]
async fn agent_admin_cannot_become_human_or_create_a_system_identity() {
    let (_directory, store) = database().await;
    let human = bootstrap(&store, "tenant", 1).await;
    let agent = store
        .create_identity(
            &human,
            &CreateIdentity {
                kind: IdentityKind::Agent,
                name: "Agent".into(),
                owner_id: Some(human.actor.identity_id.clone()),
            },
            NOW,
        )
        .await
        .unwrap();
    let input = MintToken {
        identity_id: agent.id.clone(),
        ..mint(&human, 2)
    };
    let admin = store.mint_token(&human, &input, NOW).await.unwrap();
    assert_eq!(admin.actor.kind, IdentityKind::Agent);
    assert_eq!(agent.created_by, human.actor);
    assert_code(
        store.mint_token(&admin, &mint(&human, 3), NOW).await,
        ErrorCode::PermissionDenied,
    );
    let other_human = store
        .create_identity(
            &admin,
            &CreateIdentity {
                kind: IdentityKind::Human,
                name: "Other human".into(),
                owner_id: None,
            },
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(other_human.created_by.kind, IdentityKind::Agent);
    let input = MintToken {
        identity_id: other_human.id,
        ..mint(&admin, 4)
    };
    assert_code(
        store.mint_token(&admin, &input, NOW).await,
        ErrorCode::PermissionDenied,
    );
    assert_code(
        store
            .create_identity(
                &admin,
                &CreateIdentity {
                    kind: IdentityKind::System,
                    name: "System".into(),
                    owner_id: None,
                },
                NOW,
            )
            .await,
        ErrorCode::InvalidRequest,
    );
    let input = MintToken {
        identity_id: agent.id,
        ..mint(&admin, 5)
    };
    assert_code(
        store
            .deployment_issue_human_token(&human.tenant_id, &input, NOW)
            .await,
        ErrorCode::InvalidRequest,
    );
}

#[tokio::test]
async fn ancestor_revocation_invalidates_descendants_and_stale_principals() {
    let (_directory, store) = database().await;
    let root = bootstrap(&store, "tenant", 1).await;
    let child = store.mint_token(&root, &mint(&root, 2), NOW).await.unwrap();
    let grandchild = store
        .mint_token(&child, &mint(&child, 3), NOW)
        .await
        .unwrap();
    store
        .revoke_token(&root, &child.token_id, NOW + 1)
        .await
        .unwrap();
    for hash in [2, 3] {
        assert_code(
            store
                .deployment_authenticate(TokenHash([hash; 32]), NOW + 1)
                .await,
            ErrorCode::Unauthenticated,
        );
    }
    assert_code(
        store
            .mint_token(&grandchild, &mint(&grandchild, 4), NOW + 1)
            .await,
        ErrorCode::Unauthenticated,
    );
    assert_code(
        store
            .reserve_repository(&grandchild, "project", "main", NOW + 1)
            .await,
        ErrorCode::Unauthenticated,
    );
    assert!(
        store
            .deployment_authenticate(TokenHash([1; 32]), NOW + 1)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn disabled_ancestor_identity_invalidates_other_identity_descendants() {
    let (_directory, store) = database().await;
    let human = bootstrap(&store, "tenant", 1).await;
    let agent = store
        .create_identity(
            &human,
            &CreateIdentity {
                kind: IdentityKind::Agent,
                name: "Agent".into(),
                owner_id: Some(human.actor.identity_id.clone()),
            },
            NOW,
        )
        .await
        .unwrap();
    let agent_token = store
        .mint_token(
            &human,
            &MintToken {
                identity_id: agent.id.clone(),
                ..mint(&human, 2)
            },
            NOW,
        )
        .await
        .unwrap();
    store
        .disable_identity(&human, &human.actor.identity_id, NOW + 1)
        .await
        .unwrap();
    assert_code(
        store
            .deployment_authenticate(TokenHash([2; 32]), NOW + 1)
            .await,
        ErrorCode::Unauthenticated,
    );
    assert_code(
        store
            .create_identity(
                &agent_token,
                &CreateIdentity {
                    kind: IdentityKind::Human,
                    name: "New human".into(),
                    owner_id: None,
                },
                NOW + 1,
            )
            .await,
        ErrorCode::Unauthenticated,
    );
    assert!(
        store
            .get_identity(&human.tenant_id, &agent.id)
            .await
            .unwrap()
            .disabled_at_ms
            .is_none()
    );
}

#[tokio::test]
async fn kind_lifetime_and_chain_depth_are_checked_before_insertion() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(
        directory.path().join("metadata.sqlite"),
        TokenSettings {
            agent_max_lifetime_ms: 1_000,
            max_delegation_depth: 2,
            ..TokenSettings::default()
        },
    )
    .await
    .unwrap();
    let root = bootstrap(&store, "tenant", 1).await;
    let agent = store
        .create_identity(
            &root,
            &CreateIdentity {
                kind: IdentityKind::Agent,
                name: "Agent".into(),
                owner_id: Some(root.actor.identity_id.clone()),
            },
            NOW,
        )
        .await
        .unwrap();
    let mut input = MintToken {
        identity_id: agent.id,
        ..mint(&root, 2)
    };
    assert_code(
        store.mint_token(&root, &input, NOW).await,
        ErrorCode::InvalidRequest,
    );
    input.expires_at_ms = NOW + 1_000;
    let child = store.mint_token(&root, &input, NOW).await.unwrap();
    assert_code(
        store.mint_token(&child, &mint(&child, 3), NOW).await,
        ErrorCode::InvalidRequest,
    );
    assert!(
        store
            .deployment_authenticate(TokenHash([2; 32]), NOW + 999)
            .await
            .is_ok()
    );
    assert_code(
        store
            .deployment_authenticate(TokenHash([2; 32]), NOW + 1_000)
            .await,
        ErrorCode::Unauthenticated,
    );
}

#[tokio::test]
async fn records_survive_reopen_and_repository_names_are_tenant_scoped() {
    let (directory, store) = database().await;
    let a = bootstrap(&store, "a", 1).await;
    let b = bootstrap(&store, "b", 2).await;
    let one = store
        .reserve_repository(&a, "project", "main", NOW)
        .await
        .unwrap();
    let two = store
        .reserve_repository(&b, "project", "main", NOW)
        .await
        .unwrap();
    assert_ne!(one.backend_id(), two.backend_id());
    assert_eq!(one.created_by, a.actor);
    assert_code(
        store.reserve_repository(&a, "project", "main", NOW).await,
        ErrorCode::AlreadyExists,
    );
    for branch in [
        "HEAD", "", "x..y", "/main", "main/", ".hidden", "x.lock", "main@{0}", "x\n",
    ] {
        assert_code(
            store.reserve_repository(&a, "invalid", branch, NOW).await,
            ErrorCode::InvalidRequest,
        );
    }
    store
        .mark_repository_ready(&a, &one.id, NOW + 1)
        .await
        .unwrap();
    let unicode = store
        .reserve_repository(&a, "unicode", "topic/\u{fffd}", NOW)
        .await
        .unwrap();
    assert_eq!(unicode.default_branch, "topic/\u{fffd}");
    store.close().await;
    let store = SqliteStore::open(
        directory.path().join("metadata.sqlite"),
        TokenSettings::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        store
            .get_repository(&a.tenant_id, &one.id)
            .await
            .unwrap()
            .state,
        "ready"
    );
    assert_eq!(
        store
            .deployment_authenticate(TokenHash([1; 32]), NOW + 2)
            .await
            .unwrap()
            .actor,
        a.actor
    );
}

#[tokio::test]
async fn caller_cannot_forge_identity_kind_scope_or_restrictions_on_a_principal() {
    let (_directory, store) = database().await;
    let root = bootstrap(&store, "tenant", 1).await;
    let mut input = mint(&root, 2);
    input.scope = Scope::RepoRead;
    input.repositories = Some(vec![]);
    let mut forged = store.mint_token(&root, &input, NOW).await.unwrap();
    forged.scope = Scope::TenantAdmin;
    forged.repositories = None;
    forged.actor.kind = IdentityKind::System;
    assert_code(
        store
            .reserve_repository(&forged, "project", "main", NOW)
            .await,
        ErrorCode::PermissionDenied,
    );
    assert_code(
        store.mint_token(&forged, &mint(&forged, 3), NOW).await,
        ErrorCode::PermissionDenied,
    );
}
