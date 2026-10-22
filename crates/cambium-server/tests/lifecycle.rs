mod common;

use async_trait::async_trait;
use cambium_backend::git_policy::{GitCommand, GitOperation, GitPolicy, GitRequest};
use cambium_backend::*;
use cambium_protocol::{BranchId, ObjectId, RepositoryId};
use cambium_server::git_policy::Policy;
use cambium_server::{
    secrets::{new_token, token_hash},
    service::{App, Clock, Reply},
};
use cambium_store::compaction::CompactionStore;
use cambium_store::lifecycle::LifecycleStore;
use cambium_store::{Bootstrap, SqliteStore, Store, TokenSettings};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
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
    list_refs_calls: AtomicUsize,
    lose_next_response: AtomicBool,
    lose_creation_response: AtomicBool,
    race_creation: AtomicBool,
    lose_seal_response: AtomicBool,
    fail_delete: AtomicBool,
    lose_copy_response: AtomicBool,
    lose_ref_response: AtomicBool,
    block_after_copy: AtomicBool,
    stale_cas_once: AtomicBool,
    fail_rescue_for: std::sync::Mutex<Option<ObjectId>>,
    block_copy: AtomicBool,
    copy_admitted: Notify,
    copy_release: Notify,
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
        if self.fail_delete.swap(false, Ordering::SeqCst) {
            return Err(BackendError::Unavailable);
        }
        self.backend.delete_repository(id).await
    }
    async fn seal_repository(&self, id: &BackendRepositoryId) -> Result<(), BackendError> {
        self.backend.seal_repository(id).await?;
        if self.lose_seal_response.swap(false, Ordering::SeqCst) {
            return Err(BackendError::Unavailable);
        }
        Ok(())
    }
    async fn create_ref_once(
        &self,
        id: &BackendRepositoryId,
        operation_id: &str,
        name: &str,
        object_id: &ObjectId,
    ) -> Result<bool, BackendError> {
        if self.race_creation.swap(false, Ordering::SeqCst) {
            let result = self
                .backend
                .update_refs(
                    id,
                    &[RefUpdate {
                        name: name.into(),
                        expected: RefExpectation::Absent,
                        new: Some(object_id.clone()),
                    }],
                )
                .await?;
            assert!(result[0].rejection.is_none());
        }
        let created = self
            .backend
            .create_ref_once(id, operation_id, name, object_id)
            .await?;
        if self.lose_creation_response.swap(false, Ordering::SeqCst) {
            return Err(BackendError::Unavailable);
        }
        Ok(created)
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
        self.list_refs_calls.fetch_add(1, Ordering::SeqCst);
        self.backend.list_refs(id, prefixes).await
    }
    async fn update_refs(
        &self,
        id: &BackendRepositoryId,
        updates: &[RefUpdate],
    ) -> Result<Vec<RefOutcome>, BackendError> {
        let fail_rescue = {
            let mut target = self.fail_rescue_for.lock().unwrap();
            if target.is_some()
                && updates.iter().any(|u| {
                    u.name.starts_with("refs/cambium-rescue/") && u.new.as_ref() == target.as_ref()
                })
            {
                target.take();
                true
            } else {
                false
            }
        };
        if fail_rescue {
            return Err(BackendError::Unavailable);
        }
        if updates.len() == 1
            && updates[0].name == "refs/heads/main"
            && self.stale_cas_once.swap(false, Ordering::SeqCst)
        {
            let parent = self
                .backend
                .list_refs(id, &["refs/heads/main".into()])
                .await?
                .refs
                .into_iter()
                .find(|r| r.name == "refs/heads/main")
                .map(|r| r.object_id);
            let identity = GitIdentity {
                name: "Concurrent writer".into(),
                email: "test@example.invalid".into(),
                timestamp_seconds: NOW / 1000,
                utc_offset_seconds: 0,
            };
            self.backend
                .create_commit(
                    id,
                    &CommitRequest {
                        branch: "refs/heads/main".into(),
                        expected_parent: parent,
                        message: "Concurrent CAS".into(),
                        author: identity.clone(),
                        committer: identity,
                        changes: vec![FileChange::Put {
                            path: b"racing".to_vec(),
                            contents: b"keep concurrent writer".to_vec(),
                            mode: FileMode::Regular,
                        }],
                    },
                )
                .await?;
        }
        let result = self.backend.update_refs(id, updates).await?;
        if updates
            .iter()
            .any(|update| update.name == "refs/heads/main")
            && self.lose_ref_response.swap(false, Ordering::SeqCst)
        {
            return Err(BackendError::Unavailable);
        }
        Ok(result)
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
        if self.block_copy.swap(false, Ordering::SeqCst) {
            self.copy_admitted.notify_one();
            self.copy_release.notified().await;
        }
        self.backend
            .copy_refs(source, destination, copies, atomic)
            .await?;
        if self.block_after_copy.swap(false, Ordering::SeqCst) {
            self.copy_admitted.notify_one();
            self.copy_release.notified().await;
        }
        if self.lose_copy_response.swap(false, Ordering::SeqCst) {
            return Err(BackendError::Unavailable);
        }
        Ok(())
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
async fn ephemeral_ownership_survives_lost_replies_expiry_and_name_reuse() {
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
        list_refs_calls: AtomicUsize::new(0),
        lose_next_response: AtomicBool::new(false),
        lose_creation_response: AtomicBool::new(false),
        race_creation: AtomicBool::new(false),
        lose_seal_response: AtomicBool::new(false),
        fail_delete: AtomicBool::new(false),
        lose_copy_response: AtomicBool::new(false),
        lose_ref_response: AtomicBool::new(false),
        block_after_copy: AtomicBool::new(false),
        stale_cas_once: AtomicBool::new(false),
        fail_rescue_for: std::sync::Mutex::new(None),
        block_copy: AtomicBool::new(false),
        copy_admitted: Notify::new(),
        copy_release: Notify::new(),
        block_next: AtomicBool::new(false),
        admitted: Notify::new(),
        release: Notify::new(),
    });
    let mut app = Arc::new(App::new(
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
    let repo = success(call(&app, token, "create_repository", json!({"name": "project"})).await)["repository_id"].clone();
    let repo_id: RepositoryId = serde_json::from_value(repo.clone()).unwrap();
    let first = success(call(&app, token, "write_changeset", json!({"repository_id": repo, "message": "Base", "changes": [{"action": "put", "path": "file", "contents": "base"}]})).await)["commit_id"].clone();
    let oid: ObjectId = serde_json::from_value(first.clone()).unwrap();
    let backend_id: BackendRepositoryId = format!("{}/{}/g1", principal.tenant_id, repo_id)
        .try_into()
        .unwrap();
    for ttl in [0, 7 * 86400 + 1] {
        error(call(&app, token, "create_ephemeral_branch", json!({"repository_id": repo, "name": "invalid", "start_commit": first, "ttl_seconds": ttl})).await, "invalid_request");
    }
    for race in [false, true] {
        let name = if race { "permanent-racer" } else { "owned" };
        let input = json!({"repository_id": repo, "name": name, "start_commit": first, "ttl_seconds": 2, "idempotency_key": name});
        backend.race_creation.store(race, Ordering::SeqCst);
        backend.lose_creation_response.store(true, Ordering::SeqCst);
        error(
            call(&app, token, "create_ephemeral_branch", input.clone()).await,
            "backend_unavailable",
        );
        error(
            call(
                &app,
                token,
                "read_file",
                json!({"repository_id": repo, "revision": name, "path": "file"}),
            )
            .await,
            "repository_busy",
        );
        error(
            call(&app, token, "write_changeset", json!({"repository_id": repo, "branch": name, "expected": {"kind": "at", "object_id": first}, "message": "Pending creation", "changes": [{"action": "put", "path": "file", "contents": "pending"}]})).await,
            "repository_busy",
        );
        assert_eq!(
            store
                .pending_branches(&principal.tenant_id, &repo_id, NOW)
                .await
                .unwrap()
                .len(),
            1
        );
        // Reopen the database and recover the original receipt after process loss.
        let reopened = Arc::new(
            SqliteStore::open(&path, TokenSettings::default())
                .await
                .unwrap(),
        );
        app = Arc::new(App::new(
            reopened,
            backend.clone(),
            clock.clone(),
            &[42; 32],
            server.git.clone(),
        ));
        app.recover_at_startup().await.unwrap();
        app.run_lifecycle_once().await.unwrap();
        let reply = call(&app, token, "create_ephemeral_branch", input).await;
        if race {
            error(reply, "already_exists");
        } else {
            assert_eq!(success(reply)["state"], "active");
        }
    }
    let branches = success(
        call(
            &app,
            token,
            "list_branches",
            json!({"repository_id": repo, "limit": 1}),
        )
        .await,
    );
    let branch = &branches["branches"][0];
    let branch_id: BranchId = serde_json::from_value(branch["branch_id"].clone()).unwrap();
    assert_eq!(branch["name"], "refs/heads/owned");
    assert_eq!(branches["truncated"], false);
    let reader = success(call(&app, token, "mint_token", json!({"identity_id": principal.actor.identity_id, "scope": "repo:read", "repositories": [repo], "ttl_seconds": 3600})).await)["token"].as_str().unwrap().to_owned();
    for (name, input) in [
        (
            "create_ephemeral_branch",
            json!({"repository_id": repo, "name": "cross", "start_commit": first}),
        ),
        (
            "get_branch",
            json!({"repository_id": repo, "branch_id": branch_id}),
        ),
        ("list_branches", json!({"repository_id": repo})),
        (
            "extend_branch",
            json!({"repository_id": repo, "branch_id": branch_id, "ttl_seconds": 10}),
        ),
        (
            "promote_branch",
            json!({"repository_id": repo, "branch_id": branch_id}),
        ),
    ] {
        error(
            call(&app, &users[1].0, name, input.clone()).await,
            "not_found",
        );
        if !matches!(name, "get_branch" | "list_branches") {
            error(call(&app, &reader, name, input).await, "permission_denied");
        }
    }
    let policy = Policy { app: app.clone() };
    let grant = policy
        .authorize(&GitRequest {
            path: "/a/project.git".into(),
            operation: GitOperation::Write,
            headers: vec![(
                "authorization".into(),
                format!("Bearer {token}").into_bytes(),
            )],
        })
        .await
        .unwrap();
    clock.0.store(NOW + 2000, Ordering::SeqCst);
    for (name, input) in [
        (
            "read_file",
            json!({"repository_id": repo, "revision": "owned", "path": "file"}),
        ),
        (
            "list_tree",
            json!({"repository_id": repo, "revision": "owned"}),
        ),
        (
            "diff",
            json!({"repository_id": repo, "from": "owned", "to": "main"}),
        ),
        (
            "write_changeset",
            json!({"repository_id": repo, "branch": "owned", "message": "late", "changes": [{"action": "put", "path": "file", "contents": "late"}]}),
        ),
        (
            "extend_branch",
            json!({"repository_id": repo, "branch_id": branch_id, "ttl_seconds": 10}),
        ),
        (
            "promote_branch",
            json!({"repository_id": repo, "branch_id": branch_id}),
        ),
    ] {
        error(call(&app, token, name, input).await, "branch_expired");
    }
    assert_eq!(
        policy
            .visible_refs(
                &grant.context,
                &grant.repository,
                &["refs/heads/owned".into(), "refs/heads/main".into()]
            )
            .await
            .unwrap(),
        vec!["refs/heads/main"]
    );
    let command = GitCommand {
        force: false,
        name: "refs/heads/owned".into(),
        old: Some(oid.clone()),
        new: Some(oid.clone()),
    };
    assert!(
        policy
            .pre_receive(&grant.context, &grant.repository, &[command])
            .await
            .unwrap()[0]
            .is_some()
    );
    app.run_lifecycle_once().await.unwrap();
    let names: Vec<_> = backend
        .list_refs(&backend_id, &[])
        .await
        .unwrap()
        .refs
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert!(!names.contains(&"refs/heads/owned".into()));
    assert!(names.contains(&"refs/heads/permanent-racer".into()));
    assert_eq!(
        success(
            call(
                &app,
                token,
                "read_file",
                json!({"repository_id": repo, "revision": first, "path": "file"})
            )
            .await
        )["contents"],
        "base"
    );
    // Model a previously authorized Git push landing after the first deletion.
    assert!(
        backend
            .update_refs(
                &backend_id,
                &[RefUpdate {
                    name: "refs/heads/owned".into(),
                    expected: RefExpectation::Absent,
                    new: Some(oid.clone())
                }]
            )
            .await
            .unwrap()[0]
            .rejection
            .is_none()
    );
    app.run_lifecycle_once().await.unwrap();
    assert!(
        backend
            .list_refs(&backend_id, &["refs/heads/owned".into()])
            .await
            .unwrap()
            .refs
            .is_empty()
    );
    let replacement = success(call(&app, token, "create_ephemeral_branch", json!({"repository_id": repo, "name": "owned", "start_commit": first, "ttl_seconds": 3})).await);
    let new_id = &replacement["branch_id"];
    let extended_input = json!({"repository_id": repo, "branch_id": new_id, "ttl_seconds": 5, "idempotency_key": "extend"});
    let extended = success(call(&app, token, "extend_branch", extended_input.clone()).await);
    clock.0.store(NOW + 3000, Ordering::SeqCst);
    let replay = call(&app, token, "extend_branch", extended_input).await;
    assert!(replay.replayed);
    assert_eq!(success(replay), extended);
    let promoted = success(
        call(
            &app,
            token,
            "promote_branch",
            json!({"repository_id": repo, "branch_id": new_id}),
        )
        .await,
    );
    assert_eq!(promoted["state"], "promoted");
    assert!(promoted.get("expires_at").is_none());
    clock.0.store(NOW + 10000, Ordering::SeqCst);
    app.run_lifecycle_once().await.unwrap();
    assert_eq!(
        backend
            .list_refs(&backend_id, &["refs/heads/owned".into()])
            .await
            .unwrap()
            .refs[0]
            .object_id,
        oid
    );
    success(call(&app, token, "write_changeset", json!({"repository_id": repo, "branch": "owned", "message": "permanent", "changes": [{"action": "put", "path": "file", "contents": "kept"}]})).await);
    let final_record = success(
        call(
            &app,
            token,
            "get_branch",
            json!({"repository_id": repo, "branch_id": branch_id}),
        )
        .await,
    );
    assert_eq!(final_record["state"], "expired");
}

#[tokio::test]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn generation_compaction_preserves_live_hashes_and_reclaims_only_fenced_expiries() {
    let server = common::Server::start().await;
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteStore::open(
            directory.path().join("metadata.sqlite"),
            TokenSettings::default(),
        )
        .await
        .unwrap(),
    );
    let clock = Arc::new(TestClock(AtomicI64::new(NOW)));
    let secret = new_token().unwrap();
    let principal = store
        .deployment_bootstrap(
            &Bootstrap {
                tenant_slug: "compact".into(),
                human_name: "Operator".into(),
                token_hash: token_hash(&secret).unwrap(),
                expires_at_ms: NOW + 86_400_000,
            },
            NOW,
        )
        .await
        .unwrap();
    let mut application = App::new(
        store.clone(),
        Arc::new(server.backend.clone()),
        clock.clone(),
        &[7; 32],
        server.git.clone(),
    );
    application.compaction.settle_ms = 100;
    application.compaction.drain_ms = 1000;
    let app = Arc::new(application);
    let repo = success(
        call(
            &app,
            &secret,
            "create_repository",
            json!({"name": "project"}),
        )
        .await,
    )["repository_id"]
        .clone();
    let repository: RepositoryId = serde_json::from_value(repo.clone()).unwrap();
    let base = success(call(&app, &secret, "write_changeset", json!({"repository_id": repo, "message": "base", "changes": [{"action": "put", "path": "kept", "contents": "keep"}]})).await)["commit_id"].clone();
    success(call(&app, &secret, "create_ephemeral_branch", json!({"repository_id": repo, "name": "temporary", "start_commit": base, "ttl_seconds": 1})).await);
    success(call(&app, &secret, "create_ephemeral_branch", json!({"repository_id": repo, "name": "reused", "start_commit": base, "ttl_seconds": 2})).await);
    success(call(&app, &secret, "write_changeset", json!({"repository_id": repo, "branch": "temporary", "message": "temporary", "changes": [{"action": "put", "path": "discarded", "contents": "unique transient data"}]})).await);
    let blob: ObjectId = serde_json::from_value(
        success(
            call(
                &app,
                &secret,
                "read_file",
                json!({"repository_id": repo, "revision": "temporary", "path": "discarded"}),
            )
            .await,
        )["object_id"]
            .clone(),
    )
    .unwrap();
    let old: BackendRepositoryId = format!("{}/{}/g1", principal.tenant_id, repository)
        .try_into()
        .unwrap();
    let new: BackendRepositoryId = format!("{}/{}/g2", principal.tenant_id, repository)
        .try_into()
        .unwrap();
    clock.0.store(NOW + 1000, Ordering::SeqCst);
    let requested = success(
        call(
            &app,
            &secret,
            "request_compaction",
            json!({"repository_id": repo}),
        )
        .await,
    );
    assert_eq!(requested["stage"], "preparing");
    let live = server.backend.list_refs(&old, &[]).await.unwrap();
    for expected in ["prepared", "catching_up", "verifying", "draining"] {
        app.run_lifecycle_once().await.unwrap();
        assert_eq!(
            store
                .running_compaction(&principal.tenant_id, &repository)
                .await
                .unwrap()
                .unwrap()
                .stage,
            expected
        );
    }
    assert_eq!(server.backend.list_refs(&new, &[]).await.unwrap(), live);
    assert!(matches!(
        server.backend.read_object(&new, &blob).await,
        Err(BackendError::ObjectNotFound)
    ));
    assert!(server.backend.read_object(&old, &blob).await.is_ok());
    let base: ObjectId = serde_json::from_value(base).unwrap();
    let identity = GitIdentity {
        name: "Late writer".into(),
        email: "test@example.invalid".into(),
        timestamp_seconds: NOW / 1000,
        utc_offset_seconds: 0,
    };
    let mut late = CommitRequest {
        branch: "refs/heads/main".into(),
        expected_parent: Some(base.clone()),
        message: "late".into(),
        author: identity.clone(),
        committer: identity,
        changes: vec![FileChange::Put {
            path: b"late".to_vec(),
            contents: b"late value".to_vec(),
            mode: FileMode::Regular,
        }],
    };
    let first_late = server
        .backend
        .create_commit(&old, &late)
        .await
        .unwrap()
        .commit_id;
    clock.0.store(NOW + 1100, Ordering::SeqCst);
    app.run_lifecycle_once().await.unwrap();
    assert!(
        server
            .backend
            .list_refs(&new, &[])
            .await
            .unwrap()
            .refs
            .iter()
            .any(|r| r.name == "refs/heads/main" && r.object_id == first_late)
    );
    let active = success(call(&app, &secret, "write_changeset", json!({"repository_id": repo, "message": "active independent update", "changes": [{"action": "put", "path": "active", "contents": "active"}]})).await)["commit_id"].clone();
    let mut rescued = Vec::new();
    for (index, parent) in [first_late.clone(), first_late.clone()].iter().enumerate() {
        late.expected_parent = if index == 0 {
            Some(parent.clone())
        } else {
            rescued.last().cloned()
        };
        late.message = format!("conflicting late {index}");
        late.changes = vec![FileChange::Put {
            path: b"late".to_vec(),
            contents: format!("late {index}").into_bytes(),
            mode: FileMode::Regular,
        }];
        let oid = server
            .backend
            .create_commit(&old, &late)
            .await
            .unwrap()
            .commit_id;
        rescued.push(oid);
        clock
            .0
            .store(NOW + 1200 + index as i64 * 100, Ordering::SeqCst);
        app.run_lifecycle_once().await.unwrap();
    }
    let current = server.backend.list_refs(&new, &[]).await.unwrap();
    let rescue_refs: Vec<_> = current
        .refs
        .iter()
        .filter(|r| r.name.starts_with("refs/cambium-rescue/"))
        .collect();
    assert_eq!(rescue_refs.len(), 2);
    for oid in &rescued {
        assert!(rescue_refs.iter().any(|r| &r.object_id == oid));
    }
    assert_eq!(
        current
            .refs
            .iter()
            .find(|r| r.name == "refs/heads/main")
            .unwrap()
            .object_id
            .as_str(),
        active.as_str().unwrap()
    );
    success(call(&app, &secret, "create_ephemeral_branch", json!({"repository_id": repo, "name": "expires-during-drain", "start_commit": active, "ttl_seconds": 1})).await);
    clock.0.store(NOW + 2100, Ordering::SeqCst);
    app.run_lifecycle_once().await.unwrap();
    assert_eq!(
        store
            .running_compaction(&principal.tenant_id, &repository)
            .await
            .unwrap()
            .unwrap()
            .stage,
        "sealing"
    );
    success(call(&app, &secret, "create_ephemeral_branch", json!({"repository_id": repo, "name": "reused", "start_commit": active, "ttl_seconds": 10})).await);
    let replacement = success(call(&app, &secret, "write_changeset", json!({"repository_id": repo, "branch": "reused", "message": "new branch owner", "changes": [{"action": "put", "path": "replacement", "contents": "new owner"}]})).await)["commit_id"].clone();
    late.branch = "refs/heads/reused".into();
    late.expected_parent = Some(base.clone());
    late.message = "late expired old owner".into();
    let expired_old_tip = server
        .backend
        .create_commit(&old, &late)
        .await
        .unwrap()
        .commit_id;
    clock.0.store(NOW + 3000, Ordering::SeqCst);
    for expected in ["sealed", "deleting"] {
        app.run_lifecycle_once().await.unwrap();
        assert_eq!(
            store
                .running_compaction(&principal.tenant_id, &repository)
                .await
                .unwrap()
                .unwrap()
                .stage,
            expected
        );
        assert!(server.backend.list_refs(&old, &[]).await.is_ok());
    }
    app.run_lifecycle_once().await.unwrap();
    assert!(matches!(
        server.backend.list_refs(&old, &[]).await,
        Err(BackendError::RepositoryNotFound)
    ));
    assert!(
        store
            .running_compaction(&principal.tenant_id, &repository)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .expiry_summary(&principal.tenant_id, &repository)
            .await
            .unwrap()
            .count,
        2
    );
    let final_refs = server.backend.list_refs(&new, &[]).await.unwrap();
    assert!(
        final_refs
            .refs
            .iter()
            .all(|r| !r.name.starts_with("refs/cambium/"))
    );
    assert_eq!(final_refs.head, live.head);
    assert!(
        final_refs.refs.iter().any(|r| r.name == "refs/heads/reused"
            && r.object_id.as_str() == replacement.as_str().unwrap())
    );
    assert!(matches!(
        server.backend.read_object(&new, &expired_old_tip).await,
        Err(BackendError::ObjectNotFound)
    ));
    let status = success(
        call(
            &app,
            &secret,
            "get_reclamation",
            json!({"repository_id": repo}),
        )
        .await,
    );
    assert_eq!(status["compaction"]["stage"], "complete");
    assert_eq!(status["compaction"]["conflict_count"], 2);
}

struct ObservedCopies {
    inner: cambium_server::internal_copy::CopyTokens,
    secrets: std::sync::Mutex<Vec<String>>,
}
#[async_trait]
impl cambium_backend_enroute::CopyCredentials for ObservedCopies {
    async fn for_copy(
        &self,
        destination: &BackendRepositoryId,
        copies: &[RefCopy],
    ) -> Result<String, BackendError> {
        let secret = self.inner.for_copy(destination, copies).await?;
        self.secrets.lock().unwrap().push(secret.clone());
        Ok(secret)
    }
    async fn finished_copy(&self, secret: &str) -> Result<(), BackendError> {
        self.inner.finished_copy(secret).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn signed_internal_copies_are_scoped_revoked_and_free_old_layer_files() {
    use cambium_backend_enroute::{
        CopyCredentials, EnrouteBackend, hooks::HookAdapter, signature::SigningKey,
    };
    use cambium_server::{
        internal_copy::{CopyTokens, copy_token_hash},
        transport::{PublicState, hook_router, public_router},
    };
    let hook_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/api/enroute/hooks",
        hook_listener.local_addr().unwrap()
    );
    let signing = SigningKey::from_seed(&[21; 32]);
    let server =
        common::Server::start_with_options(Some((&endpoint, &signing.to_pem().unwrap())), true)
            .await;
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteStore::open(
            directory.path().join("metadata.sqlite"),
            TokenSettings::default(),
        )
        .await
        .unwrap(),
    );
    let clock = Arc::new(TestClock(AtomicI64::new(NOW)));
    let copies = Arc::new(ObservedCopies {
        inner: CopyTokens {
            store: store.clone(),
            clock: clock.clone(),
        },
        secrets: std::sync::Mutex::new(Vec::new()),
    });
    let backend = Arc::new(
        EnrouteBackend::connect(&server.api, &server.git, Some(copies.clone()))
            .await
            .unwrap(),
    );
    let mut application = App::new(
        store.clone(),
        backend.clone(),
        clock.clone(),
        &[91; 32],
        server.git.clone(),
    );
    application.compaction.drain_ms = 0;
    let app = Arc::new(application);
    let policy = Arc::new(Policy { app: app.clone() });
    let adapter = Arc::new(
        HookAdapter::new(vec![signing.verifying_key()], &endpoint, policy.clone()).unwrap(),
    );
    let hooks = tokio::spawn(async move {
        axum::serve(hook_listener, hook_router(adapter))
            .await
            .unwrap();
    });
    let public_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let public_url = format!("http://{}", public_listener.local_addr().unwrap());
    let public_app = app.clone();
    let public = tokio::spawn(async move {
        axum::serve(
            public_listener,
            public_router(PublicState {
                app: public_app,
                allowed_origins: Vec::new(),
            }),
        )
        .await
        .unwrap();
    });
    let secret = new_token().unwrap();
    let principal = store
        .deployment_bootstrap(
            &Bootstrap {
                tenant_slug: "signed".into(),
                human_name: "Operator".into(),
                token_hash: token_hash(&secret).unwrap(),
                expires_at_ms: NOW + 86_400_000,
            },
            NOW,
        )
        .await
        .unwrap();
    let repo = success(
        call(
            &app,
            &secret,
            "create_repository",
            json!({"name": "project"}),
        )
        .await,
    )["repository_id"]
        .clone();
    let repository: RepositoryId = serde_json::from_value(repo.clone()).unwrap();
    let base = success(call(&app, &secret, "write_changeset", json!({"repository_id": repo, "message": "base", "changes": [{"action": "put", "path": "base", "contents": "permanent"}]})).await)["commit_id"].clone();
    success(call(&app, &secret, "create_ephemeral_branch", json!({"repository_id": repo, "name": "discard", "start_commit": base, "ttl_seconds": 1})).await);
    success(call(&app, &secret, "write_changeset", json!({"repository_id": repo, "branch": "discard", "message": "discard", "changes": [{"action": "put", "path": "discard", "contents": "temporary object"}]})).await);
    let old: BackendRepositoryId = format!("{}/{}/g1", principal.tenant_id, repository)
        .try_into()
        .unwrap();
    let new: BackendRepositoryId = format!("{}/{}/g2", principal.tenant_id, repository)
        .try_into()
        .unwrap();
    assert!(
        !server.layer_files(old.as_str()).is_empty(),
        "the old generation must occupy object-store layer files before deletion"
    );
    clock.0.store(NOW + 1000, Ordering::SeqCst);
    success(
        call(
            &app,
            &secret,
            "request_compaction",
            json!({"repository_id": repo}),
        )
        .await,
    );
    let record = store
        .running_compaction(&principal.tenant_id, &repository)
        .await
        .unwrap()
        .unwrap();
    let ref_copy = RefCopy {
        source: "refs/heads/main".into(),
        destination: format!("refs/cambium/{}/test", record.id),
        allow_non_fast_forward: false,
    };
    let internal = copies
        .for_copy(&new, std::slice::from_ref(&ref_copy))
        .await
        .unwrap();
    assert!(app.authenticate(&internal).await.is_err());
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for path in ["/mcp", "/v1/operations/mint_token"] {
        assert_eq!(
            client
                .post(format!("{public_url}{path}"))
                .bearer_auth(&internal)
                .json(&json!({}))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    let request = GitRequest {
        path: format!("/{}.git", new.as_str()),
        operation: GitOperation::Write,
        headers: vec![(
            "authorization".into(),
            format!("Bearer {internal}").into_bytes(),
        )],
    };
    let grant = policy.authorize(&request).await.unwrap();
    assert!(
        policy
            .authorize(&GitRequest {
                path: format!("/{}.git", old.as_str()),
                ..request.clone()
            })
            .await
            .is_err()
    );
    let reserved = vec!["refs/cambium/hidden".into(), "refs/heads/anything".into()];
    assert_eq!(
        policy
            .visible_refs(&grant.context, &new, &reserved)
            .await
            .unwrap(),
        reserved
    );
    let command = GitCommand {
        name: ref_copy.destination.clone(),
        old: None,
        new: Some(serde_json::from_value(base.clone()).unwrap()),
        force: false,
    };
    assert_eq!(
        policy
            .pre_receive(&grant.context, &new, std::slice::from_ref(&command))
            .await
            .unwrap(),
        vec![None]
    );
    clock.0.store(NOW + 121000, Ordering::SeqCst);
    assert!(
        policy
            .pre_receive(&grant.context, &new, std::slice::from_ref(&command))
            .await
            .is_err()
    );
    clock.0.store(NOW + 1000, Ordering::SeqCst);
    copies.finished_copy(&internal).await.unwrap();
    assert!(
        policy
            .pre_receive(&grant.context, &new, &[command])
            .await
            .is_err()
    );
    // Failure before the destination exists must revoke the newly minted token too.
    assert!(
        backend
            .copy_refs(&old, &new, &[ref_copy], true)
            .await
            .is_err()
    );
    for _ in 0..8 {
        app.run_lifecycle_once().await.unwrap();
    }
    assert!(
        store
            .running_compaction(&principal.tenant_id, &repository)
            .await
            .unwrap()
            .is_none()
    );
    let secrets = copies.secrets.lock().unwrap().clone();
    assert!(secrets.len() >= 3);
    for internal in &secrets {
        assert!(
            store
                .authenticate_copy_token(copy_token_hash(internal).unwrap(), NOW + 1000)
                .await
                .is_err()
        );
    }
    assert!(
        backend
            .list_refs(&new, &[])
            .await
            .unwrap()
            .refs
            .iter()
            .any(|r| r.name == "refs/heads/main" && r.object_id.as_str() == base.as_str().unwrap())
    );
    let started = std::time::Instant::now();
    while !server.layer_files(old.as_str()).is_empty()
        && started.elapsed() < std::time::Duration::from_secs(10)
    {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        server.layer_files(old.as_str()).is_empty(),
        "old layer files were not reclaimed after sealed deletion"
    );
    assert!(
        !server.layer_files(new.as_str()).is_empty(),
        "live generation data must remain stored"
    );
    println!(
        "sealed old-generation layer files reclaimed within {:?}",
        started.elapsed()
    );
    hooks.abort();
    public.abort();
}

fn faults(backend: cambium_backend_enroute::EnrouteBackend) -> Arc<FaultBackend> {
    Arc::new(FaultBackend {
        backend,
        list_refs_calls: AtomicUsize::new(0),
        lose_next_response: AtomicBool::new(false),
        lose_creation_response: AtomicBool::new(false),
        race_creation: AtomicBool::new(false),
        lose_seal_response: AtomicBool::new(false),
        fail_delete: AtomicBool::new(false),
        lose_copy_response: AtomicBool::new(false),
        lose_ref_response: AtomicBool::new(false),
        block_after_copy: AtomicBool::new(false),
        stale_cas_once: AtomicBool::new(false),
        fail_rescue_for: std::sync::Mutex::new(None),
        block_copy: AtomicBool::new(false),
        copy_admitted: Notify::new(),
        copy_release: Notify::new(),
        block_next: AtomicBool::new(false),
        admitted: Notify::new(),
        release: Notify::new(),
    })
}
fn fast_app(
    store: Arc<SqliteStore>,
    backend: Arc<FaultBackend>,
    clock: Arc<TestClock>,
    git: &str,
) -> Arc<App> {
    let mut app = App::new(store, backend, clock, &[12; 32], git.into());
    app.compaction.drain_ms = 0;
    app.compaction.settle_ms = 1;
    Arc::new(app)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn interrupted_stages_abort_before_switch_and_resume_after_it() {
    let server = common::Server::start().await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("metadata.sqlite");
    let store = Arc::new(
        SqliteStore::open(&path, TokenSettings::default())
            .await
            .unwrap(),
    );
    let clock = Arc::new(TestClock(AtomicI64::new(NOW)));
    let backend = faults(server.backend.clone());
    let mut app = fast_app(store.clone(), backend.clone(), clock.clone(), &server.git);
    let secret = new_token().unwrap();
    let principal = store
        .deployment_bootstrap(
            &Bootstrap {
                tenant_slug: "restart".into(),
                human_name: "Operator".into(),
                token_hash: token_hash(&secret).unwrap(),
                expires_at_ms: NOW + 86_400_000,
            },
            NOW,
        )
        .await
        .unwrap();
    for (index, target) in [
        "preparing",
        "prepared",
        "catching_up",
        "verifying",
        "draining",
        "sealing",
        "sealed",
        "deleting",
    ]
    .iter()
    .enumerate()
    {
        let repo = success(
            call(
                &app,
                &secret,
                "create_repository",
                json!({"name": format!("project-{index}")}),
            )
            .await,
        )["repository_id"]
            .clone();
        let repository: RepositoryId = serde_json::from_value(repo.clone()).unwrap();
        let base = success(call(&app, &secret, "write_changeset", json!({"repository_id": repo, "message": "base", "changes": [{"action": "put", "path": "base", "contents": "retained"}]})).await)["commit_id"].clone();
        success(
            call(
                &app,
                &secret,
                "request_compaction",
                json!({"repository_id": repo}),
            )
            .await,
        );
        for _ in 0..8 {
            if store
                .running_compaction(&principal.tenant_id, &repository)
                .await
                .unwrap()
                .unwrap()
                .stage
                == *target
            {
                break;
            }
            app.run_lifecycle_once().await.unwrap();
        }
        assert_eq!(
            store
                .running_compaction(&principal.tenant_id, &repository)
                .await
                .unwrap()
                .unwrap()
                .stage,
            *target
        );
        let reopened = Arc::new(
            SqliteStore::open(&path, TokenSettings::default())
                .await
                .unwrap(),
        );
        app = fast_app(reopened, backend.clone(), clock.clone(), &server.git);
        app.recover_at_startup().await.unwrap();
        for _ in 0..8 {
            if store
                .running_compaction(&principal.tenant_id, &repository)
                .await
                .unwrap()
                .is_none()
            {
                break;
            }
            app.run_lifecycle_once().await.unwrap();
        }
        let complete = store
            .latest_compaction(&principal.tenant_id, &repository)
            .await
            .unwrap()
            .unwrap();
        let preswitch = index < 4;
        assert_eq!(
            complete.stage,
            if preswitch { "aborted" } else { "complete" }
        );
        let record = store
            .get_repository(&principal.tenant_id, &repository)
            .await
            .unwrap();
        assert!(!record.fenced);
        assert_eq!(record.active_generation, if preswitch { 1 } else { 2 });
        let active: BackendRepositoryId = record.backend_id().try_into().unwrap();
        let refs = backend.list_refs(&active, &[]).await.unwrap();
        assert!(
            refs.refs
                .iter()
                .all(|r| !r.name.starts_with("refs/cambium/"))
        );
        assert!(
            refs.refs
                .iter()
                .any(|r| r.name == "refs/heads/main"
                    && r.object_id.as_str() == base.as_str().unwrap())
        );
        if preswitch {
            success(
                call(
                    &app,
                    &secret,
                    "request_compaction",
                    json!({"repository_id": repo}),
                )
                .await,
            );
            assert_eq!(
                store
                    .running_compaction(&principal.tenant_id, &repository)
                    .await
                    .unwrap()
                    .unwrap()
                    .new_generation,
                3
            );
            for _ in 0..8 {
                app.run_lifecycle_once().await.unwrap();
            }
            assert_eq!(
                store
                    .get_repository(&principal.tenant_id, &repository)
                    .await
                    .unwrap()
                    .active_generation,
                3
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn ambiguous_copies_seals_and_deletions_never_delete_an_active_or_unreconciled_generation() {
    let server = common::Server::start().await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("metadata.sqlite");
    let store = Arc::new(
        SqliteStore::open(&path, TokenSettings::default())
            .await
            .unwrap(),
    );
    let clock = Arc::new(TestClock(AtomicI64::new(NOW)));
    let backend = faults(server.backend.clone());
    let mut app = fast_app(store.clone(), backend.clone(), clock.clone(), &server.git);
    let secret = new_token().unwrap();
    let principal = store
        .deployment_bootstrap(
            &Bootstrap {
                tenant_slug: "failure".into(),
                human_name: "Operator".into(),
                token_hash: token_hash(&secret).unwrap(),
                expires_at_ms: NOW + 86_400_000,
            },
            NOW,
        )
        .await
        .unwrap();
    let repo = success(
        call(
            &app,
            &secret,
            "create_repository",
            json!({"name": "project"}),
        )
        .await,
    )["repository_id"]
        .clone();
    let repository: RepositoryId = serde_json::from_value(repo.clone()).unwrap();
    let base = success(call(&app, &secret, "write_changeset", json!({"repository_id": repo, "message": "base", "changes": [{"action": "put", "path": "base", "contents": "retained"}]})).await)["commit_id"].clone();
    let old: BackendRepositoryId = format!("{}/{}/g1", principal.tenant_id, repository)
        .try_into()
        .unwrap();
    success(
        call(
            &app,
            &secret,
            "request_compaction",
            json!({"repository_id": repo}),
        )
        .await,
    );
    backend.lose_copy_response.store(true, Ordering::SeqCst);
    assert!(app.run_lifecycle_once().await.is_err());
    assert_eq!(
        store
            .running_compaction(&principal.tenant_id, &repository)
            .await
            .unwrap()
            .unwrap()
            .stage,
        "aborting"
    );
    app.run_lifecycle_once().await.unwrap();
    assert_eq!(
        store
            .get_repository(&principal.tenant_id, &repository)
            .await
            .unwrap()
            .active_generation,
        1
    );
    assert!(backend.list_refs(&old, &[]).await.is_ok());
    success(
        call(
            &app,
            &secret,
            "request_compaction",
            json!({"repository_id": repo}),
        )
        .await,
    );
    backend.block_copy.store(true, Ordering::SeqCst);
    let worker_app = app.clone();
    let worker = tokio::spawn(async move { worker_app.run_lifecycle_once().await });
    backend.copy_admitted.notified().await;
    // Prepare must not hold the API writer lock while copying.
    let write = tokio::time::timeout(std::time::Duration::from_secs(2), call(&app, &secret, "write_changeset", json!({"repository_id": repo, "message": "during prepare", "changes": [{"action": "put", "path": "base", "contents": "updated during prepare"}]}))).await.unwrap();
    let updated = success(write)["commit_id"].clone();
    assert_ne!(updated, base);
    worker.abort();
    let _ = worker.await;
    let reopened = Arc::new(
        SqliteStore::open(&path, TokenSettings::default())
            .await
            .unwrap(),
    );
    app = fast_app(reopened, backend.clone(), clock.clone(), &server.git);
    app.recover_at_startup().await.unwrap();
    app.run_lifecycle_once().await.unwrap();
    assert_eq!(
        store
            .latest_compaction(&principal.tenant_id, &repository)
            .await
            .unwrap()
            .unwrap()
            .stage,
        "aborted"
    );
    success(
        call(
            &app,
            &secret,
            "request_compaction",
            json!({"repository_id": repo}),
        )
        .await,
    );
    for _ in 0..5 {
        app.run_lifecycle_once().await.unwrap();
    }
    assert_eq!(
        store
            .running_compaction(&principal.tenant_id, &repository)
            .await
            .unwrap()
            .unwrap()
            .stage,
        "sealing"
    );
    backend.lose_seal_response.store(true, Ordering::SeqCst);
    assert!(app.run_lifecycle_once().await.is_err());
    assert_eq!(
        store
            .running_compaction(&principal.tenant_id, &repository)
            .await
            .unwrap()
            .unwrap()
            .stage,
        "sealing"
    );
    assert!(backend.list_refs(&old, &[]).await.is_ok());
    let reopened = Arc::new(
        SqliteStore::open(&path, TokenSettings::default())
            .await
            .unwrap(),
    );
    app = fast_app(reopened, backend.clone(), clock.clone(), &server.git);
    app.recover_at_startup().await.unwrap();
    app.run_lifecycle_once().await.unwrap();
    assert_eq!(
        store
            .running_compaction(&principal.tenant_id, &repository)
            .await
            .unwrap()
            .unwrap()
            .stage,
        "sealed"
    );
    app.run_lifecycle_once().await.unwrap();
    backend.fail_delete.store(true, Ordering::SeqCst);
    assert!(app.run_lifecycle_once().await.is_err());
    assert!(backend.list_refs(&old, &[]).await.is_ok());
    app.run_lifecycle_once().await.unwrap();
    assert!(matches!(
        backend.list_refs(&old, &[]).await,
        Err(BackendError::RepositoryNotFound)
    ));
    let current = store
        .get_repository(&principal.tenant_id, &repository)
        .await
        .unwrap();
    assert_eq!(current.active_generation, 4);
    let active: BackendRepositoryId = current.backend_id().try_into().unwrap();
    assert!(
        backend
            .list_refs(&active, &[])
            .await
            .unwrap()
            .refs
            .iter()
            .any(|r| r.name == "refs/heads/main"
                && r.object_id.as_str() == updated.as_str().unwrap())
    );
}

fn git_command(directory: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Lifecycle test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .current_dir(directory)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn reconciliation_handles_stale_cas_tags_deletions_and_a_pending_rescue_after_restart() {
    let server = common::Server::start().await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("metadata.sqlite");
    let store = Arc::new(
        SqliteStore::open(&path, TokenSettings::default())
            .await
            .unwrap(),
    );
    let clock = Arc::new(TestClock(AtomicI64::new(NOW)));
    let backend = faults(server.backend.clone());
    let build = |store: Arc<SqliteStore>| {
        let mut app = App::new(
            store,
            backend.clone(),
            clock.clone(),
            &[12; 32],
            server.git.clone(),
        );
        app.compaction.settle_ms = 1;
        app.compaction.drain_ms = 1000;
        Arc::new(app)
    };
    let mut app = build(store.clone());
    let secret = new_token().unwrap();
    let principal = store
        .deployment_bootstrap(
            &Bootstrap {
                tenant_slug: "reconcile".into(),
                human_name: "Operator".into(),
                token_hash: token_hash(&secret).unwrap(),
                expires_at_ms: NOW + 86_400_000,
            },
            NOW,
        )
        .await
        .unwrap();
    let repo = success(
        call(
            &app,
            &secret,
            "create_repository",
            json!({"name": "project"}),
        )
        .await,
    )["repository_id"]
        .clone();
    let repository: RepositoryId = serde_json::from_value(repo.clone()).unwrap();
    let base: ObjectId = serde_json::from_value(success(call(&app, &secret, "write_changeset", json!({"repository_id": repo, "message": "base", "changes": [{"action": "put", "path": "base", "contents": "retained"}]})).await)["commit_id"].clone()).unwrap();
    let old: BackendRepositoryId = format!("{}/{}/g1", principal.tenant_id, repository)
        .try_into()
        .unwrap();
    let new: BackendRepositoryId = format!("{}/{}/g2", principal.tenant_id, repository)
        .try_into()
        .unwrap();
    for name in [
        "refs/heads/delete",
        "refs/heads/conflict-delete",
        "refs/heads/already",
    ] {
        backend
            .update_refs(
                &old,
                &[RefUpdate {
                    name: name.into(),
                    expected: RefExpectation::Absent,
                    new: Some(base.clone()),
                }],
            )
            .await
            .unwrap();
    }
    let checkout = directory.path().join("checkout");
    let old_url = format!("{}{}.git", server.git, old.as_str());
    let new_url = format!("{}{}.git", server.git, new.as_str());
    git_command(
        directory.path(),
        &["clone", &old_url, checkout.to_str().unwrap()],
    );
    for name in ["one", "two", "three", "four"] {
        git_command(&checkout, &["tag", "-a", name, "-m", name]);
    }
    let tag_two: ObjectId = git_command(&checkout, &["rev-parse", "refs/tags/two"])
        .parse()
        .unwrap();
    let tag_four: ObjectId = git_command(&checkout, &["rev-parse", "refs/tags/four"])
        .parse()
        .unwrap();
    git_command(
        &checkout,
        &["push", "origin", "refs/tags/one:refs/tags/late"],
    );
    success(
        call(
            &app,
            &secret,
            "request_compaction",
            json!({"repository_id": repo}),
        )
        .await,
    );
    for _ in 0..4 {
        app.run_lifecycle_once().await.unwrap();
    }
    git_command(
        &checkout,
        &["push", "--force", "origin", "refs/tags/two:refs/tags/late"],
    );
    git_command(
        &checkout,
        &[
            "push",
            "--force",
            &new_url,
            "refs/tags/three:refs/tags/late",
        ],
    );
    for name in ["refs/heads/delete", "refs/heads/conflict-delete"] {
        backend
            .update_refs(
                &old,
                &[RefUpdate {
                    name: name.into(),
                    expected: RefExpectation::At(base.clone()),
                    new: None,
                }],
            )
            .await
            .unwrap();
    }
    success(call(&app, &secret, "write_changeset", json!({"repository_id": repo, "branch": "conflict-delete", "message": "keep deletion conflict", "changes": [{"action": "put", "path": "own", "contents": "keep"}]})).await);
    let identity = GitIdentity {
        name: "Late".into(),
        email: "test@example.invalid".into(),
        timestamp_seconds: NOW / 1000,
        utc_offset_seconds: 0,
    };
    let request = CommitRequest {
        branch: "refs/heads/main".into(),
        expected_parent: Some(base),
        message: "late main".into(),
        author: identity.clone(),
        committer: identity,
        changes: vec![FileChange::Put {
            path: b"late".to_vec(),
            contents: b"late main value".to_vec(),
            mode: FileMode::Regular,
        }],
    };
    let late = backend
        .create_commit(&old, &request)
        .await
        .unwrap()
        .commit_id;
    backend
        .update_refs(
            &old,
            &[RefUpdate {
                name: "refs/heads/already".into(),
                expected: RefExpectation::Any,
                new: Some(late.clone()),
            }],
        )
        .await
        .unwrap();
    backend
        .copy_refs(
            &old,
            &new,
            &[RefCopy {
                source: "refs/heads/already".into(),
                destination: "refs/heads/already".into(),
                allow_non_fast_forward: true,
            }],
            true,
        )
        .await
        .unwrap();
    let descendant = success(call(&app, &secret, "write_changeset", json!({"repository_id": repo, "branch": "already", "message": "descendant of late", "changes": [{"action": "put", "path": "child", "contents": "keep"}]})).await)["commit_id"].clone();
    backend.stale_cas_once.store(true, Ordering::SeqCst);
    *backend.fail_rescue_for.lock().unwrap() = Some(tag_two.clone());
    clock.0.store(NOW + 10, Ordering::SeqCst);
    assert!(app.run_lifecycle_once().await.is_err());
    let pending = store
        .running_compaction(&principal.tenant_id, &repository)
        .await
        .unwrap()
        .unwrap();
    assert!(
        pending.state["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["old_value"] == json!(tag_two))
    );
    git_command(
        &checkout,
        &["push", "--force", "origin", "refs/tags/four:refs/tags/late"],
    );
    app = build(Arc::new(
        SqliteStore::open(&path, TokenSettings::default())
            .await
            .unwrap(),
    ));
    app.recover_at_startup().await.unwrap();
    app.run_lifecycle_once().await.unwrap();
    let actual = backend.list_refs(&new, &[]).await.unwrap();
    assert!(!actual.refs.iter().any(|r| r.name == "refs/heads/delete"));
    assert!(
        actual
            .refs
            .iter()
            .any(|r| r.name == "refs/heads/conflict-delete")
    );
    assert_eq!(
        actual
            .refs
            .iter()
            .find(|r| r.name == "refs/heads/already")
            .unwrap()
            .object_id
            .as_str(),
        descendant.as_str().unwrap()
    );
    let rescues: Vec<_> = actual
        .refs
        .iter()
        .filter(|r| r.name.starts_with("refs/cambium-rescue/"))
        .collect();
    assert_eq!(rescues.len(), 3);
    for oid in [&late, &tag_two, &tag_four] {
        assert!(rescues.iter().any(|r| &r.object_id == oid));
    }
    assert_eq!(
        backend.read_object(&new, &tag_two).await.unwrap().kind,
        ObjectKind::Tag
    );
    assert_eq!(
        success(
            call(
                &app,
                &secret,
                "read_file",
                json!({"repository_id": repo, "path": "racing"})
            )
            .await
        )["contents"],
        "keep concurrent writer"
    );
    clock.0.store(NOW + 2000, Ordering::SeqCst);
    for _ in 0..4 {
        app.run_lifecycle_once().await.unwrap();
    }
    assert!(matches!(
        backend.list_refs(&old, &[]).await,
        Err(BackendError::RepositoryNotFound)
    ));
    let status = success(
        call(
            &app,
            &secret,
            "get_reclamation",
            json!({"repository_id": repo}),
        )
        .await,
    );
    assert_eq!(status["compaction"]["conflict_count"], 4);
    let mut cursor = None;
    let mut conflicts = Vec::new();
    loop {
        let mut input = json!({"repository_id": repo, "limit": 1});
        if let Some(token) = cursor {
            input["page_token"] = json!(token);
        }
        let page = success(call(&app, &secret, "get_reclamation", input).await);
        conflicts.extend(page["compaction"]["conflicts"].as_array().unwrap().clone());
        cursor = page["compaction"]
            .get("next_page_token")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(conflicts.len(), 4);
    assert!(
        backend
            .list_refs(&new, &[])
            .await
            .unwrap()
            .refs
            .iter()
            .all(|r| !r.name.starts_with("refs/cambium/"))
    );
}

#[tokio::test]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn an_all_ephemeral_repository_compacts_to_an_empty_generation() {
    let server = common::Server::start().await;
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteStore::open(
            directory.path().join("metadata.sqlite"),
            TokenSettings::default(),
        )
        .await
        .unwrap(),
    );
    let clock = Arc::new(TestClock(AtomicI64::new(NOW)));
    let backend = faults(server.backend.clone());
    let mut application = App::new(
        store.clone(),
        backend.clone(),
        clock.clone(),
        &[11; 32],
        server.git.clone(),
    );
    application.compaction.max_expired = 1;
    application.compaction.drain_ms = 0;
    let app = Arc::new(application);
    let secret = new_token().unwrap();
    let principal = store
        .deployment_bootstrap(
            &Bootstrap {
                tenant_slug: "empty".into(),
                human_name: "Operator".into(),
                token_hash: token_hash(&secret).unwrap(),
                expires_at_ms: NOW + 86_400_000,
            },
            NOW,
        )
        .await
        .unwrap();
    let repo = success(
        call(
            &app,
            &secret,
            "create_repository",
            json!({"name": "project"}),
        )
        .await,
    )["repository_id"]
        .clone();
    let repository: RepositoryId = serde_json::from_value(repo.clone()).unwrap();
    let seed = success(call(&app, &secret, "write_changeset", json!({"repository_id": repo, "branch": "seed", "message": "seed", "changes": [{"action": "put", "path": "seed", "contents": "discard everything"}]})).await)["commit_id"].clone();
    success(
        call(
            &app,
            &secret,
            "create_ephemeral_branch",
            json!({"repository_id": repo, "name": "main", "start_commit": seed, "ttl_seconds": 1}),
        )
        .await,
    );
    let old: BackendRepositoryId = format!("{}/{}/g1", principal.tenant_id, repository)
        .try_into()
        .unwrap();
    backend
        .update_refs(
            &old,
            &[RefUpdate {
                name: "refs/heads/seed".into(),
                expected: RefExpectation::Any,
                new: None,
            }],
        )
        .await
        .unwrap();
    clock.0.store(NOW + 1000, Ordering::SeqCst);
    // The expiry threshold itself schedules compaction; no operator request.
    for _ in 0..8 {
        app.run_lifecycle_once().await.unwrap();
    }
    let record = store
        .get_repository(&principal.tenant_id, &repository)
        .await
        .unwrap();
    assert_eq!(record.active_generation, 2);
    let active: BackendRepositoryId = record.backend_id().try_into().unwrap();
    let refs = backend.list_refs(&active, &[]).await.unwrap();
    assert_eq!(refs.head, "refs/heads/main");
    assert!(refs.refs.is_empty());
    assert!(matches!(
        backend
            .read_object(&active, &serde_json::from_value(seed).unwrap())
            .await,
        Err(BackendError::ObjectNotFound)
    ));
    assert_eq!(
        store
            .expiry_summary(&principal.tenant_id, &repository)
            .await
            .unwrap()
            .count,
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn failed_verification_aborts_and_repository_deletion_waits_for_copy() {
    let server = common::Server::start().await;
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteStore::open(
            directory.path().join("metadata.sqlite"),
            TokenSettings::default(),
        )
        .await
        .unwrap(),
    );
    let clock = Arc::new(TestClock(AtomicI64::new(NOW)));
    let backend = faults(server.backend.clone());
    let app = fast_app(store.clone(), backend.clone(), clock, &server.git);
    let secret = new_token().unwrap();
    let principal = store
        .deployment_bootstrap(
            &Bootstrap {
                tenant_slug: "verification".into(),
                human_name: "Operator".into(),
                token_hash: token_hash(&secret).unwrap(),
                expires_at_ms: NOW + 86_400_000,
            },
            NOW,
        )
        .await
        .unwrap();
    let repo = success(
        call(
            &app,
            &secret,
            "create_repository",
            json!({"name": "project"}),
        )
        .await,
    )["repository_id"]
        .clone();
    let repository: RepositoryId = serde_json::from_value(repo.clone()).unwrap();
    let base = success(call(&app, &secret, "write_changeset", json!({"repository_id": repo, "message": "base", "changes": [{"action": "put", "path": "base", "contents": "retained"}]})).await)["commit_id"].clone();
    success(
        call(
            &app,
            &secret,
            "request_compaction",
            json!({"repository_id": repo}),
        )
        .await,
    );
    for _ in 0..3 {
        app.run_lifecycle_once().await.unwrap();
    }
    assert_eq!(
        store
            .running_compaction(&principal.tenant_id, &repository)
            .await
            .unwrap()
            .unwrap()
            .stage,
        "verifying"
    );
    let destination: BackendRepositoryId = format!("{}/{}/g2", principal.tenant_id, repository)
        .try_into()
        .unwrap();
    backend
        .update_refs(
            &destination,
            &[RefUpdate {
                name: "refs/heads/main".into(),
                expected: RefExpectation::Any,
                new: None,
            }],
        )
        .await
        .unwrap();
    assert!(app.run_lifecycle_once().await.is_err());
    app.run_lifecycle_once().await.unwrap();
    let current = store
        .get_repository(&principal.tenant_id, &repository)
        .await
        .unwrap();
    assert_eq!(current.active_generation, 1);
    assert!(!current.fenced);
    let old: BackendRepositoryId = current.backend_id().try_into().unwrap();
    assert!(
        backend
            .list_refs(&old, &[])
            .await
            .unwrap()
            .refs
            .iter()
            .any(|r| r.object_id.as_str() == base.as_str().unwrap())
    );
    assert!(matches!(
        backend.list_refs(&destination, &[]).await,
        Err(BackendError::RepositoryNotFound)
    ));

    success(
        call(
            &app,
            &secret,
            "request_compaction",
            json!({"repository_id": repo}),
        )
        .await,
    );
    backend.block_copy.store(true, Ordering::SeqCst);
    let worker_app = app.clone();
    let worker = tokio::spawn(async move { worker_app.run_lifecycle_once().await });
    backend.copy_admitted.notified().await;
    let delete_app = app.clone();
    let mut deletion = tokio::spawn(async move {
        call(
            &delete_app,
            &secret,
            "delete_repository",
            json!({"repository_id": repo}),
        )
        .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut deletion)
            .await
            .is_err()
    );
    backend.copy_release.notify_one();
    worker.await.unwrap().unwrap();
    success(deletion.await.unwrap());
    assert!(
        store
            .running_compaction(&principal.tenant_id, &repository)
            .await
            .unwrap()
            .is_none()
    );
    for generation in 1..=3 {
        let id: BackendRepositoryId =
            format!("{}/{}/g{generation}", principal.tenant_id, repository)
                .try_into()
                .unwrap();
        assert!(matches!(
            backend.list_refs(&id, &[]).await,
            Err(BackendError::RepositoryNotFound)
        ));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn orphan_sweeps_are_bounded_and_still_remove_late_temporary_refs() {
    let server = common::Server::start().await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("metadata.sqlite");
    let store = Arc::new(
        SqliteStore::open(&path, TokenSettings::default())
            .await
            .unwrap(),
    );
    let clock = Arc::new(TestClock(AtomicI64::new(NOW)));
    let backend = faults(server.backend.clone());
    let mut app = fast_app(store.clone(), backend.clone(), clock.clone(), &server.git);
    let secret = new_token().unwrap();
    let principal = store
        .deployment_bootstrap(
            &Bootstrap {
                tenant_slug: "orphan-sweep".into(),
                human_name: "Operator".into(),
                token_hash: token_hash(&secret).unwrap(),
                expires_at_ms: NOW + 86_400_000,
            },
            NOW,
        )
        .await
        .unwrap();
    let repo = success(
        call(
            &app,
            &secret,
            "create_repository",
            json!({"name": "project"}),
        )
        .await,
    )["repository_id"]
        .clone();
    let repository: RepositoryId = serde_json::from_value(repo.clone()).unwrap();
    let base = success(call(&app, &secret, "write_changeset", json!({"repository_id": repo, "message": "base", "changes": [{"action": "put", "path": "base", "contents": "retained"}]})).await)["commit_id"].clone();
    let base: ObjectId = serde_json::from_value(base).unwrap();
    let mut names = Vec::new();
    for _ in 0..2 {
        let record = success(
            call(
                &app,
                &secret,
                "request_compaction",
                json!({"repository_id": repo}),
            )
            .await,
        );
        names.push(format!(
            "refs/cambium/{}/copy/late",
            record["compaction_id"].as_str().unwrap()
        ));
        for _ in 0..8 {
            app.run_lifecycle_once().await.unwrap();
        }
    }
    let active: BackendRepositoryId = store
        .get_repository(&principal.tenant_id, &repository)
        .await
        .unwrap()
        .backend_id()
        .try_into()
        .unwrap();
    let updates = names
        .iter()
        .map(|name| RefUpdate {
            name: name.clone(),
            expected: RefExpectation::Absent,
            new: Some(base.clone()),
        })
        .collect::<Vec<_>>();
    backend.update_refs(&active, &updates).await.unwrap();
    let reopened = Arc::new(
        SqliteStore::open(&path, TokenSettings::default())
            .await
            .unwrap(),
    );
    app = fast_app(reopened, backend.clone(), clock.clone(), &server.git);
    app.recover_at_startup().await.unwrap();
    backend.list_refs_calls.store(0, Ordering::SeqCst);
    for _ in 0..10 {
        app.run_lifecycle_once().await.unwrap();
    }
    assert_eq!(backend.list_refs_calls.load(Ordering::SeqCst), 0);
    clock.0.store(NOW + 60_000, Ordering::SeqCst);
    app.run_lifecycle_once().await.unwrap();
    assert_eq!(backend.list_refs_calls.load(Ordering::SeqCst), 1);
    assert!(
        server
            .backend
            .list_refs(&active, &["refs/cambium/".into()])
            .await
            .unwrap()
            .refs
            .is_empty()
    );
    // An old, already-admitted RPC can still land after a successful sweep.
    backend.update_refs(&active, &updates[..1]).await.unwrap();
    clock.0.store(NOW + 120_000, Ordering::SeqCst);
    app.run_lifecycle_once().await.unwrap();
    assert_eq!(backend.list_refs_calls.load(Ordering::SeqCst), 2);
    assert!(
        server
            .backend
            .list_refs(&active, &["refs/cambium/".into()])
            .await
            .unwrap()
            .refs
            .is_empty()
    );
}

struct ReconciliationFixture {
    server: common::Server,
    directory: tempfile::TempDir,
    store: Arc<SqliteStore>,
    clock: Arc<TestClock>,
    backend: Arc<FaultBackend>,
    app: Arc<App>,
    token: String,
    repo: Value,
    old: BackendRepositoryId,
    new: BackendRepositoryId,
    base: ObjectId,
}

impl ReconciliationFixture {
    async fn start() -> Self {
        let server = common::Server::start().await;
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SqliteStore::open(
                directory.path().join("metadata.sqlite"),
                TokenSettings::default(),
            )
            .await
            .unwrap(),
        );
        let clock = Arc::new(TestClock(AtomicI64::new(NOW)));
        let backend = faults(server.backend.clone());
        let mut app = App::new(
            store.clone(),
            backend.clone(),
            clock.clone(),
            &[12; 32],
            server.git.clone(),
        );
        app.compaction.drain_ms = 1000;
        app.compaction.settle_ms = 1;
        let app = Arc::new(app);
        let token = new_token().unwrap();
        let principal = store
            .deployment_bootstrap(
                &Bootstrap {
                    tenant_slug: "reconcile-regression".into(),
                    human_name: "Operator".into(),
                    token_hash: token_hash(&token).unwrap(),
                    expires_at_ms: NOW + 86_400_000,
                },
                NOW,
            )
            .await
            .unwrap();
        let repo = success(
            call(
                &app,
                &token,
                "create_repository",
                json!({"name": "project"}),
            )
            .await,
        )["repository_id"]
            .clone();
        let base: ObjectId = serde_json::from_value(success(call(&app, &token, "write_changeset", json!({"repository_id": repo, "message": "base", "changes": [{"action": "put", "path": "base", "contents": "retained"}]})).await)["commit_id"].clone()).unwrap();
        let old = format!("{}/{}/g1", principal.tenant_id, repo.as_str().unwrap())
            .try_into()
            .unwrap();
        let new = format!("{}/{}/g2", principal.tenant_id, repo.as_str().unwrap())
            .try_into()
            .unwrap();
        success(
            call(
                &app,
                &token,
                "request_compaction",
                json!({"repository_id": repo}),
            )
            .await,
        );
        for _ in 0..4 {
            app.run_lifecycle_once().await.unwrap();
        }
        Self {
            server,
            directory,
            store,
            clock,
            backend,
            app,
            token,
            repo,
            old,
            new,
            base,
        }
    }

    async fn write_old(&self, message: &str) -> ObjectId {
        let identity = GitIdentity {
            name: "Old writer".into(),
            email: "test@example.invalid".into(),
            timestamp_seconds: NOW / 1000,
            utc_offset_seconds: 0,
        };
        self.server
            .backend
            .update_refs(
                &self.old,
                &[RefUpdate {
                    name: "refs/heads/main".into(),
                    expected: RefExpectation::Any,
                    new: Some(self.base.clone()),
                }],
            )
            .await
            .unwrap();
        self.server
            .backend
            .create_commit(
                &self.old,
                &CommitRequest {
                    branch: "refs/heads/main".into(),
                    expected_parent: Some(self.base.clone()),
                    message: message.into(),
                    author: identity.clone(),
                    committer: identity,
                    changes: vec![FileChange::Put {
                        path: b"late".to_vec(),
                        contents: message.as_bytes().to_vec(),
                        mode: FileMode::Regular,
                    }],
                },
            )
            .await
            .unwrap()
            .commit_id
    }

    async fn restart(&mut self) {
        self.store = Arc::new(
            SqliteStore::open(
                self.directory.path().join("metadata.sqlite"),
                TokenSettings::default(),
            )
            .await
            .unwrap(),
        );
        let mut app = App::new(
            self.store.clone(),
            self.backend.clone(),
            self.clock.clone(),
            &[12; 32],
            self.server.git.clone(),
        );
        app.compaction.drain_ms = 1000;
        app.compaction.settle_ms = 1;
        self.app = Arc::new(app);
        self.app.recover_at_startup().await.unwrap();
    }

    async fn finish(&self) {
        self.clock.0.store(NOW + 2000, Ordering::SeqCst);
        for _ in 0..4 {
            self.app.run_lifecycle_once().await.unwrap();
        }
        assert!(matches!(
            self.server.backend.list_refs(&self.old, &[]).await,
            Err(BackendError::RepositoryNotFound)
        ));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn reconciliation_regression_restores_snapshot_and_removes_late_creation() {
    for lose_reply in [false, true] {
        let mut fixture = ReconciliationFixture::start().await;
        let late = fixture.write_old("first late value").await;
        fixture
            .server
            .backend
            .update_refs(
                &fixture.old,
                &[RefUpdate {
                    name: "refs/heads/created-late".into(),
                    expected: RefExpectation::Absent,
                    new: Some(fixture.base.clone()),
                }],
            )
            .await
            .unwrap();
        fixture
            .backend
            .lose_ref_response
            .store(lose_reply, Ordering::SeqCst);
        fixture.clock.0.store(NOW + 10, Ordering::SeqCst);
        let result = fixture.app.run_lifecycle_once().await;
        assert_eq!(result.is_err(), lose_reply);
        let refs = fixture
            .server
            .backend
            .list_refs(&fixture.new, &[])
            .await
            .unwrap();
        assert!(
            refs.refs
                .iter()
                .any(|r| r.name == "refs/heads/main" && r.object_id == late)
        );
        fixture
            .server
            .backend
            .update_refs(
                &fixture.old,
                &[
                    RefUpdate {
                        name: "refs/heads/main".into(),
                        expected: RefExpectation::At(late),
                        new: Some(fixture.base.clone()),
                    },
                    RefUpdate {
                        name: "refs/heads/created-late".into(),
                        expected: RefExpectation::Any,
                        new: None,
                    },
                ],
            )
            .await
            .unwrap();
        fixture.restart().await;
        fixture.finish().await;
        let refs = fixture
            .server
            .backend
            .list_refs(&fixture.new, &[])
            .await
            .unwrap();
        assert_eq!(
            refs.refs
                .iter()
                .find(|r| r.name == "refs/heads/main")
                .unwrap()
                .object_id,
            fixture.base
        );
        assert!(
            !refs
                .refs
                .iter()
                .any(|r| r.name == "refs/heads/created-late")
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn reconciliation_regression_keeps_conflict_when_copy_finishes_before_process_loss() {
    let mut fixture = ReconciliationFixture::start().await;
    let active = success(call(&fixture.app, &fixture.token, "write_changeset", json!({"repository_id": fixture.repo, "message": "independent active", "changes": [{"action": "put", "path": "active", "contents": "retained"}]})).await)["commit_id"].clone();
    let first = fixture.write_old("first conflicting value").await;
    fixture
        .backend
        .block_after_copy
        .store(true, Ordering::SeqCst);
    fixture.clock.0.store(NOW + 10, Ordering::SeqCst);
    let worker_app = fixture.app.clone();
    let worker = tokio::spawn(async move { worker_app.run_lifecycle_once().await });
    fixture.backend.copy_admitted.notified().await;
    let second = fixture.write_old("second conflicting value").await;
    worker.abort();
    let _ = worker.await;
    fixture.restart().await;
    fixture.finish().await;
    let refs = fixture
        .server
        .backend
        .list_refs(&fixture.new, &[])
        .await
        .unwrap();
    for oid in [first, second] {
        assert!(
            refs.refs
                .iter()
                .any(|r| r.name.starts_with("refs/cambium-rescue/") && r.object_id == oid),
            "missing rescue for {oid}"
        );
    }
    assert_eq!(
        refs.refs
            .iter()
            .find(|r| r.name == "refs/heads/main")
            .unwrap()
            .object_id
            .as_str(),
        active.as_str().unwrap()
    );
}
