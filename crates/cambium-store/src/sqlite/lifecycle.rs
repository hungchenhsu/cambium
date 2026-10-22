use super::*;
use crate::lifecycle::{BranchCreation, BranchRecord, LifecycleStore};
use cambium_protocol::BranchId;

fn branch(row: &SqliteRow) -> StoreResult<BranchRecord> {
    Ok(BranchRecord {
        tenant_id: parse(row.try_get("tenant_id")?)?,
        id: parse(row.try_get("id")?)?,
        repository_id: parse(row.try_get("repository_id")?)?,
        name: row.try_get("name")?,
        start_commit: parse(row.try_get("start_commit")?)?,
        creation_backend_id: row.try_get("creation_backend_id")?,
        expires_at_ms: row.try_get("expires_at_ms")?,
        state: row.try_get("state")?,
        created_at_ms: row.try_get("created_at_ms")?,
        created_by: actor(row)?,
        last_activity_at_ms: row.try_get("last_activity_at_ms")?,
    })
}

#[async_trait]
impl LifecycleStore for SqliteStore {
    async fn reserve_branch(
        &self,
        principal: &Principal,
        input: &BranchCreation,
        now_ms: i64,
    ) -> StoreResult<BranchRecord> {
        let mut tx = self.writer.begin().await?;
        let (principal, _) = self
            .resolve(&mut tx, &principal.tenant_id, &principal.token_id, now_ms)
            .await?;
        principal.require_repository(&input.repository_id, Scope::RepoWrite)?;
        let row = sqlx::query(
            "SELECT * FROM repositories WHERE tenant_id = ? AND id = ? AND state = 'ready'",
        )
        .bind(principal.tenant_id.as_str())
        .bind(input.repository_id.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| request(ErrorCode::NotFound))?;
        let repo = repository(&row)?;
        if repo.fenced {
            return Err(request(ErrorCode::RepositoryBusy));
        }
        if let Some(row) = sqlx::query("SELECT * FROM branches WHERE tenant_id = ? AND id = ?")
            .bind(principal.tenant_id.as_str())
            .bind(input.id.as_str())
            .fetch_optional(&mut *tx)
            .await?
        {
            let existing = branch(&row)?;
            if existing.repository_id != input.repository_id
                || existing.name != input.name
                || existing.start_commit != input.start_commit
            {
                return Err(StoreError::Corrupt);
            }
            return Ok(existing);
        }
        if input.expires_at_ms <= now_ms {
            return Err(request(ErrorCode::BranchExpired));
        }
        let backend = format!(
            "{}/{}/g{}",
            principal.tenant_id, repo.id, repo.active_generation
        );
        sqlx::query("INSERT INTO branches (tenant_id, id, repository_id, name, expires_at_ms, state, created_at_ms, created_by, created_by_kind, updated_at_ms, updated_by, updated_by_kind, creation_backend_id, start_commit) VALUES (?, ?, ?, ?, ?, 'creating', ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(principal.tenant_id.as_str()).bind(input.id.as_str()).bind(repo.id.as_str()).bind(&input.name).bind(input.expires_at_ms).bind(now_ms).bind(principal.actor.identity_id.as_str()).bind(kind_name(principal.actor.kind)).bind(now_ms).bind(principal.actor.identity_id.as_str()).bind(kind_name(principal.actor.kind)).bind(&backend).bind(input.start_commit.as_str()).execute(&mut *tx).await.map_err(map_unique)?;
        let row = sqlx::query("SELECT * FROM branches WHERE tenant_id = ? AND id = ?")
            .bind(principal.tenant_id.as_str())
            .bind(input.id.as_str())
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        branch(&row)
    }

    async fn finish_branch_creation(
        &self,
        tenant: &TenantId,
        id: &BranchId,
        created: bool,
        now_ms: i64,
    ) -> StoreResult<()> {
        if created {
            sqlx::query("UPDATE branches SET state = 'active', updated_at_ms = ?, updated_by = 'idn_00000000000000000000000000', updated_by_kind = 'system' WHERE tenant_id = ? AND id = ? AND state = 'creating'")
                .bind(now_ms).bind(tenant.as_str()).bind(id.as_str()).execute(&self.writer).await?;
        } else {
            sqlx::query(
                "DELETE FROM branches WHERE tenant_id = ? AND id = ? AND state = 'creating'",
            )
            .bind(tenant.as_str())
            .bind(id.as_str())
            .execute(&self.writer)
            .await?;
        }
        Ok(())
    }

    async fn get_branch(
        &self,
        tenant: &TenantId,
        repo: &RepositoryId,
        id: &BranchId,
    ) -> StoreResult<BranchRecord> {
        let row = sqlx::query(
            "SELECT * FROM branches WHERE tenant_id = ? AND repository_id = ? AND id = ?",
        )
        .bind(tenant.as_str())
        .bind(repo.as_str())
        .bind(id.as_str())
        .fetch_optional(&self.readers)
        .await?
        .ok_or_else(|| request(ErrorCode::NotFound))?;
        branch(&row)
    }

    async fn list_branches(
        &self,
        tenant: &TenantId,
        repo: &RepositoryId,
        after: Option<&BranchId>,
        limit: u32,
    ) -> StoreResult<Vec<BranchRecord>> {
        let rows = sqlx::query("SELECT * FROM branches WHERE tenant_id = ? AND repository_id = ? AND id > ? ORDER BY id LIMIT ?")
            .bind(tenant.as_str()).bind(repo.as_str()).bind(after.map(BranchId::as_str).unwrap_or("")).bind(limit.min(1001)).fetch_all(&self.readers).await?;
        rows.iter().map(branch).collect()
    }

    async fn change_branch(
        &self,
        principal: &Principal,
        repo: &RepositoryId,
        id: &BranchId,
        expires_at_ms: Option<i64>,
        now_ms: i64,
    ) -> StoreResult<BranchRecord> {
        let mut tx = self.writer.begin().await?;
        let (principal, _) = self
            .resolve(&mut tx, &principal.tenant_id, &principal.token_id, now_ms)
            .await?;
        principal.require_repository(repo, Scope::RepoWrite)?;
        let row = sqlx::query(
            "SELECT * FROM repositories WHERE tenant_id = ? AND id = ? AND state = 'ready'",
        )
        .bind(principal.tenant_id.as_str())
        .bind(repo.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| request(ErrorCode::NotFound))?;
        if repository(&row)?.fenced {
            return Err(request(ErrorCode::RepositoryBusy));
        }
        let row = sqlx::query(
            "SELECT * FROM branches WHERE tenant_id = ? AND repository_id = ? AND id = ?",
        )
        .bind(principal.tenant_id.as_str())
        .bind(repo.as_str())
        .bind(id.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| request(ErrorCode::NotFound))?;
        let current = branch(&row)?;
        if current.state == "promoted" && expires_at_ms.is_none() {
            return Ok(current);
        }
        if current.state == "promoted" {
            return Err(invalid("branch_id", "A promoted branch has no TTL"));
        }
        if current.state == "expiring"
            || current.state == "expired"
            || current.expires_at_ms <= now_ms
        {
            return Err(request(ErrorCode::BranchExpired));
        }
        if current.state != "active" {
            return Err(request(ErrorCode::RepositoryBusy));
        }
        let state = if expires_at_ms.is_some() {
            "active"
        } else {
            "promoted"
        };
        let expiry = expires_at_ms
            .unwrap_or(current.expires_at_ms)
            .max(current.expires_at_ms);
        sqlx::query("UPDATE branches SET state = ?, expires_at_ms = ?, updated_at_ms = ?, updated_by = ?, updated_by_kind = ? WHERE tenant_id = ? AND id = ?")
            .bind(state).bind(expiry).bind(now_ms).bind(principal.actor.identity_id.as_str()).bind(kind_name(principal.actor.kind)).bind(principal.tenant_id.as_str()).bind(id.as_str()).execute(&mut *tx).await?;
        let row = sqlx::query("SELECT * FROM branches WHERE tenant_id = ? AND id = ?")
            .bind(principal.tenant_id.as_str())
            .bind(id.as_str())
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        branch(&row)
    }

    async fn pending_branches(
        &self,
        tenant: &TenantId,
        repo: &RepositoryId,
        now_ms: i64,
    ) -> StoreResult<Vec<BranchRecord>> {
        let rows = sqlx::query("SELECT * FROM branches WHERE tenant_id = ? AND repository_id = ? AND (state IN ('creating', 'expiring') OR (state = 'active' AND expires_at_ms <= ?)) ORDER BY rowid")
            .bind(tenant.as_str()).bind(repo.as_str()).bind(now_ms).fetch_all(&self.readers).await?;
        rows.iter().map(branch).collect()
    }

    async fn begin_branch_expiry(
        &self,
        tenant: &TenantId,
        id: &BranchId,
        now_ms: i64,
    ) -> StoreResult<()> {
        sqlx::query("UPDATE branches SET state = 'expiring', updated_at_ms = ?, updated_by = 'idn_00000000000000000000000000', updated_by_kind = 'system' WHERE tenant_id = ? AND id = ? AND state = 'active' AND expires_at_ms <= ?")
            .bind(now_ms).bind(tenant.as_str()).bind(id.as_str()).bind(now_ms).execute(&self.writer).await?;
        Ok(())
    }

    async fn finish_branch_expiry(
        &self,
        tenant: &TenantId,
        id: &BranchId,
        now_ms: i64,
    ) -> StoreResult<()> {
        let mut tx = self.writer.begin().await?;
        sqlx::query("INSERT OR IGNORE INTO expiries (tenant_id, branch_id, repository_id, recorded_at_ms, created_by, created_by_kind, updated_at_ms, updated_by, updated_by_kind) SELECT tenant_id, id, repository_id, ?, 'idn_00000000000000000000000000', 'system', ?, 'idn_00000000000000000000000000', 'system' FROM branches WHERE tenant_id = ? AND id = ? AND state = 'expiring'")
            .bind(now_ms).bind(now_ms).bind(tenant.as_str()).bind(id.as_str()).execute(&mut *tx).await?;
        sqlx::query("UPDATE branches SET state = 'expired', updated_at_ms = ?, updated_by = 'idn_00000000000000000000000000', updated_by_kind = 'system' WHERE tenant_id = ? AND id = ? AND state = 'expiring'")
            .bind(now_ms).bind(tenant.as_str()).bind(id.as_str()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn expired_branch_names(
        &self,
        tenant: &TenantId,
        repo: &RepositoryId,
    ) -> StoreResult<Vec<String>> {
        Ok(sqlx::query_scalar("SELECT DISTINCT b.name FROM branches b JOIN expiries e ON e.tenant_id = b.tenant_id AND e.branch_id = b.id WHERE b.tenant_id = ? AND b.repository_id = ? AND e.reclaimed_at_ms IS NULL AND NOT EXISTS(SELECT 1 FROM branches newer WHERE newer.tenant_id = b.tenant_id AND newer.repository_id = b.repository_id AND newer.name = b.name AND newer.rowid > b.rowid AND newer.state IN ('creating', 'active', 'promoted'))")
            .bind(tenant.as_str()).bind(repo.as_str()).fetch_all(&self.readers).await?)
    }

    async fn deployment_lifecycle_repositories(
        &self,
        now_ms: i64,
    ) -> StoreResult<Vec<RepositoryRecord>> {
        let rows = sqlx::query("SELECT * FROM repositories r WHERE r.state = 'ready' AND (EXISTS(SELECT 1 FROM branches b WHERE b.tenant_id = r.tenant_id AND b.repository_id = r.id AND b.state IN ('creating', 'active', 'expiring')) OR EXISTS(SELECT 1 FROM expiries e WHERE e.tenant_id = r.tenant_id AND e.repository_id = r.id AND e.reclaimed_at_ms IS NULL) OR EXISTS(SELECT 1 FROM compactions c WHERE c.tenant_id = r.tenant_id AND c.repository_id = r.id AND (c.completed_at_ms IS NULL OR r.orphan_sweep_after_ms <= ?)))")
            .bind(now_ms).fetch_all(&self.readers).await?;
        rows.iter().map(repository).collect()
    }
}
