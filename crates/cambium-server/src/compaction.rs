//! Generation compaction. Only a confirmed seal and final reconcile permit deletion.

use cambium_backend::{
    BackendError, BackendRepositoryId, ObjectKind, RefCopy, RefExpectation, RefSnapshot, RefUpdate,
};
use cambium_protocol::{
    ApiError, BranchId, CompactionId, ErrorCode, ObjectId, Principal, RepositoryId, TenantId,
};
use cambium_store::{RepositoryRecord, compaction::CompactionRecord};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use crate::service::{
    App, Reservation, backend_error, backend_id, check_limit, decode_cursor, encode_cursor,
    internal, store_error, timestamp,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompactionSettings {
    pub max_delay_ms: i64,
    pub max_expired: i64,
    pub settle_ms: i64,
    pub drain_ms: i64,
}

#[derive(Serialize, Deserialize)]
struct PendingRef {
    name: String,
    expected: Option<ObjectId>,
    value: Option<ObjectId>,
}
impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            max_delay_ms: 86_400_000,
            max_expired: 100,
            settle_ms: 60_000,
            drain_ms: 3_600_000,
        }
    }
}

fn backend(value: String) -> Result<BackendRepositoryId, ApiError> {
    value.try_into().map_err(backend_error)
}
fn map(snapshot: &RefSnapshot) -> BTreeMap<String, ObjectId> {
    snapshot
        .refs
        .iter()
        .map(|r| (r.name.clone(), r.object_id.clone()))
        .collect()
}
fn prefix(record: &CompactionRecord) -> String {
    format!("refs/cambium/{}/", record.id)
}
fn digest(name: &str) -> String {
    format!("{:x}", Sha256::digest(name.as_bytes()))
}
fn millis(value: &Value, field: &str) -> Result<i64, ApiError> {
    value[field].as_i64().ok_or_else(internal)
}
fn busy() -> ApiError {
    ApiError::new(ErrorCode::RepositoryBusy)
}
fn filtered(mut snapshot: RefSnapshot) -> RefSnapshot {
    snapshot
        .refs
        .retain(|r| !r.name.starts_with("refs/cambium/"));
    snapshot.refs.sort_by(|a, b| a.name.cmp(&b.name));
    snapshot
}

impl App {
    pub(crate) async fn request_compaction(
        &self,
        principal: &Principal,
        repository: &RepositoryRecord,
        reservation: &Reservation<'_>,
    ) -> Result<Value, ApiError> {
        if !self.backend.capabilities().durable_seal {
            return Err(internal());
        }
        let id: CompactionId = if let Some(saved) = &reservation.saved {
            serde_json::from_value(saved["compaction_id"].clone()).map_err(|_| internal())?
        } else {
            let running = self
                .store
                .running_compaction(&repository.tenant_id, &repository.id)
                .await
                .map_err(store_error)?;
            let id = running.map(|record| record.id).unwrap_or_default();
            self.save(reservation, &json!({"compaction_id": id}))
                .await?;
            id
        };
        self.expire_branches(repository).await?;
        let record = self
            .store
            .start_compaction(
                &principal.tenant_id,
                &repository.id,
                Some(principal),
                Some(&id),
                self.clock.now_ms(),
            )
            .await
            .map_err(store_error)?;
        self.compaction_json(&record, 0, 100)
    }

    fn compaction_json(
        &self,
        record: &CompactionRecord,
        offset: usize,
        limit: usize,
    ) -> Result<Value, ApiError> {
        let mut value = json!({"compaction_id": record.id, "repository_id": record.repository_id, "stage": record.stage, "created_at": timestamp(record.created_at_ms)?, "updated_at": timestamp(record.updated_at_ms)?, "old_generation": record.old_generation, "new_generation": record.new_generation});
        if let Some(completed) = record.completed_at_ms {
            value["completed_at"] = json!(timestamp(completed)?);
        }
        let conflicts = record.state["conflicts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        value["conflict_count"] = json!(conflicts.len());
        if offset > conflicts.len() {
            return Err(ApiError::invalid(
                "page_token",
                "Cursor exceeds the conflict list",
            ));
        }
        value["conflicts"] = json!(
            conflicts
                .iter()
                .skip(offset)
                .take(limit)
                .collect::<Vec<_>>()
        );
        let next = offset.saturating_add(limit);
        value["conflicts_truncated"] = json!(conflicts.len() > next);
        if conflicts.len() > next {
            value["next_page_token"] = json!(encode_cursor(&(
                &record.tenant_id,
                &record.repository_id,
                &record.id,
                next
            ))?);
        }
        if record.completed_at_ms.is_none()
            && let Some(error) = record.state.get("last_error")
        {
            value["last_error"] = error.clone();
        }
        Ok(value)
    }

    pub(crate) async fn reclamation_json(
        &self,
        repository: &RepositoryRecord,
        input: &crate::requests::GetReclamation,
    ) -> Result<Value, ApiError> {
        check_limit(input.limit)?;
        let summary = self
            .store
            .expiry_summary(&repository.tenant_id, &repository.id)
            .await
            .map_err(store_error)?;
        let (selected, offset) = if let Some(token) = &input.page_token {
            let (tenant, repo, id, offset): (TenantId, RepositoryId, CompactionId, usize) =
                decode_cursor(token)?;
            if tenant != repository.tenant_id
                || repo != repository.id
                || input
                    .compaction_id
                    .as_ref()
                    .is_some_and(|selected| selected != &id)
            {
                return Err(ApiError::invalid(
                    "page_token",
                    "Cursor belongs to another query",
                ));
            }
            (Some(id), offset)
        } else {
            (input.compaction_id.clone(), 0)
        };
        let latest = if let Some(id) = selected {
            Some(
                self.store
                    .get_compaction(&repository.tenant_id, &repository.id, &id)
                    .await
                    .map_err(store_error)?,
            )
        } else {
            self.store
                .latest_compaction(&repository.tenant_id, &repository.id)
                .await
                .map_err(store_error)?
        };
        let mut value = json!({"repository_id": repository.id, "active_generation": repository.active_generation, "fenced": repository.fenced, "unreclaimed_expiries": summary.count, "overdue": summary.oldest_at_ms.is_some_and(|oldest| self.clock.now_ms().saturating_sub(oldest) >= self.compaction.max_delay_ms.saturating_add(self.compaction.drain_ms))});
        if let Some(oldest) = summary.oldest_at_ms {
            value["oldest_expiry_at"] = json!(timestamp(oldest)?);
        }
        if let Some(record) = latest {
            value["compaction"] = self.compaction_json(&record, offset, input.limit as usize)?;
        }
        Ok(value)
    }

    /// Caller owns the lifecycle lock, but ordinary API writes remain possible in Prepare.
    pub(crate) async fn advance_compaction(
        &self,
        mut record: CompactionRecord,
    ) -> Result<(), ApiError> {
        let result = self.advance_compaction_inner(&mut record).await;
        if let Err(error) = &result {
            // A pre-switch destination with an ambiguous mutation is never activated.
            // Re-read after a possibly ambiguous metadata commit before deciding which
            // generation is active. A later sweep also recognizes *_copying stages.
            if let Ok(Some(mut current)) = self
                .store
                .running_compaction(&record.tenant_id, &record.repository_id)
                .await
            {
                current.state["last_error"] = json!(error.code);
                if matches!(
                    current.stage.as_str(),
                    "prepare_copying" | "catchup_copying"
                ) {
                    current.stage = "aborting".into();
                }
                self.store
                    .save_compaction(&current, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
            }
        }
        result
    }

    async fn advance_compaction_inner(
        &self,
        record: &mut CompactionRecord,
    ) -> Result<(), ApiError> {
        let repository = self
            .store
            .get_repository(&record.tenant_id, &record.repository_id)
            .await
            .map_err(store_error)?;
        if repository.state != "ready" {
            return Ok(());
        }
        let old = backend(record.old_backend_id())?;
        let new = backend(record.new_backend_id())?;
        if record.state["needs_cleanup"] == true {
            let sealed = matches!(record.stage.as_str(), "sealed" | "deleting");
            self.complete_pending_ref(record, sealed).await?;
            self.complete_rescues(record, sealed).await?;
            self.clean_temporary(
                record,
                &old,
                matches!(record.stage.as_str(), "sealed" | "deleting"),
            )
            .await?;
            self.clean_temporary(record, &new, record.stage == "aborting")
                .await?;
            record.state["needs_cleanup"] = json!(false);
            self.store
                .save_compaction(record, self.clock.now_ms())
                .await
                .map_err(store_error)?;
        }
        match record.stage.as_str() {
            "preparing" => {
                if repository.active_generation != record.old_generation {
                    return Err(internal());
                }
                record.stage = "prepare_copying".into();
                self.store
                    .save_compaction(record, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
                self.backend
                    .create_repository(&new, &repository.default_branch)
                    .await
                    .map_err(backend_error)?;
                let snapshot = self.live_snapshot(&repository, &old).await?;
                self.match_snapshot(record, &old, &new, &snapshot).await?;
                record.stage = "prepared".into();
                self.store
                    .save_compaction(record, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
            }
            "prepare_copying" | "catchup_copying" => {
                record.stage = "aborting".into();
                self.store
                    .save_compaction(record, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
            }
            "prepared" => {
                let _guard = self
                    .repository_lock(&record.tenant_id, &record.repository_id)
                    .await?;
                self.expire_branches(&repository).await?;
                self.store
                    .fence_compaction(record, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
            }
            "catching_up" => {
                record.stage = "catchup_copying".into();
                self.store
                    .save_compaction(record, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
                let snapshot = self.live_snapshot(&repository, &old).await?;
                self.match_snapshot(record, &old, &new, &snapshot).await?;
                record.state["switch_snapshot"] = json!(snapshot);
                record.stage = "verifying".into();
                self.store
                    .save_compaction(record, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
            }
            "verifying" => {
                let snapshot: RefSnapshot =
                    serde_json::from_value(record.state["switch_snapshot"].clone())
                        .map_err(|_| internal())?;
                let actual = filtered(
                    self.backend
                        .list_refs(&new, &[])
                        .await
                        .map_err(backend_error)?,
                );
                if actual != snapshot {
                    record.stage = "aborting".into();
                    self.store
                        .save_compaction(record, self.clock.now_ms())
                        .await
                        .map_err(store_error)?;
                    return Err(busy());
                }
                self.clean_temporary(record, &old, false).await?;
                self.clean_temporary(record, &new, false).await?;
                let _guard = self
                    .repository_lock(&record.tenant_id, &record.repository_id)
                    .await?;
                self.store
                    .switch_generation(record, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
            }
            "draining" => {
                if repository.active_generation != record.new_generation {
                    return Err(internal());
                }
                let now = self.clock.now_ms();
                if now.saturating_sub(millis(&record.state, "switched_at_ms")?)
                    >= self.compaction.drain_ms
                {
                    // Historical intents may need an anchor in the writable old
                    // generation after its public ref has moved again.
                    self.complete_pending_ref(record, false).await?;
                    self.complete_rescues(record, false).await?;
                    record.stage = "sealing".into();
                    self.store
                        .save_compaction(record, now)
                        .await
                        .map_err(store_error)?;
                } else if now.saturating_sub(millis(&record.state, "last_reconciled_at_ms")?)
                    >= self.compaction.settle_ms
                {
                    self.reconcile(record, false).await?;
                    self.clean_temporary(record, &old, false).await?;
                    self.clean_temporary(record, &new, false).await?;
                    record.state["last_reconciled_at_ms"] = json!(now);
                    self.store
                        .save_compaction(record, now)
                        .await
                        .map_err(store_error)?;
                }
            }
            "sealing" => {
                if repository.active_generation != record.new_generation {
                    return Err(internal());
                }
                self.backend
                    .seal_repository(&old)
                    .await
                    .map_err(backend_error)?;
                record.stage = "sealed".into();
                self.store
                    .save_compaction(record, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
            }
            "sealed" => {
                if repository.active_generation != record.new_generation {
                    return Err(internal());
                }
                self.reconcile(record, true).await?;
                self.clean_temporary(record, &new, false).await?;
                record.stage = "deleting".into();
                self.store
                    .save_compaction(record, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
            }
            "deleting" => {
                if repository.active_generation != record.new_generation {
                    return Err(internal());
                }
                self.backend
                    .delete_repository(&old)
                    .await
                    .map_err(backend_error)?;
                self.store
                    .finish_compaction(record, false, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
            }
            "aborting" => {
                if repository.active_generation != record.old_generation {
                    return Err(internal());
                }
                // Sealing the abandoned destination also fences previously admitted
                // copies. A missing destination was never created or is already gone.
                match self.backend.seal_repository(&new).await {
                    Ok(()) | Err(BackendError::RepositoryNotFound) => {}
                    Err(error) => return Err(backend_error(error)),
                }
                self.clean_temporary(record, &old, false).await?;
                self.backend
                    .delete_repository(&new)
                    .await
                    .map_err(backend_error)?;
                self.store
                    .finish_compaction(record, true, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
            }
            _ => return Err(internal()),
        }
        Ok(())
    }

    async fn live_snapshot(
        &self,
        repository: &RepositoryRecord,
        id: &BackendRepositoryId,
    ) -> Result<RefSnapshot, ApiError> {
        let blocked = self
            .store
            .blocked_branch_names(&repository.tenant_id, &repository.id, self.clock.now_ms())
            .await
            .map_err(store_error)?;
        let mut snapshot = filtered(
            self.backend
                .list_refs(id, &[])
                .await
                .map_err(backend_error)?,
        );
        snapshot.refs.retain(|r| !blocked.contains(&r.name));
        Ok(snapshot)
    }

    async fn match_snapshot(
        &self,
        record: &CompactionRecord,
        old: &BackendRepositoryId,
        new: &BackendRepositoryId,
        snapshot: &RefSnapshot,
    ) -> Result<(), ApiError> {
        let actual = filtered(
            self.backend
                .list_refs(new, &[])
                .await
                .map_err(backend_error)?,
        );
        let mut current = map(&actual);
        let target = map(snapshot);
        for item in &snapshot.refs {
            if current.get(&item.name) == Some(&item.object_id) {
                continue;
            }
            self.copy_value(record, old, new, &item.name, &item.object_id, false)
                .await?;
            let expectation = current
                .get(&item.name)
                .cloned()
                .map(RefExpectation::At)
                .unwrap_or(RefExpectation::Absent);
            self.cas_ref(new, &item.name, expectation, Some(item.object_id.clone()))
                .await?;
            current.insert(item.name.clone(), item.object_id.clone());
        }
        for (name, oid) in current {
            if !target.contains_key(&name) {
                self.cas_ref(new, &name, RefExpectation::At(oid), None)
                    .await?;
            }
        }
        Ok(())
    }

    async fn copy_value(
        &self,
        record: &CompactionRecord,
        old: &BackendRepositoryId,
        new: &BackendRepositoryId,
        name: &str,
        oid: &ObjectId,
        sealed: bool,
    ) -> Result<(), ApiError> {
        let source = if sealed {
            name.to_owned()
        } else {
            let source = format!("{}source/{}/{}", prefix(record), digest(name), oid);
            self.cas_ref(old, &source, RefExpectation::Any, Some(oid.clone()))
                .await?;
            source
        };
        // Late or retried Git copies can only touch temporary refs, never a live ref.
        let destination = format!("{}copy/{}/{}", prefix(record), digest(name), oid);
        self.backend
            .copy_refs(
                old,
                new,
                &[RefCopy {
                    source,
                    destination: destination.clone(),
                    allow_non_fast_forward: true,
                }],
                true,
            )
            .await
            .map_err(backend_error)?;
        let actual = self
            .backend
            .list_refs(new, std::slice::from_ref(&destination))
            .await
            .map_err(backend_error)?;
        if !actual
            .refs
            .iter()
            .any(|r| r.name == destination && r.object_id == *oid)
        {
            return Err(busy());
        }
        Ok(())
    }

    async fn cas_ref(
        &self,
        id: &BackendRepositoryId,
        name: &str,
        expected: RefExpectation,
        new: Option<ObjectId>,
    ) -> Result<(), ApiError> {
        let outcomes = self
            .backend
            .update_refs(
                id,
                &[RefUpdate {
                    name: name.into(),
                    expected,
                    new,
                }],
            )
            .await
            .map_err(backend_error)?;
        if outcomes.len() != 1 || outcomes[0].rejection.is_some() {
            return Err(busy());
        }
        Ok(())
    }

    async fn clean_temporary(
        &self,
        record: &CompactionRecord,
        id: &BackendRepositoryId,
        sealed: bool,
    ) -> Result<(), ApiError> {
        if sealed {
            return Ok(());
        }
        let own = prefix(record);
        let snapshot = match self.backend.list_refs(id, std::slice::from_ref(&own)).await {
            Ok(snapshot) => snapshot,
            Err(BackendError::RepositoryNotFound) => return Ok(()),
            Err(error) => return Err(backend_error(error)),
        };
        for item in snapshot.refs {
            if item.name.starts_with(&own) {
                match self
                    .cas_ref(id, &item.name, RefExpectation::Any, None)
                    .await
                {
                    Ok(()) => {}
                    Err(error) if error.code == ErrorCode::RepositoryBusy => {
                        // A seal response may have been lost before its metadata save.
                        // Its temporary refs disappear when the generation is deleted.
                        if id.as_str() != record.old_backend_id() || record.stage != "sealing" {
                            return Err(error);
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }

    async fn skip_old_names(
        &self,
        record: &CompactionRecord,
    ) -> Result<BTreeSet<String>, ApiError> {
        let mut skipped: BTreeSet<String> =
            serde_json::from_value(record.state["excluded_names"].clone())
                .map_err(|_| internal())?;
        let owners = record.state["branch_owners"]
            .as_object()
            .ok_or_else(internal)?;
        for (name, id) in owners {
            let id: BranchId = serde_json::from_value(id.clone()).map_err(|_| internal())?;
            let branch = self
                .store
                .get_branch(&record.tenant_id, &record.repository_id, &id)
                .await
                .map_err(store_error)?;
            if matches!(branch.state.as_str(), "expiring" | "expired")
                || (branch.state == "active" && branch.expires_at_ms <= self.clock.now_ms())
            {
                skipped.insert(name.clone());
            }
        }
        Ok(skipped)
    }

    async fn reconcile(&self, record: &mut CompactionRecord, sealed: bool) -> Result<(), ApiError> {
        let old = backend(record.old_backend_id())?;
        let new = backend(record.new_backend_id())?;
        self.complete_pending_ref(record, sealed).await?;
        self.complete_rescues(record, sealed).await?;
        let snapshot: RefSnapshot = serde_json::from_value(record.state["switch_snapshot"].clone())
            .map_err(|_| internal())?;
        let before = map(&snapshot);
        let applied: BTreeMap<String, Option<ObjectId>> = serde_json::from_value(
            record
                .state
                .get("applied_refs")
                .cloned()
                .unwrap_or(json!({})),
        )
        .map_err(|_| internal())?;
        let current_old = map(&filtered(
            self.backend
                .list_refs(&old, &[])
                .await
                .map_err(backend_error)?,
        ));
        let skipped = self.skip_old_names(record).await?;
        let names: BTreeSet<_> = before
            .keys()
            .chain(current_old.keys())
            .chain(applied.keys())
            .cloned()
            .collect();
        for name in names {
            let old_value = current_old.get(&name);
            let baseline = applied
                .get(&name)
                .map(Option::as_ref)
                .unwrap_or_else(|| before.get(&name));
            if old_value == baseline || skipped.contains(&name) {
                continue;
            }
            let active = self.ref_value(&new, &name).await?;
            // Apply from our last successful value before checking ancestry: an
            // old-side rollback or deletion must undo our earlier application.
            if active.as_ref() == baseline {
                record.state["pending_ref"] = json!(PendingRef {
                    name,
                    expected: baseline.cloned(),
                    value: old_value.cloned(),
                });
                self.store
                    .save_compaction(record, self.clock.now_ms())
                    .await
                    .map_err(store_error)?;
                self.complete_pending_ref(record, sealed).await?;
            } else if !self
                .value_retained(&old, &new, old_value, active.as_ref())
                .await?
            {
                self.preserve_conflict(record, &name, old_value, active.as_ref(), sealed)
                    .await?;
            }
        }
        Ok(())
    }

    async fn ref_value(
        &self,
        repository: &BackendRepositoryId,
        name: &str,
    ) -> Result<Option<ObjectId>, ApiError> {
        Ok(self
            .backend
            .list_refs(repository, &[name.into()])
            .await
            .map_err(backend_error)?
            .refs
            .into_iter()
            .find(|r| r.name == name)
            .map(|r| r.object_id))
    }

    async fn value_retained(
        &self,
        old: &BackendRepositoryId,
        new: &BackendRepositoryId,
        value: Option<&ObjectId>,
        active: Option<&ObjectId>,
    ) -> Result<bool, ApiError> {
        if value == active {
            return Ok(true);
        }
        let (Some(value), Some(active)) = (value, active) else {
            return Ok(false);
        };
        let old_kind = self
            .backend
            .read_object(old, value)
            .await
            .map_err(backend_error)?
            .kind;
        let active_kind = self
            .backend
            .read_object(new, active)
            .await
            .map_err(backend_error)?
            .kind;
        if old_kind != ObjectKind::Commit || active_kind != ObjectKind::Commit {
            return Ok(false);
        }
        self.backend
            .is_ancestor(new, value, active)
            .await
            .map_err(backend_error)
    }

    async fn complete_pending_ref(
        &self,
        record: &mut CompactionRecord,
        sealed: bool,
    ) -> Result<(), ApiError> {
        let Some(pending) = record.state.get("pending_ref") else {
            return Ok(());
        };
        let pending: PendingRef =
            serde_json::from_value(pending.clone()).map_err(|_| internal())?;
        let old = backend(record.old_backend_id())?;
        let new = backend(record.new_backend_id())?;
        // An expired owner must not affect a new branch reusing the same name.
        if self.skip_old_names(record).await?.contains(&pending.name) {
            record
                .state
                .as_object_mut()
                .ok_or_else(internal)?
                .remove("pending_ref");
            return self
                .store
                .save_compaction(record, self.clock.now_ms())
                .await
                .map_err(store_error);
        }
        for _ in 0..16 {
            let active = self.ref_value(&new, &pending.name).await?;
            let applied = if active == pending.value {
                // The CAS may have committed before its response or metadata save
                // was lost. The durable intent identifies its resulting baseline.
                true
            } else if active == pending.expected {
                if let Some(oid) = &pending.value {
                    self.copy_value(record, &old, &new, &pending.name, oid, sealed)
                        .await?;
                }
                let expected = pending
                    .expected
                    .clone()
                    .map(RefExpectation::At)
                    .unwrap_or(RefExpectation::Absent);
                match self
                    .cas_ref(&new, &pending.name, expected, pending.value.clone())
                    .await
                {
                    Ok(()) => true,
                    Err(error) if error.code == ErrorCode::RepositoryBusy => continue,
                    Err(error) => return Err(error),
                }
            } else {
                if !self
                    .value_retained(&old, &new, pending.value.as_ref(), active.as_ref())
                    .await?
                {
                    self.preserve_conflict(
                        record,
                        &pending.name,
                        pending.value.as_ref(),
                        active.as_ref(),
                        sealed,
                    )
                    .await?;
                }
                false
            };
            if applied {
                if !record.state["applied_refs"].is_object() {
                    record.state["applied_refs"] = json!({});
                }
                record.state["applied_refs"][&pending.name] = json!(pending.value);
            }
            record
                .state
                .as_object_mut()
                .ok_or_else(internal)?
                .remove("pending_ref");
            self.store
                .save_compaction(record, self.clock.now_ms())
                .await
                .map_err(store_error)?;
            return Ok(());
        }
        Err(busy())
    }

    async fn preserve_conflict(
        &self,
        record: &mut CompactionRecord,
        name: &str,
        old_value: Option<&ObjectId>,
        active: Option<&ObjectId>,
        sealed: bool,
    ) -> Result<(), ApiError> {
        let mut conflict = json!({"ref": name});
        if let Some(oid) = old_value {
            conflict["old_value"] = json!(oid);
            conflict["rescue_ref"] = json!(format!(
                "refs/cambium-rescue/{}/{}/{}",
                record.id,
                digest(name),
                oid
            ));
        }
        if let Some(oid) = active {
            conflict["active_value"] = json!(oid);
        }
        if !record.state["conflicts"].is_array() {
            record.state["conflicts"] = json!([]);
        }
        let conflicts = record.state["conflicts"]
            .as_array_mut()
            .ok_or_else(internal)?;
        if !conflicts
            .iter()
            .any(|c| c["ref"] == conflict["ref"] && c.get("old_value") == conflict.get("old_value"))
        {
            conflicts.push(conflict);
            // Persist before the first backend mutation, including temporary copies.
            self.store
                .save_compaction(record, self.clock.now_ms())
                .await
                .map_err(store_error)?;
            eprintln!(
                "cambium: reconciliation conflict in {} ({})",
                record.repository_id, record.id
            );
        }
        self.complete_rescues(record, sealed).await
    }

    async fn complete_rescues(
        &self,
        record: &CompactionRecord,
        sealed: bool,
    ) -> Result<(), ApiError> {
        let old = backend(record.old_backend_id())?;
        let new = backend(record.new_backend_id())?;
        for conflict in record.state["conflicts"].as_array().into_iter().flatten() {
            let Some(name) = conflict.get("rescue_ref").and_then(Value::as_str) else {
                continue;
            };
            let oid: ObjectId =
                serde_json::from_value(conflict["old_value"].clone()).map_err(|_| internal())?;
            if let Some(existing) = self.ref_value(&new, name).await? {
                if existing != oid {
                    return Err(internal());
                }
            } else {
                let original = conflict["ref"].as_str().ok_or_else(internal)?;
                self.copy_value(record, &old, &new, original, &oid, sealed)
                    .await?;
                self.cas_ref(&new, name, RefExpectation::Absent, Some(oid))
                    .await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn clean_orphan_temporary_refs(
        &self,
        repository: &RepositoryRecord,
    ) -> Result<(), ApiError> {
        let now = self.clock.now_ms();
        if !self
            .store
            .claim_orphan_sweep(
                &repository.tenant_id,
                &repository.id,
                now,
                now.saturating_add(60_000),
            )
            .await
            .map_err(store_error)?
        {
            return Ok(());
        }
        let id = backend_id(repository)?;
        let snapshot = self
            .backend
            .list_refs(&id, &["refs/cambium/".into()])
            .await
            .map_err(backend_error)?;
        let mut groups = BTreeMap::<CompactionId, Vec<RefUpdate>>::new();
        for reference in snapshot.refs {
            let Some((candidate, _)) = reference
                .name
                .strip_prefix("refs/cambium/")
                .and_then(|rest| rest.split_once('/'))
            else {
                continue;
            };
            let Ok(compaction) = candidate.parse::<CompactionId>() else {
                continue;
            };
            groups.entry(compaction).or_default().push(RefUpdate {
                name: reference.name,
                expected: RefExpectation::At(reference.object_id),
                new: None,
            });
        }
        for (compaction, updates) in groups {
            let record = match self
                .store
                .get_compaction(&repository.tenant_id, &repository.id, &compaction)
                .await
            {
                Ok(record) => record,
                Err(error) if error.code() == ErrorCode::NotFound => continue,
                Err(error) => return Err(store_error(error)),
            };
            if record.completed_at_ms.is_none() {
                continue;
            }
            for batch in updates.chunks(256) {
                let results = self
                    .backend
                    .update_refs(&id, batch)
                    .await
                    .map_err(backend_error)?;
                if results.len() != batch.len()
                    || results.iter().any(|result| result.rejection.is_some())
                {
                    return Err(busy());
                }
            }
        }
        Ok(())
    }
}
