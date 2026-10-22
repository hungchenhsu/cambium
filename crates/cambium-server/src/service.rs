//! The single authorization and operation path used by HTTP and MCP.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use cambium_backend::{
    Backend, BackendError, BackendRepositoryId, CommitRequest, FileChange, FileMode, GitIdentity,
};
use cambium_protocol::{
    ApiError, ErrorCode, ObjectId, Principal, RepositoryId, RequestId, Scope, TenantId,
    bytes::{decode_pair, encode_pair},
};
use cambium_store::{
    IdentityRecord, RepositoryRecord, StoreError, TokenHash,
    operations::{Claim, Completion, OperationKey, OperationsStore},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::{
    objects::{GitObjects, TreeEntry, collisions, validate_path},
    requests::{self, Operation},
    secrets::{ResponseCipher, new_token, token_hash},
};

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}
pub struct SystemClock;
impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
    }
}

type RepositoryLocks = HashMap<(TenantId, RepositoryId), Weak<AsyncMutex<()>>>;

pub struct App {
    pub store: Arc<dyn OperationsStore>,
    pub backend: Arc<dyn Backend>,
    pub clock: Arc<dyn Clock>,
    cipher: ResponseCipher,
    git_base_url: String,
    locks: Mutex<RepositoryLocks>,
}

#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
    pub request_id: RequestId,
    pub replayed: bool,
}

impl Reply {
    pub fn error(error: ApiError, request_id: RequestId) -> Self {
        Self {
            status: error.code.status(),
            body: json!(error.response(request_id.clone())),
            request_id,
            replayed: false,
        }
    }
}

struct Reservation<'a> {
    principal: &'a Principal,
    key: &'a OperationKey,
    run_id: &'a RequestId,
    saved: Option<Value>,
}

impl App {
    pub fn new(
        store: Arc<dyn OperationsStore>,
        backend: Arc<dyn Backend>,
        clock: Arc<dyn Clock>,
        response_key: &[u8; 32],
        git_base_url: String,
    ) -> Self {
        Self {
            store,
            backend,
            clock,
            cipher: ResponseCipher::new(response_key),
            git_base_url: git_base_url.trim_end_matches('/').to_owned(),
            locks: Mutex::new(HashMap::new()),
        }
    }

    pub async fn authenticate(&self, token: &str) -> Result<Principal, ApiError> {
        self.store
            .deployment_authenticate(token_hash(token)?, self.clock.now_ms())
            .await
            .map_err(store_error)
    }

    /// The service owner must hold the deployment lock before calling this.
    /// Never run it concurrently with admitted operations.
    pub async fn recover_at_startup(&self) -> Result<(), ApiError> {
        self.store
            .deployment_recover_operations(self.clock.now_ms())
            .await
            .map_err(store_error)?;
        self.resume_repositories().await
    }

    pub async fn resume_repositories(&self) -> Result<(), ApiError> {
        for record in self
            .store
            .deployment_pending_repositories()
            .await
            .map_err(store_error)?
        {
            let _guard = self.repository_lock(&record.tenant_id, &record.id).await?;
            let record = self
                .store
                .get_repository_including_deleted(&record.tenant_id, &record.id)
                .await
                .map_err(store_error)?;
            match record.state.as_str() {
                "creating" => {
                    self.backend
                        .create_repository(&backend_id(&record)?, &record.default_branch)
                        .await
                        .map_err(backend_error)?;
                    self.store
                        .finish_repository_creation(
                            &record.tenant_id,
                            &record.id,
                            self.clock.now_ms(),
                        )
                        .await
                        .map_err(store_error)?;
                }
                "deleting" => {
                    self.delete_generations(&record.tenant_id, &record.id)
                        .await?
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Cancellation of a connection must not abandon an admitted mutation.
    pub async fn call(
        self: &Arc<Self>,
        token: String,
        name: String,
        input: Value,
        header_key: Option<String>,
    ) -> Reply {
        let app = self.clone();
        let request_id = RequestId::new();
        let task_id = request_id.clone();
        match tokio::spawn(async move {
            app.call_inner(&token, &name, input, header_key, task_id)
                .await
        })
        .await
        {
            Ok(reply) => reply,
            Err(_) => Reply::error(internal(), request_id),
        }
    }

    async fn call_inner(
        &self,
        token: &str,
        name: &str,
        mut input: Value,
        header_key: Option<String>,
        request_id: RequestId,
    ) -> Reply {
        let result = async {
            let principal = self.authenticate(token).await?;
            let object = input
                .as_object_mut()
                .ok_or_else(|| ApiError::invalid("body", "Expected an object"))?;
            let body_key = object
                .remove("idempotency_key")
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| ApiError::invalid("idempotency_key", "Expected a string"))
                })
                .transpose()?;
            if header_key.is_some() && body_key.is_some() && header_key != body_key {
                return Err(ApiError::invalid(
                    "idempotency_key",
                    "Header and body keys must match",
                ));
            }
            let operation: Operation =
                serde_json::from_value(json!({"operation": name, "input": input}))
                    .map_err(|e| ApiError::invalid("body", &e.to_string()))?;
            self.authorize(&principal, &operation).await?;
            if !operation.mutating() {
                let body = self.execute(&principal, &operation, None).await?;
                return Ok(Reply {
                    status: 200,
                    body,
                    request_id: request_id.clone(),
                    replayed: false,
                });
            }
            let key = OperationKey {
                key: header_key
                    .or(body_key)
                    .unwrap_or_else(|| request_id.to_string()),
                fingerprint: Sha256::digest(
                    serde_json::to_vec(&("POST", format!("/v1/operations/{name}"), &input))
                        .map_err(|_| internal())?,
                )
                .into(),
            };
            match self
                .store
                .claim_operation(&principal, &key, self.clock.now_ms())
                .await
                .map_err(store_error)?
            {
                Claim::Replay {
                    status,
                    encrypted_response,
                } => {
                    let body =
                        self.cipher
                            .decrypt(&principal, &key, status, &encrypted_response)?;
                    Ok(Reply {
                        status,
                        body,
                        request_id: request_id.clone(),
                        replayed: true,
                    })
                }
                Claim::Run {
                    run_id,
                    reservation,
                } => {
                    let reservation = Reservation {
                        principal: &principal,
                        key: &key,
                        run_id: &run_id,
                        saved: reservation,
                    };
                    let outcome = self
                        .execute(&principal, &operation, Some(reservation))
                        .await;
                    let cacheable = outcome
                        .as_ref()
                        .map_or_else(|e| e.code.cacheable(), |_| true);
                    let mut reply = match outcome {
                        Ok(body) => Reply {
                            status: 200,
                            body,
                            request_id: request_id.clone(),
                            replayed: false,
                        },
                        Err(error) => Reply::error(error, request_id.clone()),
                    };
                    let completion = if cacheable {
                        match self
                            .cipher
                            .encrypt(&principal, &key, reply.status, &reply.body)
                        {
                            Ok(encrypted_response) => Some(Completion {
                                status: reply.status,
                                encrypted_response,
                            }),
                            Err(error) => {
                                reply = Reply::error(error, request_id.clone());
                                None
                            }
                        }
                    } else {
                        None
                    };
                    loop {
                        let saved = self
                            .store
                            .finish_operation(
                                &principal,
                                &key,
                                &run_id,
                                completion.as_ref(),
                                self.clock.now_ms(),
                            )
                            .await;
                        match saved {
                            Ok(()) => break,
                            // Keep the admitted task alive through a temporary database outage.
                            // A process crash is handled by startup recovery instead.
                            Err(StoreError::Database(_)) => {
                                tokio::time::sleep(std::time::Duration::from_secs(1)).await
                            }
                            Err(error) => return Err(store_error(error)),
                        }
                    }
                    Ok(reply)
                }
            }
        }
        .await;
        result.unwrap_or_else(|error| Reply::error(error, request_id))
    }

    pub async fn repository_lock(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
    ) -> Result<OwnedMutexGuard<()>, ApiError> {
        let lock = {
            let mut locks = self.locks.lock().map_err(|_| internal())?;
            locks.retain(|_, value| value.strong_count() > 0);
            let entry = locks
                .entry((tenant.clone(), repository.clone()))
                .or_default();
            let lock = entry
                .upgrade()
                .unwrap_or_else(|| Arc::new(AsyncMutex::new(())));
            *entry = Arc::downgrade(&lock);
            lock
        };
        Ok(lock.lock_owned().await)
    }

    pub async fn authorized_repository(
        &self,
        principal: &Principal,
        id: &RepositoryId,
        scope: Scope,
    ) -> Result<RepositoryRecord, ApiError> {
        if !principal.can_see(id) {
            return Err(not_found());
        }
        let repository = self
            .store
            .get_repository(&principal.tenant_id, id)
            .await
            .map_err(store_error)?;
        principal.require(scope)?;
        Ok(repository)
    }

    async fn authorize(
        &self,
        principal: &Principal,
        operation: &Operation,
    ) -> Result<(), ApiError> {
        if let Some(id) = operation.repository() {
            if matches!(operation, Operation::DeleteRepository(_)) {
                if !principal.can_see(id) {
                    return Err(not_found());
                }
                self.store
                    .get_repository_including_deleted(&principal.tenant_id, id)
                    .await
                    .map_err(store_error)?;
                principal.require(operation.scope())?;
            } else {
                self.authorized_repository(principal, id, operation.scope())
                    .await?;
            }
        } else {
            match operation {
                Operation::MintToken(input) => {
                    let request = token_request(input, TokenHash([0; 32]), self.clock.now_ms())?;
                    self.store
                        .check_mint_token(principal, &request, self.clock.now_ms())
                        .await
                        .map_err(store_error)?;
                }
                Operation::GetIdentity(input) | Operation::DisableIdentity(input) => {
                    self.store
                        .get_identity(&principal.tenant_id, &input.identity_id)
                        .await
                        .map_err(store_error)?;
                }
                Operation::RevokeToken(input) => {
                    self.store
                        .token_identity(&principal.tenant_id, &input.token_id)
                        .await
                        .map_err(store_error)?;
                }
                Operation::CreateRepository(_) if principal.repositories.is_some() => {
                    return Err(ApiError::new(ErrorCode::PermissionDenied));
                }
                _ => {}
            }
            principal.require(operation.scope())?;
        }
        Ok(())
    }

    async fn save(&self, reservation: &Reservation<'_>, value: &Value) -> Result<(), ApiError> {
        self.store
            .reserve_operation(
                reservation.principal,
                reservation.key,
                reservation.run_id,
                value,
                self.clock.now_ms(),
            )
            .await
            .map_err(store_error)
    }

    async fn execute(
        &self,
        principal: &Principal,
        operation: &Operation,
        mut reservation: Option<Reservation<'_>>,
    ) -> Result<Value, ApiError> {
        let _guard = if operation.mutating() {
            if let Some(id) = operation.repository() {
                Some(self.repository_lock(&principal.tenant_id, id).await?)
            } else {
                None
            }
        } else {
            None
        };
        let principal = self
            .store
            .revalidate(
                &principal.tenant_id,
                &principal.token_id,
                self.clock.now_ms(),
            )
            .await
            .map_err(store_error)?;
        self.authorize(&principal, operation).await?;
        match operation {
            Operation::CreateRepository(input) => {
                let default_branch = validate_branch(&input.default_branch)?;
                let reservation = reservation.as_mut().ok_or_else(internal)?;
                let record = match &reservation.saved {
                    Some(value) => {
                        let id: RepositoryId =
                            serde_json::from_value(value["repository_id"].clone())
                                .map_err(|_| internal())?;
                        self.store
                            .get_repository(&principal.tenant_id, &id)
                            .await
                            .map_err(store_error)?
                    }
                    None => {
                        let record = self
                            .store
                            .reserve_repository(
                                &principal,
                                &input.name,
                                &default_branch,
                                self.clock.now_ms(),
                            )
                            .await
                            .map_err(store_error)?;
                        self.save(reservation, &json!({"repository_id": record.id}))
                            .await?;
                        record
                    }
                };
                let _guard = self
                    .repository_lock(&principal.tenant_id, &record.id)
                    .await?;
                let record = self
                    .store
                    .get_repository(&principal.tenant_id, &record.id)
                    .await
                    .map_err(store_error)?;
                if record.state == "deleting" {
                    return Err(ApiError::new(ErrorCode::RepositoryBusy));
                }
                self.backend
                    .create_repository(&backend_id(&record)?, &record.default_branch)
                    .await
                    .map_err(backend_error)?;
                self.store
                    .finish_repository_creation(
                        &principal.tenant_id,
                        &record.id,
                        self.clock.now_ms(),
                    )
                    .await
                    .map_err(store_error)?;
                self.repository_json(
                    &self
                        .store
                        .get_repository(&principal.tenant_id, &record.id)
                        .await
                        .map_err(store_error)?,
                )
                .await
            }
            Operation::GetRepository(input) => {
                let repository = self
                    .authorized_repository(&principal, &input.repository_id, Scope::RepoRead)
                    .await?;
                self.repository_json(&repository).await
            }
            Operation::ListRepositories(input) => self.list_repositories(&principal, input).await,
            Operation::DeleteRepository(input) => {
                let current = self
                    .store
                    .get_repository_including_deleted(&principal.tenant_id, &input.repository_id)
                    .await
                    .map_err(store_error)?;
                if current.state == "deleted" {
                    return Ok(json!({"deleted": true, "repository_id": input.repository_id}));
                }
                self.store
                    .begin_repository_deletion(
                        &principal,
                        &input.repository_id,
                        self.clock.now_ms(),
                    )
                    .await
                    .map_err(store_error)?;
                self.delete_generations(&principal.tenant_id, &input.repository_id)
                    .await?;
                Ok(json!({"deleted": true, "repository_id": input.repository_id}))
            }
            Operation::WriteChangeset(input) => {
                self.write_changeset(
                    &principal,
                    input,
                    reservation.as_ref().ok_or_else(internal)?,
                )
                .await
            }
            Operation::ReadFile(input) => {
                let repository = self
                    .readable_repository(&principal, &input.repository_id)
                    .await?;
                let backend_id = backend_id(&repository)?;
                let objects = GitObjects {
                    backend: self.backend.as_ref(),
                    repository: &backend_id,
                };
                let revision = objects
                    .resolve(&input.revision)
                    .await
                    .map_err(backend_error)?;
                let path = decode_pair(input.path.as_deref(), input.path_b64.as_deref(), "path")?;
                let (entry, contents) = objects
                    .file(&revision, &path)
                    .await
                    .map_err(backend_error)?;
                let mut result = entry_json(&entry);
                result["revision"] = json!(revision);
                add_bytes(&mut result, "contents", &contents, input.always_base64);
                Ok(result)
            }
            Operation::ListTree(input) => self.list_tree(&principal, input).await,
            Operation::Diff(input) => self.diff(&principal, input).await,
            Operation::CreateIdentity(input) => {
                let record = self
                    .store
                    .create_identity(
                        &principal,
                        &cambium_store::CreateIdentity {
                            kind: input.kind,
                            name: input.name.clone(),
                            owner_id: input.owner_id.clone(),
                        },
                        self.clock.now_ms(),
                    )
                    .await
                    .map_err(store_error)?;
                identity_json(&record)
            }
            Operation::GetIdentity(input) => identity_json(
                &self
                    .store
                    .get_identity(&principal.tenant_id, &input.identity_id)
                    .await
                    .map_err(store_error)?,
            ),
            Operation::MintToken(input) => {
                let secret = new_token()?;
                let request = token_request(input, token_hash(&secret)?, self.clock.now_ms())?;
                let minted = self
                    .store
                    .mint_token(&principal, &request, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
                let mut response = json!({"token_id": minted.token_id, "identity_id": minted.actor.identity_id, "kind": minted.actor.kind, "scope": minted.scope, "expires_at": timestamp(minted.expires_at_ms)?, "token": secret});
                if let Some(repositories) = minted.repositories {
                    response["repositories"] = json!(repositories);
                }
                Ok(response)
            }
            Operation::RevokeToken(input) => {
                self.store
                    .revoke_token(&principal, &input.token_id, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
                Ok(json!({"revoked": true, "token_id": input.token_id}))
            }
            Operation::DisableIdentity(input) => {
                self.store
                    .disable_identity(&principal, &input.identity_id, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
                Ok(json!({"disabled": true, "identity_id": input.identity_id}))
            }
        }
    }

    pub async fn delete_generations(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
    ) -> Result<(), ApiError> {
        for id in self
            .store
            .generation_ids(tenant, repository)
            .await
            .map_err(store_error)?
        {
            self.backend
                .delete_repository(&id.try_into().map_err(backend_error)?)
                .await
                .map_err(backend_error)?;
        }
        self.store
            .finish_repository_deletion(tenant, repository, self.clock.now_ms())
            .await
            .map_err(store_error)
    }

    async fn readable_repository(
        &self,
        principal: &Principal,
        id: &RepositoryId,
    ) -> Result<RepositoryRecord, ApiError> {
        let repository = self
            .authorized_repository(principal, id, Scope::RepoRead)
            .await?;
        if repository.state != "ready" {
            return Err(ApiError::new(ErrorCode::RepositoryBusy));
        }
        Ok(repository)
    }

    async fn repository_json(&self, record: &RepositoryRecord) -> Result<Value, ApiError> {
        let slug = self
            .store
            .tenant_slug(&record.tenant_id)
            .await
            .map_err(store_error)?;
        Ok(
            json!({"repository_id": record.id, "name": record.name, "default_branch": record.default_branch, "state": record.state, "created_at": timestamp(record.created_at_ms)?, "created_by": record.created_by, "git_url": format!("{}/{}/{}.git", self.git_base_url, slug, record.name)}),
        )
    }

    async fn list_repositories(
        &self,
        principal: &Principal,
        input: &requests::ListRepositories,
    ) -> Result<Value, ApiError> {
        check_limit(input.limit)?;
        let mut after = input
            .page_token
            .as_ref()
            .map(|value| decode_cursor::<RepositoryId>(value))
            .transpose()?;
        let mut items = Vec::new();
        loop {
            let rows = self
                .store
                .list_repositories(&principal.tenant_id, after.as_ref(), 1001)
                .await
                .map_err(store_error)?;
            if rows.is_empty() {
                break;
            }
            for row in &rows {
                after = Some(row.id.clone());
                if principal.can_see(&row.id) {
                    items.push(self.repository_json(row).await?);
                    if items.len() > input.limit as usize {
                        break;
                    }
                }
            }
            if items.len() > input.limit as usize || rows.len() < 1001 {
                break;
            }
        }
        let truncated = items.len() > input.limit as usize;
        items.truncate(input.limit as usize);
        let mut result = json!({"repositories": items, "truncated": truncated});
        if truncated {
            let last = result["repositories"]
                .as_array()
                .and_then(|v| v.last())
                .ok_or_else(internal)?;
            result["next_page_token"] = json!(encode_cursor(&last["repository_id"])?);
        }
        Ok(result)
    }

    async fn write_changeset(
        &self,
        principal: &Principal,
        input: &requests::WriteChangeset,
        reservation: &Reservation<'_>,
    ) -> Result<Value, ApiError> {
        let repository = self
            .readable_repository(principal, &input.repository_id)
            .await?;
        if repository.fenced {
            return Err(ApiError::new(ErrorCode::RepositoryBusy));
        }
        let backend_id = backend_id(&repository)?;
        let branch = validate_branch(&input.branch)?;
        let request: CommitRequest = if let Some(saved) = &reservation.saved {
            serde_json::from_value(saved["commit"].clone()).map_err(|_| internal())?
        } else {
            let expected_parent = match &input.expected {
                Some(requests::ExpectedTip::Absent) => None,
                Some(requests::ExpectedTip::At(id)) => Some(id.clone()),
                None => self
                    .backend
                    .list_refs(&backend_id, std::slice::from_ref(&branch))
                    .await
                    .map_err(backend_error)?
                    .refs
                    .into_iter()
                    .find(|r| r.name == branch)
                    .map(|r| r.object_id),
            };
            let identity = self
                .store
                .get_identity(&principal.tenant_id, &principal.actor.identity_id)
                .await
                .map_err(store_error)?;
            let fallback = GitIdentity {
                name: identity.name,
                email: format!("{}@cambium.invalid", principal.actor.identity_id),
                timestamp_seconds: self.clock.now_ms().div_euclid(1000),
                utc_offset_seconds: 0,
            };
            let author = input
                .author
                .as_ref()
                .map(git_identity)
                .transpose()?
                .unwrap_or_else(|| fallback.clone());
            let committer = input
                .committer
                .as_ref()
                .map(git_identity)
                .transpose()?
                .unwrap_or(fallback);
            let changes = input
                .changes
                .iter()
                .map(file_change)
                .collect::<Result<Vec<_>, _>>()?;
            if changes.is_empty() {
                return Err(ApiError::invalid(
                    "changes",
                    "At least one change is required",
                ));
            }
            let request = CommitRequest {
                branch: branch.clone(),
                expected_parent,
                message: input.message.clone(),
                author,
                committer,
                changes,
            };
            self.save(reservation, &json!({"commit": request})).await?;
            request
        };
        let result = self
            .backend
            .create_commit(&backend_id, &request)
            .await
            .map_err(|error| {
                let mut error = backend_error(error);
                if error.code == ErrorCode::RefConflict {
                    error.details.insert("ref".into(), json!(branch));
                    if let Some(expected) = &request.expected_parent {
                        error.details.insert("expected".into(), json!(expected));
                    }
                }
                error
            })?;
        let objects = GitObjects {
            backend: self.backend.as_ref(),
            repository: &backend_id,
        };
        let entries = objects
            .tree(&result.commit_id, true)
            .await
            .map_err(backend_error)?;
        let mut response = json!({"commit_id": result.commit_id, "tree_id": result.tree_id});
        if let Some(parent) = result.parent_commit_id {
            response["parent_commit_id"] = json!(parent);
        }
        add_collision_warnings(&mut response, &entries);
        Ok(response)
    }

    async fn list_tree(
        &self,
        principal: &Principal,
        input: &requests::ListTree,
    ) -> Result<Value, ApiError> {
        check_limit(input.limit)?;
        let repository = self
            .readable_repository(principal, &input.repository_id)
            .await?;
        let backend_id = backend_id(&repository)?;
        let objects = GitObjects {
            backend: self.backend.as_ref(),
            repository: &backend_id,
        };
        let binding = json!([
            principal.tenant_id,
            input.repository_id,
            input.revision,
            input.recursive
        ]);
        let (revision, offset) = match input.page_token.as_deref() {
            Some(token) => {
                let cursor: TreeCursor = decode_cursor(token)?;
                cursor.check(&binding)?;
                (cursor.first, cursor.offset)
            }
            None => (
                objects
                    .resolve(&input.revision)
                    .await
                    .map_err(backend_error)?,
                0,
            ),
        };
        let entries = objects
            .tree(&revision, input.recursive)
            .await
            .map_err(backend_error)?;
        let mut result = page(
            entries.iter().map(entry_json).collect(),
            offset,
            input.limit,
            TreeCursor {
                binding,
                first: revision.clone(),
                second: None,
                offset,
            },
        )?;
        result["revision"] = json!(revision);
        add_collision_warnings(&mut result, &entries);
        Ok(result)
    }

    async fn diff(&self, principal: &Principal, input: &requests::Diff) -> Result<Value, ApiError> {
        check_limit(input.limit)?;
        let repository = self
            .readable_repository(principal, &input.repository_id)
            .await?;
        let backend_id = backend_id(&repository)?;
        let objects = GitObjects {
            backend: self.backend.as_ref(),
            repository: &backend_id,
        };
        let binding = json!([
            principal.tenant_id,
            input.repository_id,
            input.from,
            input.to
        ]);
        let (from, to, offset) = match input.page_token.as_deref() {
            Some(token) => {
                let cursor: TreeCursor = decode_cursor(token)?;
                cursor.check(&binding)?;
                (
                    cursor.first,
                    cursor
                        .second
                        .ok_or_else(|| ApiError::invalid("page_token", "Invalid diff cursor"))?,
                    cursor.offset,
                )
            }
            None => (
                objects.resolve(&input.from).await.map_err(backend_error)?,
                objects.resolve(&input.to).await.map_err(backend_error)?,
                0,
            ),
        };
        let differences = objects.diff(&from, &to).await.map_err(backend_error)?;
        let values = differences
            .iter()
            .map(|item| {
                let mut value = json!({});
                add_bytes(&mut value, "path", &item.path, false);
                if let Some(old) = &item.old {
                    value["old"] = entry_json(old);
                }
                if let Some(new) = &item.new {
                    value["new"] = entry_json(new);
                }
                value
            })
            .collect();
        let mut result = page(
            values,
            offset,
            input.limit,
            TreeCursor {
                binding,
                first: from.clone(),
                second: Some(to.clone()),
                offset,
            },
        )?;
        result["from"] = json!(from);
        result["to"] = json!(to);
        Ok(result)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TreeCursor {
    binding: Value,
    first: ObjectId,
    second: Option<ObjectId>,
    offset: usize,
}
impl TreeCursor {
    fn check(&self, expected: &Value) -> Result<(), ApiError> {
        if &self.binding != expected {
            return Err(ApiError::invalid(
                "page_token",
                "Cursor belongs to a different request",
            ));
        }
        Ok(())
    }
}
fn page(
    values: Vec<Value>,
    offset: usize,
    limit: u32,
    mut cursor: TreeCursor,
) -> Result<Value, ApiError> {
    if offset > values.len() {
        return Err(ApiError::invalid(
            "page_token",
            "Cursor is outside the result",
        ));
    }
    let end = (offset + limit as usize).min(values.len());
    let truncated = end < values.len();
    let mut result = json!({"entries": &values[offset..end], "truncated": truncated});
    if truncated {
        cursor.offset = end;
        result["next_page_token"] = json!(encode_cursor(&cursor)?);
    }
    Ok(result)
}
fn encode_cursor<T: Serialize>(value: &T) -> Result<String, ApiError> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).map_err(|_| internal())?))
}
fn decode_cursor<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T, ApiError> {
    if value.len() > 8192 {
        return Err(ApiError::invalid("page_token", "Cursor is too long"));
    }
    URL_SAFE_NO_PAD
        .decode(value)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| ApiError::invalid("page_token", "Invalid cursor"))
}
fn check_limit(limit: u32) -> Result<(), ApiError> {
    if !(1..=1000).contains(&limit) {
        return Err(ApiError::invalid("limit", "Expected 1 to 1000"));
    }
    Ok(())
}

pub fn backend_id(repository: &RepositoryRecord) -> Result<BackendRepositoryId, ApiError> {
    repository.backend_id().try_into().map_err(backend_error)
}
pub fn validate_branch(value: &str) -> Result<String, ApiError> {
    let full = if value.starts_with("refs/heads/") {
        value.to_owned()
    } else {
        format!("refs/heads/{value}")
    };
    if value.starts_with("refs/") && !value.starts_with("refs/heads/")
        || full.contains('\u{fffd}')
        || gix_validate::reference::name(full.as_str().into()).is_err()
    {
        return Err(ApiError::invalid(
            "branch",
            "Expected a UTF-8 branch under refs/heads/",
        ));
    }
    Ok(full)
}
fn file_change(input: &requests::FileChange) -> Result<FileChange, ApiError> {
    let (path, path_b64) = match input {
        requests::FileChange::Put { path, path_b64, .. }
        | requests::FileChange::Delete { path, path_b64 } => (path, path_b64),
    };
    let path = decode_pair(path.as_deref(), path_b64.as_deref(), "path")?;
    validate_path(&path).map_err(backend_error)?;
    match input {
        requests::FileChange::Put {
            contents,
            contents_b64,
            mode,
            ..
        } => Ok(FileChange::Put {
            path,
            contents: decode_pair(contents.as_deref(), contents_b64.as_deref(), "contents")?,
            mode: mode.unwrap_or(FileMode::Regular),
        }),
        requests::FileChange::Delete { .. } => Ok(FileChange::Delete { path }),
    }
}
fn git_identity(input: &requests::GitIdentity) -> Result<GitIdentity, ApiError> {
    let parsed = OffsetDateTime::parse(&input.time, &Rfc3339)
        .map_err(|_| ApiError::invalid("time", "Expected an RFC 3339 timestamp"))?;
    if parsed.nanosecond() != 0 {
        return Err(ApiError::invalid(
            "time",
            "Git timestamps have whole-second precision",
        ));
    }
    Ok(GitIdentity {
        name: input.name.clone(),
        email: input.email.clone(),
        timestamp_seconds: parsed.unix_timestamp(),
        utc_offset_seconds: parsed.offset().whole_seconds(),
    })
}
fn token_request(
    input: &requests::MintToken,
    hash: TokenHash,
    now_ms: i64,
) -> Result<cambium_store::MintToken, ApiError> {
    let millis = i64::try_from(input.ttl_seconds)
        .ok()
        .and_then(|v| v.checked_mul(1000))
        .filter(|v| *v > 0)
        .ok_or_else(|| ApiError::invalid("ttl_seconds", "Expected a positive duration"))?;
    let expires_at_ms = now_ms
        .checked_add(millis)
        .ok_or_else(|| ApiError::invalid("ttl_seconds", "Duration is too large"))?;
    Ok(cambium_store::MintToken {
        identity_id: input.identity_id.clone(),
        token_hash: hash,
        scope: input.scope,
        repositories: input.repositories.clone(),
        expires_at_ms,
    })
}
fn identity_json(record: &IdentityRecord) -> Result<Value, ApiError> {
    let mut result = json!({"identity_id": record.id, "kind": record.kind, "name": record.name, "created_at": timestamp(record.created_at_ms)?, "created_by": record.created_by});
    if let Some(owner) = &record.owner_id {
        result["owner_id"] = json!(owner);
    }
    if let Some(disabled) = record.disabled_at_ms {
        result["disabled_at"] = json!(timestamp(disabled)?);
    }
    Ok(result)
}
pub fn timestamp(milliseconds: i64) -> Result<String, ApiError> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(milliseconds) * 1_000_000)
        .map_err(|_| internal())?
        .format(&Rfc3339)
        .map_err(|_| internal())
}
fn entry_json(entry: &TreeEntry) -> Value {
    let mut value = json!({"object_id": entry.object_id, "mode": format!("{:06o}", entry.mode)});
    add_bytes(&mut value, "path", &entry.path, false);
    value
}
fn add_bytes(value: &mut Value, name: &str, bytes: &[u8], always_base64: bool) {
    let (plain, encoded) = encode_pair(bytes, always_base64);
    if let Some(plain) = plain {
        value[name] = json!(plain);
    }
    if let Some(encoded) = encoded {
        value[format!("{name}_b64")] = json!(encoded);
    }
}
fn add_collision_warnings(value: &mut Value, entries: &[TreeEntry]) {
    let groups = collisions(entries);
    if !groups.is_empty() {
        let paths: Vec<Vec<Value>> = groups
            .into_iter()
            .map(|paths| {
                paths
                    .into_iter()
                    .map(|path| {
                        let mut value = json!({});
                        add_bytes(&mut value, "path", &path, false);
                        value
                    })
                    .collect()
            })
            .collect();
        value["warnings"] = json!([{"code": "path_collision", "message": "Some paths collide under case folding or Unicode normalization", "details": {"groups": paths}}]);
    }
}
pub fn store_error(error: StoreError) -> ApiError {
    match error {
        StoreError::Request(error) => error,
        _ => internal(),
    }
}
pub fn backend_error(error: BackendError) -> ApiError {
    match error {
        BackendError::RepositoryNotFound | BackendError::ObjectNotFound => not_found(),
        BackendError::StaleExpectation { actual } => {
            let mut e = ApiError::new(ErrorCode::RefConflict);
            if let Some(actual) = actual {
                e.details.insert("actual".into(), json!(actual));
            }
            e
        }
        BackendError::InvalidRequest { field, reason } => ApiError::invalid(&field, &reason),
        BackendError::UnsupportedPath { path } => {
            let mut e = ApiError::new(ErrorCode::UnsupportedPath);
            let v = json!(cambium_protocol::bytes::PathValue::from_bytes(&path));
            e.details = v.as_object().cloned().unwrap_or_default();
            e
        }
        BackendError::TooLarge { limit } => {
            let mut e = ApiError::new(ErrorCode::PayloadTooLarge);
            e.details.insert("limit_bytes".into(), json!(limit));
            e
        }
        BackendError::Contention => ApiError::new(ErrorCode::Contention),
        BackendError::Unavailable => ApiError::new(ErrorCode::BackendUnavailable),
        BackendError::RepositorySealed => ApiError::new(ErrorCode::RepositoryBusy),
        BackendError::Internal => internal(),
    }
}
pub fn internal() -> ApiError {
    ApiError::new(ErrorCode::Internal)
}
pub fn not_found() -> ApiError {
    ApiError::new(ErrorCode::NotFound)
}
