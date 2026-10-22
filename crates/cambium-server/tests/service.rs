mod common;

use async_trait::async_trait;
use cambium_backend::*;
use cambium_protocol::ObjectId;
use cambium_server::{
    secrets::{new_token, token_hash},
    service::{App, Clock, Reply},
};
use cambium_store::operations::OperationsStore;
use cambium_store::{Bootstrap, SqliteStore, Store, TokenSettings};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI64, Ordering},
};
use tokio::sync::Notify;

const NOW: i64 = 1_800_000_000_000;

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct FaultBackend {
    backend: cambium_backend_enroute::EnrouteBackend,
    lose_next_response: AtomicBool,
    block_next: AtomicBool,
    admitted: Notify,
    release: Notify,
}

#[async_trait]
impl Backend for FaultBackend {
    fn capabilities(&self) -> Capabilities {
        self.backend.capabilities()
    }
    async fn create_repository(
        &self,
        id: &BackendRepositoryId,
        branch: &str,
    ) -> Result<(), BackendError> {
        self.backend.create_repository(id, branch).await
    }
    async fn delete_repository(&self, id: &BackendRepositoryId) -> Result<(), BackendError> {
        self.backend.delete_repository(id).await
    }
    async fn seal_repository(&self, id: &BackendRepositoryId) -> Result<(), BackendError> {
        self.backend.seal_repository(id).await
    }
    async fn list_repositories(
        &self,
        prefix: &str,
        page: Option<&str>,
    ) -> Result<RepositoryPage, BackendError> {
        self.backend.list_repositories(prefix, page).await
    }
    async fn list_refs(
        &self,
        id: &BackendRepositoryId,
        prefixes: &[String],
    ) -> Result<RefSnapshot, BackendError> {
        self.backend.list_refs(id, prefixes).await
    }
    async fn update_refs(
        &self,
        id: &BackendRepositoryId,
        updates: &[RefUpdate],
    ) -> Result<Vec<RefOutcome>, BackendError> {
        self.backend.update_refs(id, updates).await
    }
    async fn read_object(
        &self,
        id: &BackendRepositoryId,
        oid: &ObjectId,
    ) -> Result<RawObject, BackendError> {
        self.backend.read_object(id, oid).await
    }
    async fn is_ancestor(
        &self,
        id: &BackendRepositoryId,
        a: &ObjectId,
        b: &ObjectId,
    ) -> Result<bool, BackendError> {
        self.backend.is_ancestor(id, a, b).await
    }
    async fn create_commit(
        &self,
        id: &BackendRepositoryId,
        request: &CommitRequest,
    ) -> Result<CommitResult, BackendError> {
        if self.block_next.swap(false, Ordering::SeqCst) {
            self.admitted.notify_one();
            self.release.notified().await;
        }
        let result = self.backend.create_commit(id, request).await?;
        if self.lose_next_response.swap(false, Ordering::SeqCst) {
            return Err(BackendError::Unavailable);
        }
        Ok(result)
    }
    async fn copy_refs(
        &self,
        source: &BackendRepositoryId,
        destination: &BackendRepositoryId,
        copies: &[RefCopy],
        atomic: bool,
    ) -> Result<(), BackendError> {
        self.backend
            .copy_refs(source, destination, copies, atomic)
            .await
    }
}

async fn call(app: &Arc<App>, token: &str, operation: &str, input: Value) -> Reply {
    app.call(token.to_owned(), operation.to_owned(), input, None)
        .await
}
fn success(reply: Reply) -> Value {
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    reply.body
}
fn error(reply: Reply, code: &str) {
    assert_eq!(reply.body["error"]["code"], code, "{:?}", reply);
}

#[tokio::test]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn shared_operations_preserve_authorization_bytes_and_idempotent_effects() {
    let server = common::Server::start().await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("metadata.sqlite");
    let store = Arc::new(
        SqliteStore::open(&path, TokenSettings::default())
            .await
            .unwrap(),
    );
    let clock = Arc::new(TestClock(AtomicI64::new(NOW)));
    let backend = Arc::new(FaultBackend {
        backend: server.backend.clone(),
        lose_next_response: AtomicBool::new(false),
        block_next: AtomicBool::new(false),
        admitted: Notify::new(),
        release: Notify::new(),
    });
    let app = Arc::new(App::new(
        store.clone(),
        backend.clone(),
        clock.clone(),
        &[42; 32],
        server.git.clone(),
    ));
    let mut users = Vec::new();
    for slug in ["a", "b"] {
        let secret = new_token().unwrap();
        let principal = store
            .deployment_bootstrap(
                &Bootstrap {
                    tenant_slug: slug.into(),
                    human_name: "Operator".into(),
                    token_hash: token_hash(&secret).unwrap(),
                    expires_at_ms: NOW + 86_400_000,
                },
                NOW,
            )
            .await
            .unwrap();
        users.push((secret, principal));
    }
    let (token, principal) = &users[0];
    let other = &users[1].0;
    error(
        call(&app, "", "create_repository", json!({"bad": true})).await,
        "unauthenticated",
    );
    error(
        call(
            &app,
            token,
            "create_repository",
            json!({"name": "project", "typo": 1}),
        )
        .await,
        "invalid_request",
    );
    let created = success(
        call(
            &app,
            token,
            "create_repository",
            json!({"name": "project", "idempotency_key": "create"}),
        )
        .await,
    );
    let repo = &created["repository_id"];
    assert_eq!(created["default_branch"], "refs/heads/main");
    let replay = call(
        &app,
        token,
        "create_repository",
        json!({"idempotency_key": "create", "name": "project"}),
    )
    .await;
    assert!(replay.replayed);
    assert_eq!(success(replay), created);
    error(
        call(
            &app,
            token,
            "create_repository",
            json!({"name": "different", "idempotency_key": "create"}),
        )
        .await,
        "idempotency_key_reused",
    );
    for (operation, input) in [
        ("get_repository", json!({"repository_id": repo})),
        ("delete_repository", json!({"repository_id": repo})),
        ("read_file", json!({"repository_id": repo, "path": "a"})),
        ("list_tree", json!({"repository_id": repo})),
        (
            "diff",
            json!({"repository_id": repo, "from": "HEAD", "to": "HEAD"}),
        ),
        (
            "write_changeset",
            json!({"repository_id": repo, "message": "x", "changes": []}),
        ),
        (
            "get_identity",
            json!({"identity_id": principal.actor.identity_id}),
        ),
        (
            "disable_identity",
            json!({"identity_id": principal.actor.identity_id}),
        ),
        ("revoke_token", json!({"token_id": principal.token_id})),
        (
            "mint_token",
            json!({"identity_id": principal.actor.identity_id, "scope": "repo:read"}),
        ),
    ] {
        error(call(&app, other, operation, input).await, "not_found");
    }
    assert_eq!(
        success(call(&app, other, "list_repositories", json!({})).await)["repositories"],
        json!([])
    );
    let write = json!({"repository_id": repo, "message": "Unicode and bytes\n", "idempotency_key": "first", "changes": [
        {"action": "put", "path": "文/é", "contents_b64": "AP8="},
        {"action": "put", "path": "文/e\u{301}", "contents": "separate"},
        {"action": "put", "path": "Case", "contents": "upper"},
        {"action": "put", "path": "case", "contents": "lower", "mode": "executable"}
    ]});
    let first = success(call(&app, token, "write_changeset", write.clone()).await);
    assert_eq!(first["warnings"][0]["code"], "path_collision");
    assert_eq!(
        success(call(&app, token, "write_changeset", write.clone()).await),
        first
    );
    let file = success(
        call(
            &app,
            token,
            "read_file",
            json!({"repository_id": repo, "path": "文/é"}),
        )
        .await,
    );
    assert_eq!(file["contents_b64"], "AP8=");
    assert!(file.get("contents").is_none());
    let page = success(
        call(
            &app,
            token,
            "list_tree",
            json!({"repository_id": repo, "limit": 2}),
        )
        .await,
    );
    assert_eq!(page["truncated"], true);
    let page2 = success(
        call(
            &app,
            token,
            "list_tree",
            json!({"repository_id": repo, "limit": 2, "page_token": page["next_page_token"]}),
        )
        .await,
    );
    assert_eq!(page2["revision"], first["commit_id"]);
    error(call(&app, token, "write_changeset", json!({"repository_id": repo, "message": "bad path", "changes": [{"action": "put", "path_b64": "/w==", "contents": "x"}]})).await, "unsupported_path");

    let narrow = success(call(&app, token, "mint_token", json!({"identity_id": principal.actor.identity_id, "scope": "repo:read", "repositories": [repo], "idempotency_key": "mint-reader"})).await);
    let reader = narrow["token"].as_str().unwrap();
    error(
        call(&app, reader, "write_changeset", write).await,
        "permission_denied",
    );
    error(
        call(
            &app,
            reader,
            "create_repository",
            json!({"name": "project", "idempotency_key": "create"}),
        )
        .await,
        "permission_denied",
    );
    let minted_admin = json!({"identity_id": principal.actor.identity_id, "scope": "tenant:admin", "idempotency_key": "mint-admin"});
    success(call(&app, token, "mint_token", minted_admin.clone()).await);
    error(
        call(&app, reader, "mint_token", minted_admin).await,
        "permission_denied",
    );

    backend.lose_next_response.store(true, Ordering::SeqCst);
    let lost = json!({"repository_id": repo, "message": "lost response", "idempotency_key": "lost", "changes": [{"action": "put", "path": "lost", "contents": "once"}]});
    error(
        call(&app, token, "write_changeset", lost.clone()).await,
        "backend_unavailable",
    );
    let conflict = call(&app, token, "write_changeset", lost).await;
    assert_eq!(conflict.body["error"]["code"], "ref_conflict");
    assert_eq!(
        conflict.body["error"]["details"]["expected"],
        first["commit_id"]
    );
    let after = success(call(&app, token, "list_tree", json!({"repository_id": repo})).await);
    assert_eq!(
        conflict.body["error"]["details"]["actual"],
        after["revision"]
    );

    backend.block_next.store(true, Ordering::SeqCst);
    let blocked = json!({"repository_id": repo, "message": "disconnect", "idempotency_key": "disconnect", "changes": [{"action": "put", "path": "finished", "contents": "yes"}]});
    let task_app = app.clone();
    let task_token = token.clone();
    let task_input = blocked.clone();
    let caller =
        tokio::spawn(
            async move { call(&task_app, &task_token, "write_changeset", task_input).await },
        );
    backend.admitted.notified().await;
    error(
        call(&app, token, "write_changeset", blocked.clone()).await,
        "idempotency_key_in_use",
    );
    caller.abort();
    backend.release.notify_one();
    let mut replayed = false;
    for _ in 0..100 {
        let reply = call(&app, token, "write_changeset", blocked.clone()).await;
        if reply.status == 200 {
            assert!(reply.replayed);
            replayed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(replayed, "the detached mutation never finished");
    let deletion = json!({"repository_id": repo, "idempotency_key": "delete"});
    success(call(&app, token, "delete_repository", deletion.clone()).await);
    let replay = call(&app, token, "delete_repository", deletion).await;
    assert!(replay.replayed);
    success(replay);
    error(
        call(
            &app,
            other,
            "delete_repository",
            json!({"repository_id": repo, "idempotency_key": "delete"}),
        )
        .await,
        "not_found",
    );
    let pending = store
        .reserve_repository(principal, "interrupted", "refs/heads/main", NOW)
        .await
        .unwrap();
    app.recover_at_startup().await.unwrap();
    assert_eq!(
        store
            .get_repository(&principal.tenant_id, &pending.id)
            .await
            .unwrap()
            .state,
        "ready"
    );
    store
        .begin_repository_deletion(principal, &pending.id, NOW)
        .await
        .unwrap();
    let backend_id: BackendRepositoryId = pending.backend_id().try_into().unwrap();
    backend.delete_repository(&backend_id).await.unwrap();
    app.resume_repositories().await.unwrap();
    assert!(
        store
            .get_repository(&principal.tenant_id, &pending.id)
            .await
            .is_err()
    );
    store.close().await;
    let bytes = std::fs::read(&path).unwrap();
    for secret in [token.as_bytes(), reader.as_bytes()] {
        assert!(!bytes.windows(secret.len()).any(|v| v == secret));
    }
}
