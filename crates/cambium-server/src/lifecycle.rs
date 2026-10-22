//! Recoverable ephemeral ref ownership and expiry, serialized per repository.

use cambium_backend::{ObjectKind, RefExpectation, RefUpdate};
use cambium_protocol::{ApiError, BranchId, ErrorCode, ObjectId, Principal};
use cambium_store::{
    RepositoryRecord,
    lifecycle::{BranchCreation, BranchRecord},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    requests::Operation,
    service::{
        App, Reservation, backend_error, backend_id, check_limit, decode_cursor, encode_cursor,
        internal, store_error, timestamp, validate_branch,
    },
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LifecycleSettings {
    pub max_ttl_seconds: u64,
}
impl Default for LifecycleSettings {
    fn default() -> Self {
        Self {
            max_ttl_seconds: 7 * 24 * 3600,
        }
    }
}

impl App {
    pub(crate) fn branch_deadline(&self, seconds: u64) -> Result<i64, ApiError> {
        if seconds == 0 || seconds > self.lifecycle.max_ttl_seconds {
            return Err(ApiError::invalid(
                "ttl_seconds",
                "Duration must be positive and within branches.max_ttl_seconds",
            ));
        }
        let duration = i64::try_from(seconds)
            .ok()
            .and_then(|v| v.checked_mul(1000))
            .ok_or_else(|| ApiError::invalid("ttl_seconds", "Duration is too large"))?;
        self.clock
            .now_ms()
            .checked_add(duration)
            .ok_or_else(|| ApiError::invalid("ttl_seconds", "Duration is too large"))
    }

    pub(crate) async fn execute_branch(
        &self,
        principal: &Principal,
        operation: &Operation,
        reservation: Option<&Reservation<'_>>,
    ) -> Result<Value, ApiError> {
        match operation {
            Operation::CreateEphemeralBranch(input) => {
                if !self.backend.capabilities().durable_ref_creation {
                    return Err(internal());
                }
                let reservation = reservation.ok_or_else(internal)?;
                let creation: BranchCreation = if let Some(saved) = &reservation.saved {
                    serde_json::from_value(saved.clone()).map_err(|_| internal())?
                } else {
                    let creation = BranchCreation {
                        id: BranchId::new(),
                        repository_id: input.repository_id.clone(),
                        name: validate_branch(&input.name)?,
                        start_commit: input.start_commit.clone(),
                        expires_at_ms: self.branch_deadline(input.ttl_seconds)?,
                    };
                    self.save(reservation, &json!(creation)).await?;
                    creation
                };
                let repository = self
                    .readable_repository(principal, &input.repository_id)
                    .await?;
                if repository.fenced {
                    return Err(ApiError::new(ErrorCode::RepositoryBusy));
                }
                let object = self
                    .backend
                    .read_object(&backend_id(&repository)?, &creation.start_commit)
                    .await
                    .map_err(backend_error)?;
                if object.kind != ObjectKind::Commit {
                    return Err(ApiError::invalid("start_commit", "Expected a commit"));
                }
                let record = self
                    .store
                    .reserve_branch(principal, &creation, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
                if record.state == "creating" && !self.resolve_creation(&record).await? {
                    return Err(ApiError::new(ErrorCode::AlreadyExists));
                }
                let record = self
                    .store
                    .get_branch(&principal.tenant_id, &input.repository_id, &creation.id)
                    .await
                    .map_err(store_error)?;
                self.branch_json(&record)
            }
            Operation::GetBranch(input) => self.branch_json(
                &self
                    .store
                    .get_branch(&principal.tenant_id, &input.repository_id, &input.branch_id)
                    .await
                    .map_err(store_error)?,
            ),
            Operation::ListBranches(input) => {
                check_limit(input.limit)?;
                let binding = json!([principal.tenant_id, input.repository_id]);
                let after = if let Some(token) = &input.page_token {
                    let (saved, id): (Value, BranchId) = decode_cursor(token)?;
                    if saved != binding {
                        return Err(ApiError::invalid(
                            "page_token",
                            "Cursor belongs to another query",
                        ));
                    }
                    Some(id)
                } else {
                    None
                };
                let mut records = self
                    .store
                    .list_branches(
                        &principal.tenant_id,
                        &input.repository_id,
                        after.as_ref(),
                        input.limit + 1,
                    )
                    .await
                    .map_err(store_error)?;
                let truncated = records.len() > input.limit as usize;
                records.truncate(input.limit as usize);
                let values = records
                    .iter()
                    .map(|record| self.branch_json(record))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut response = json!({"branches": values, "truncated": truncated});
                if truncated {
                    response["next_page_token"] = json!(encode_cursor(&(
                        binding,
                        &records.last().ok_or_else(internal)?.id
                    ))?);
                }
                Ok(response)
            }
            Operation::ExtendBranch(input) => {
                let reservation = reservation.ok_or_else(internal)?;
                let deadline = if let Some(saved) = &reservation.saved {
                    saved["expires_at_ms"].as_i64().ok_or_else(internal)?
                } else {
                    let deadline = self.branch_deadline(input.ttl_seconds)?;
                    self.save(reservation, &json!({"expires_at_ms": deadline}))
                        .await?;
                    deadline
                };
                self.branch_json(
                    &self
                        .store
                        .change_branch(
                            principal,
                            &input.repository_id,
                            &input.branch_id,
                            Some(deadline),
                            self.clock.now_ms(),
                        )
                        .await
                        .map_err(store_error)?,
                )
            }
            Operation::PromoteBranch(input) => self.branch_json(
                &self
                    .store
                    .change_branch(
                        principal,
                        &input.repository_id,
                        &input.branch_id,
                        None,
                        self.clock.now_ms(),
                    )
                    .await
                    .map_err(store_error)?,
            ),
            _ => Err(internal()),
        }
    }

    pub(crate) fn branch_json(&self, record: &BranchRecord) -> Result<Value, ApiError> {
        let state = if record.state == "active" && record.expires_at_ms <= self.clock.now_ms() {
            "expiring"
        } else {
            &record.state
        };
        let mut value = json!({"branch_id": record.id, "repository_id": record.repository_id, "name": record.name, "state": state, "created_at": timestamp(record.created_at_ms)?, "created_by": record.created_by});
        if record.state != "promoted" {
            value["expires_at"] = json!(timestamp(record.expires_at_ms)?);
        }
        if let Some(last) = record.last_activity_at_ms {
            value["last_activity_at"] = json!(timestamp(last)?);
        }
        Ok(value)
    }

    async fn resolve_creation(&self, record: &BranchRecord) -> Result<bool, ApiError> {
        let created = self
            .backend
            .create_ref_once(
                &record
                    .creation_backend_id
                    .clone()
                    .try_into()
                    .map_err(backend_error)?,
                record.id.as_str(),
                &record.name,
                &record.start_commit,
            )
            .await
            .map_err(backend_error)?;
        self.store
            .finish_branch_creation(&record.tenant_id, &record.id, created, self.clock.now_ms())
            .await
            .map_err(store_error)?;
        Ok(created)
    }

    pub async fn run_lifecycle_once(&self) -> Result<(), ApiError> {
        let repositories = self
            .store
            .deployment_lifecycle_repositories()
            .await
            .map_err(store_error)?;
        let mut error = None;
        for repository in repositories {
            let _lifecycle = self
                .lifecycle_lock(&repository.tenant_id, &repository.id)
                .await?;
            let result = async {
                let guard = self
                    .repository_lock(&repository.tenant_id, &repository.id)
                    .await?;
                let current = self
                    .store
                    .get_repository_including_deleted(&repository.tenant_id, &repository.id)
                    .await
                    .map_err(store_error)?;
                if current.state != "ready" {
                    return Ok(());
                }
                self.expire_branches(&current).await?;
                self.clean_orphan_temporary_refs(&current).await?;
                let mut compaction = self
                    .store
                    .running_compaction(&current.tenant_id, &current.id)
                    .await
                    .map_err(store_error)?;
                if compaction.is_none() {
                    let summary = self
                        .store
                        .expiry_summary(&current.tenant_id, &current.id)
                        .await
                        .map_err(store_error)?;
                    if summary.count > 0
                        && (summary.count >= self.compaction.max_expired
                            || summary.oldest_at_ms.is_some_and(|oldest| {
                                self.clock.now_ms().saturating_sub(oldest)
                                    >= self.compaction.max_delay_ms
                            }))
                    {
                        compaction = Some(
                            self.store
                                .start_compaction(
                                    &current.tenant_id,
                                    &current.id,
                                    None,
                                    None,
                                    self.clock.now_ms(),
                                )
                                .await
                                .map_err(store_error)?,
                        );
                    }
                }
                drop(guard);
                if let Some(compaction) = compaction {
                    self.advance_compaction(compaction).await?;
                }
                Ok::<_, ApiError>(())
            }
            .await;
            if let Err(failure) = result {
                error = Some(failure);
            }
        }
        error.map_or(Ok(()), Err)
    }

    /// Caller holds the repository lock. Never promote a ref by comparing its OID.
    pub(crate) async fn expire_branches(
        &self,
        repository: &RepositoryRecord,
    ) -> Result<(), ApiError> {
        for mut record in self
            .store
            .pending_branches(&repository.tenant_id, &repository.id, self.clock.now_ms())
            .await
            .map_err(store_error)?
        {
            if record.state == "creating" {
                if !self.resolve_creation(&record).await? {
                    continue;
                }
                record = self
                    .store
                    .get_branch(&record.tenant_id, &record.repository_id, &record.id)
                    .await
                    .map_err(store_error)?;
            }
            if record.state == "active" && record.expires_at_ms > self.clock.now_ms() {
                continue;
            }
            self.store
                .begin_branch_expiry(&record.tenant_id, &record.id, self.clock.now_ms())
                .await
                .map_err(store_error)?;
            self.remove_expired_ref(repository, &record.name).await?;
            self.store
                .finish_branch_expiry(&record.tenant_id, &record.id, self.clock.now_ms())
                .await
                .map_err(store_error)?;
        }
        // A push admitted before expiry can still recreate the old ref. Keep it
        // hidden and remove it again until the old generation is reclaimed.
        for name in self
            .store
            .expired_branch_names(&repository.tenant_id, &repository.id)
            .await
            .map_err(store_error)?
        {
            self.remove_expired_ref(repository, &name).await?;
        }
        Ok(())
    }

    async fn remove_expired_ref(
        &self,
        repository: &RepositoryRecord,
        name: &str,
    ) -> Result<(), ApiError> {
        let results = self
            .backend
            .update_refs(
                &backend_id(repository)?,
                &[RefUpdate {
                    name: name.into(),
                    expected: RefExpectation::Any,
                    new: None,
                }],
            )
            .await
            .map_err(backend_error)?;
        if results.len() != 1 || results[0].rejection.is_some() {
            return Err(ApiError::new(ErrorCode::RepositoryBusy));
        }
        Ok(())
    }

    pub(crate) async fn check_branch_available(
        &self,
        repository: &RepositoryRecord,
        revision: &str,
    ) -> Result<(), ApiError> {
        if revision.parse::<ObjectId>().is_ok() {
            return Ok(());
        }
        let name = if revision == "HEAD" {
            repository.default_branch.clone()
        } else if revision.starts_with("refs/") {
            revision.into()
        } else {
            format!("refs/heads/{revision}")
        };
        let blocked = self
            .store
            .blocked_branch_names(&repository.tenant_id, &repository.id, self.clock.now_ms())
            .await
            .map_err(store_error)?;
        if blocked.contains(&name) {
            if self
                .store
                .pending_branches(&repository.tenant_id, &repository.id, self.clock.now_ms())
                .await
                .map_err(store_error)?
                .iter()
                .any(|branch| branch.name == name && branch.state == "creating")
            {
                return Err(ApiError::new(ErrorCode::RepositoryBusy));
            }
            return Err(ApiError::new(ErrorCode::BranchExpired));
        }
        Ok(())
    }
}
