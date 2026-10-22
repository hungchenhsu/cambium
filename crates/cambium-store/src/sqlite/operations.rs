use super::*;
use crate::operations::{Claim, Completion, OperationKey, OperationsStore};
use cambium_protocol::RequestId;
use serde_json::Value;

const RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

#[async_trait]
impl OperationsStore for SqliteStore {
    async fn record_branch_activity(
        &self,
        principal: &Principal,
        repository: &RepositoryId,
        names: &[String],
        now_ms: i64,
    ) -> StoreResult<()> {
        let mut tx = self.writer.begin().await?;
        let (principal, _) = self
            .resolve(&mut tx, &principal.tenant_id, &principal.token_id, now_ms)
            .await?;
        principal.require_repository(repository, Scope::RepoWrite)?;
        for name in names {
            sqlx::query("UPDATE branches SET last_activity_at_ms = MAX(COALESCE(last_activity_at_ms, 0), ?), updated_at_ms = ?, updated_by = ?, updated_by_kind = ? WHERE tenant_id = ? AND repository_id = ? AND name = ? AND state = 'active' AND expires_at_ms > ?")
                .bind(now_ms).bind(now_ms).bind(principal.actor.identity_id.as_str()).bind(kind_name(principal.actor.kind)).bind(principal.tenant_id.as_str()).bind(repository.as_str()).bind(name).bind(now_ms).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn deployment_purge_expired_responses(&self, now_ms: i64) -> StoreResult<()> {
        sqlx::query("DELETE FROM idempotency WHERE expires_at_ms <= ? AND state != 'running'")
            .bind(now_ms)
            .execute(&self.writer)
            .await?;
        sqlx::query("DELETE FROM internal_tokens WHERE expires_at_ms <= ?")
            .bind(now_ms)
            .execute(&self.writer)
            .await?;
        Ok(())
    }

    async fn blocked_branch_names(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
        now_ms: i64,
    ) -> StoreResult<Vec<String>> {
        Ok(sqlx::query_scalar("SELECT DISTINCT b.name FROM branches b WHERE b.tenant_id = ? AND b.repository_id = ? AND (b.state = 'creating' OR b.state = 'expiring' OR (b.state = 'active' AND b.expires_at_ms <= ?) OR (EXISTS(SELECT 1 FROM expiries e WHERE e.tenant_id = b.tenant_id AND e.branch_id = b.id AND e.reclaimed_at_ms IS NULL) AND NOT EXISTS(SELECT 1 FROM branches live WHERE live.tenant_id = b.tenant_id AND live.repository_id = b.repository_id AND live.name = b.name AND live.rowid > b.rowid AND (live.state = 'promoted' OR (live.state = 'active' AND live.expires_at_ms > ?)))))")
            .bind(tenant.as_str()).bind(repository.as_str()).bind(now_ms).bind(now_ms).fetch_all(&self.readers).await?)
    }
    async fn get_repository_including_deleted(
        &self,
        tenant: &TenantId,
        id: &RepositoryId,
    ) -> StoreResult<RepositoryRecord> {
        let row = sqlx::query("SELECT * FROM repositories WHERE tenant_id = ? AND id = ?")
            .bind(tenant.as_str())
            .bind(id.as_str())
            .fetch_optional(&self.readers)
            .await?
            .ok_or_else(|| request(ErrorCode::NotFound))?;
        repository(&row)
    }

    async fn finish_repository_creation(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
        now_ms: i64,
    ) -> StoreResult<()> {
        let mut tx = self.writer.begin().await?;
        sqlx::query("UPDATE generations SET state = 'active', updated_at_ms = ? WHERE tenant_id = ? AND repository_id = ? AND number = 1 AND state = 'pending' AND EXISTS(SELECT 1 FROM repositories WHERE tenant_id = ? AND id = ? AND state = 'creating')")
            .bind(now_ms).bind(tenant.as_str()).bind(repository.as_str()).bind(tenant.as_str()).bind(repository.as_str()).execute(&mut *tx).await?;
        sqlx::query("UPDATE repositories SET state = 'ready', updated_at_ms = ? WHERE tenant_id = ? AND id = ? AND state = 'creating'")
            .bind(now_ms).bind(tenant.as_str()).bind(repository.as_str()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    async fn check_mint_token(
        &self,
        principal: &Principal,
        input: &MintToken,
        now_ms: i64,
    ) -> StoreResult<()> {
        let mut tx = self.readers.begin().await?;
        self.check_mint_at(&mut tx, principal, input, now_ms)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn token_identity(&self, tenant: &TenantId, token: &TokenId) -> StoreResult<IdentityId> {
        let value: String =
            sqlx::query_scalar("SELECT identity_id FROM tokens WHERE tenant_id = ? AND id = ?")
                .bind(tenant.as_str())
                .bind(token.as_str())
                .fetch_optional(&self.readers)
                .await?
                .ok_or_else(|| request(ErrorCode::NotFound))?;
        parse(value)
    }
    async fn revalidate(
        &self,
        tenant: &TenantId,
        token: &TokenId,
        now_ms: i64,
    ) -> StoreResult<Principal> {
        let mut tx = self.readers.begin().await?;
        let result = self.resolve(&mut tx, tenant, token, now_ms).await?.0;
        tx.commit().await?;
        Ok(result)
    }

    async fn tenant_slug(&self, tenant: &TenantId) -> StoreResult<String> {
        sqlx::query_scalar("SELECT slug FROM tenants WHERE id = ?")
            .bind(tenant.as_str())
            .fetch_optional(&self.readers)
            .await?
            .ok_or_else(|| request(ErrorCode::NotFound))
    }

    async fn find_repository(
        &self,
        tenant: &TenantId,
        name: &str,
    ) -> StoreResult<RepositoryRecord> {
        let row = sqlx::query(
            "SELECT * FROM repositories WHERE tenant_id = ? AND name = ? AND state != 'deleted'",
        )
        .bind(tenant.as_str())
        .bind(name)
        .fetch_optional(&self.readers)
        .await?
        .ok_or_else(|| request(ErrorCode::NotFound))?;
        repository(&row)
    }

    async fn list_repositories(
        &self,
        tenant: &TenantId,
        after: Option<&RepositoryId>,
        limit: u32,
    ) -> StoreResult<Vec<RepositoryRecord>> {
        let rows = sqlx::query("SELECT * FROM repositories WHERE tenant_id = ? AND id > ? AND state != 'deleted' ORDER BY id LIMIT ?")
            .bind(tenant.as_str()).bind(after.map(RepositoryId::as_str).unwrap_or("")).bind(limit.min(1001)).fetch_all(&self.readers).await?;
        rows.iter().map(repository).collect()
    }

    async fn begin_repository_deletion(
        &self,
        principal: &Principal,
        id: &RepositoryId,
        now_ms: i64,
    ) -> StoreResult<RepositoryRecord> {
        let mut tx = self.writer.begin().await?;
        let principal = self
            .resolve(&mut tx, &principal.tenant_id, &principal.token_id, now_ms)
            .await?
            .0;
        principal.require_repository(id, Scope::RepoAdmin)?;
        let row = sqlx::query(
            "SELECT * FROM repositories WHERE tenant_id = ? AND id = ? AND state != 'deleted'",
        )
        .bind(principal.tenant_id.as_str())
        .bind(id.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| request(ErrorCode::NotFound))?;
        let record = repository(&row)?;
        sqlx::query("UPDATE repositories SET state = 'deleting', fenced = 1, updated_at_ms = ?, updated_by = ?, updated_by_kind = ? WHERE tenant_id = ? AND id = ?")
            .bind(now_ms).bind(principal.actor.identity_id.as_str()).bind(kind_name(principal.actor.kind)).bind(principal.tenant_id.as_str()).bind(id.as_str()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn generation_ids(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
    ) -> StoreResult<Vec<String>> {
        Ok(sqlx::query_scalar("SELECT backend_id FROM generations WHERE tenant_id = ? AND repository_id = ? AND state != 'deleted' ORDER BY number")
            .bind(tenant.as_str()).bind(repository.as_str()).fetch_all(&self.readers).await?)
    }

    async fn finish_repository_deletion(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
        now_ms: i64,
    ) -> StoreResult<()> {
        let mut tx = self.writer.begin().await?;
        // Preserve the requesting actor while recovery completes its operation.
        sqlx::query("UPDATE generations SET state = 'deleted', updated_at_ms = ? WHERE tenant_id = ? AND repository_id = ? AND EXISTS(SELECT 1 FROM repositories WHERE tenant_id = ? AND id = ? AND state = 'deleting')")
            .bind(now_ms).bind(tenant.as_str()).bind(repository.as_str()).bind(tenant.as_str()).bind(repository.as_str()).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM internal_tokens WHERE tenant_id = ? AND compaction_id IN (SELECT id FROM compactions WHERE tenant_id = ? AND repository_id = ?)")
            .bind(tenant.as_str()).bind(tenant.as_str()).bind(repository.as_str()).execute(&mut *tx).await?;
        sqlx::query("UPDATE compactions SET stage = 'deleted', completed_at_ms = ?, updated_at_ms = ?, updated_by = 'idn_00000000000000000000000000', updated_by_kind = 'system' WHERE tenant_id = ? AND repository_id = ? AND completed_at_ms IS NULL")
            .bind(now_ms).bind(now_ms).bind(tenant.as_str()).bind(repository.as_str()).execute(&mut *tx).await?;
        sqlx::query("UPDATE expiries SET reclaimed_at_ms = ?, updated_at_ms = ?, updated_by = 'idn_00000000000000000000000000', updated_by_kind = 'system' WHERE tenant_id = ? AND repository_id = ? AND reclaimed_at_ms IS NULL")
            .bind(now_ms).bind(now_ms).bind(tenant.as_str()).bind(repository.as_str()).execute(&mut *tx).await?;
        sqlx::query("UPDATE repositories SET state = 'deleted', updated_at_ms = ? WHERE tenant_id = ? AND id = ? AND state = 'deleting'")
            .bind(now_ms).bind(tenant.as_str()).bind(repository.as_str()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn deployment_pending_repositories(&self) -> StoreResult<Vec<RepositoryRecord>> {
        let rows = sqlx::query("SELECT * FROM repositories WHERE state IN ('creating', 'deleting') ORDER BY tenant_id, id").fetch_all(&self.readers).await?;
        rows.iter().map(repository).collect()
    }

    async fn deployment_recover_operations(&self, now_ms: i64) -> StoreResult<()> {
        sqlx::query("UPDATE idempotency SET state = 'retryable', run_id = NULL, updated_at_ms = ? WHERE state = 'running'")
            .bind(now_ms).execute(&self.writer).await?;
        Ok(())
    }

    async fn claim_operation(
        &self,
        principal: &Principal,
        key: &OperationKey,
        now_ms: i64,
    ) -> StoreResult<Claim> {
        if key.key.is_empty()
            || key.key.len() > 255
            || !key.key.bytes().all(|b| (0x20..=0x7e).contains(&b))
        {
            return Err(invalid(
                "idempotency_key",
                "must contain 1 to 255 printable ASCII characters",
            ));
        }
        let expiry = now_ms
            .checked_add(RETENTION_MS)
            .ok_or(StoreError::Corrupt)?;
        let mut tx = self.writer.begin().await?;
        let principal = self
            .resolve(&mut tx, &principal.tenant_id, &principal.token_id, now_ms)
            .await?
            .0;
        let tenant = principal.tenant_id.as_str();
        let identity = principal.actor.identity_id.as_str();
        // A running operation is never evicted while its owner may still finish.
        sqlx::query("DELETE FROM idempotency WHERE tenant_id = ? AND identity_id = ? AND key = ? AND expires_at_ms <= ? AND state != 'running'")
            .bind(tenant).bind(identity).bind(&key.key).bind(now_ms).execute(&mut *tx).await?;
        let row = sqlx::query(
            "SELECT * FROM idempotency WHERE tenant_id = ? AND identity_id = ? AND key = ?",
        )
        .bind(tenant)
        .bind(identity)
        .bind(&key.key)
        .fetch_optional(&mut *tx)
        .await?;
        let run_id = RequestId::new();
        let mut reservation = None;
        if let Some(row) = row {
            if row.try_get::<Vec<u8>, _>("fingerprint")? != key.fingerprint {
                return Err(request(ErrorCode::IdempotencyKeyReused));
            }
            match row.try_get::<String, _>("state")?.as_str() {
                "running" => return Err(request(ErrorCode::IdempotencyKeyInUse)),
                "complete" => {
                    let status: i64 = row.try_get("response_status")?;
                    let response = row.try_get("response_json")?;
                    tx.commit().await?;
                    return Ok(Claim::Replay {
                        status: status.try_into().map_err(|_| StoreError::Corrupt)?,
                        encrypted_response: response,
                    });
                }
                "retryable" => {
                    reservation = row
                        .try_get::<Option<String>, _>("reservation_json")?
                        .map(|v| serde_json::from_str(&v).map_err(|_| StoreError::Corrupt))
                        .transpose()?;
                }
                _ => return Err(StoreError::Corrupt),
            }
            sqlx::query("UPDATE idempotency SET state = 'running', run_id = ?, updated_at_ms = ?, updated_by = ?, updated_by_kind = ? WHERE tenant_id = ? AND identity_id = ? AND key = ?")
                .bind(run_id.as_str()).bind(now_ms).bind(identity).bind(kind_name(principal.actor.kind)).bind(tenant).bind(identity).bind(&key.key).execute(&mut *tx).await?;
        } else {
            sqlx::query("INSERT INTO idempotency (tenant_id, identity_id, key, fingerprint, state, run_id, created_at_ms, updated_at_ms, expires_at_ms, created_by, created_by_kind, updated_by, updated_by_kind) VALUES (?, ?, ?, ?, 'running', ?, ?, ?, ?, ?, ?, ?, ?)")
                .bind(tenant).bind(identity).bind(&key.key).bind(key.fingerprint.as_slice()).bind(run_id.as_str()).bind(now_ms).bind(now_ms).bind(expiry).bind(identity).bind(kind_name(principal.actor.kind)).bind(identity).bind(kind_name(principal.actor.kind)).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(Claim::Run {
            run_id,
            reservation,
        })
    }

    async fn reserve_operation(
        &self,
        principal: &Principal,
        key: &OperationKey,
        run_id: &RequestId,
        reservation: &Value,
        now_ms: i64,
    ) -> StoreResult<()> {
        let changed = sqlx::query("UPDATE idempotency SET reservation_json = ?, updated_at_ms = ? WHERE tenant_id = ? AND identity_id = ? AND key = ? AND fingerprint = ? AND state = 'running' AND run_id = ?")
            .bind(reservation.to_string()).bind(now_ms).bind(principal.tenant_id.as_str()).bind(principal.actor.identity_id.as_str()).bind(&key.key).bind(key.fingerprint.as_slice()).bind(run_id.as_str()).execute(&self.writer).await?.rows_affected();
        if changed != 1 {
            return Err(request(ErrorCode::IdempotencyKeyInUse));
        }
        Ok(())
    }

    async fn finish_operation(
        &self,
        principal: &Principal,
        key: &OperationKey,
        run_id: &RequestId,
        completion: Option<&Completion>,
        now_ms: i64,
    ) -> StoreResult<()> {
        let changed = sqlx::query("UPDATE idempotency SET state = ?, response_status = ?, response_json = ?, run_id = NULL, updated_at_ms = ? WHERE tenant_id = ? AND identity_id = ? AND key = ? AND fingerprint = ? AND state = 'running' AND run_id = ?")
            .bind(if completion.is_some() { "complete" } else { "retryable" }).bind(completion.map(|v| i64::from(v.status))).bind(completion.map(|v| &v.encrypted_response)).bind(now_ms).bind(principal.tenant_id.as_str()).bind(principal.actor.identity_id.as_str()).bind(&key.key).bind(key.fingerprint.as_slice()).bind(run_id.as_str()).execute(&self.writer).await?.rows_affected();
        if changed != 1 {
            return Err(request(ErrorCode::IdempotencyKeyInUse));
        }
        Ok(())
    }
}
