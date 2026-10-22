use super::*;
use crate::compaction::{CompactionRecord, CompactionStore, CopyToken, ExpirySummary};
use cambium_protocol::{BranchId, CompactionId};
use serde_json::json;

fn compaction(row: &SqliteRow) -> StoreResult<CompactionRecord> {
    Ok(CompactionRecord {
        tenant_id: parse(row.try_get("tenant_id")?)?,
        id: parse(row.try_get("id")?)?,
        repository_id: parse(row.try_get("repository_id")?)?,
        old_generation: row.try_get("old_generation")?,
        new_generation: row.try_get("new_generation")?,
        stage: row.try_get("stage")?,
        state: serde_json::from_str(&row.try_get::<String, _>("state_json")?)
            .map_err(|_| StoreError::Corrupt)?,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
        completed_at_ms: row.try_get("completed_at_ms")?,
    })
}
fn copy_token(row: &SqliteRow) -> StoreResult<CopyToken> {
    Ok(CopyToken {
        tenant_id: parse(row.try_get("tenant_id")?)?,
        id: parse(row.try_get("id")?)?,
        compaction_id: parse(row.try_get("compaction_id")?)?,
        repository_id: parse(row.try_get("repository_id")?)?,
        destination_backend_id: row.try_get("destination_backend_id")?,
        expires_at_ms: row.try_get("expires_at_ms")?,
    })
}
const COPY_SELECT: &str = "SELECT t.*, c.repository_id FROM internal_tokens t JOIN compactions c ON c.tenant_id = t.tenant_id AND c.id = t.compaction_id JOIN repositories r ON r.tenant_id = c.tenant_id AND r.id = c.repository_id WHERE c.completed_at_ms IS NULL AND c.stage != 'aborting' AND r.state = 'ready' AND t.expires_at_ms > ?";

#[async_trait]
impl CompactionStore for SqliteStore {
    async fn expiry_summary(
        &self,
        tenant: &TenantId,
        repo: &RepositoryId,
    ) -> StoreResult<ExpirySummary> {
        let row = sqlx::query("SELECT COUNT(*) AS count, MIN(recorded_at_ms) AS oldest FROM expiries WHERE tenant_id = ? AND repository_id = ? AND reclaimed_at_ms IS NULL")
            .bind(tenant.as_str()).bind(repo.as_str()).fetch_one(&self.readers).await?;
        Ok(ExpirySummary {
            count: row.try_get("count")?,
            oldest_at_ms: row.try_get("oldest")?,
        })
    }

    async fn start_compaction(
        &self,
        tenant: &TenantId,
        repo: &RepositoryId,
        principal: Option<&Principal>,
        requested_id: Option<&CompactionId>,
        now_ms: i64,
    ) -> StoreResult<CompactionRecord> {
        let mut tx = self.writer.begin().await?;
        let principal = match principal {
            Some(p) => {
                if &p.tenant_id != tenant {
                    return Err(request(ErrorCode::NotFound));
                }
                let (p, _) = self.resolve(&mut tx, tenant, &p.token_id, now_ms).await?;
                p.require_repository(repo, Scope::RepoAdmin)?;
                Some(p)
            }
            None => None,
        };
        let row = sqlx::query(
            "SELECT * FROM repositories WHERE tenant_id = ? AND id = ? AND state = 'ready'",
        )
        .bind(tenant.as_str())
        .bind(repo.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| request(ErrorCode::NotFound))?;
        let repository = repository(&row)?;
        if let Some(id) = requested_id
            && let Some(row) =
                sqlx::query("SELECT * FROM compactions WHERE tenant_id = ? AND id = ?")
                    .bind(tenant.as_str())
                    .bind(id.as_str())
                    .fetch_optional(&mut *tx)
                    .await?
        {
            let record = compaction(&row)?;
            if &record.repository_id != repo {
                return Err(StoreError::Corrupt);
            }
            return Ok(record);
        }
        if let Some(row) = sqlx::query("SELECT * FROM compactions WHERE tenant_id = ? AND repository_id = ? AND completed_at_ms IS NULL")
            .bind(tenant.as_str()).bind(repo.as_str()).fetch_optional(&mut *tx).await? { return compaction(&row); }
        if repository.fenced {
            return Err(request(ErrorCode::RepositoryBusy));
        }
        let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM branches WHERE tenant_id = ? AND repository_id = ? AND state = 'creating'").bind(tenant.as_str()).bind(repo.as_str()).fetch_one(&mut *tx).await?;
        if pending > 0 {
            return Err(request(ErrorCode::RepositoryBusy));
        }
        let id = requested_id.cloned().unwrap_or_default();
        let next = repository
            .next_generation
            .checked_add(1)
            .ok_or(StoreError::Corrupt)?;
        let backend = format!("{tenant}/{repo}/g{}", repository.next_generation);
        let actor_id = principal
            .as_ref()
            .map(|p| p.actor.identity_id.as_str())
            .unwrap_or("idn_00000000000000000000000000");
        let actor_kind = principal
            .as_ref()
            .map(|p| kind_name(p.actor.kind))
            .unwrap_or("system");
        sqlx::query("INSERT INTO generations (tenant_id, repository_id, number, backend_id, state, created_at_ms, created_by, created_by_kind, updated_at_ms, updated_by, updated_by_kind) VALUES (?, ?, ?, ?, 'building', ?, ?, ?, ?, ?, ?)")
            .bind(tenant.as_str()).bind(repo.as_str()).bind(repository.next_generation).bind(backend).bind(now_ms).bind(actor_id).bind(actor_kind).bind(now_ms).bind(actor_id).bind(actor_kind).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO compactions (tenant_id, id, repository_id, old_generation, new_generation, stage, state_json, created_at_ms, updated_at_ms, created_by, created_by_kind, updated_by, updated_by_kind) VALUES (?, ?, ?, ?, ?, 'preparing', '{}', ?, ?, ?, ?, ?, ?)")
            .bind(tenant.as_str()).bind(id.as_str()).bind(repo.as_str()).bind(repository.active_generation).bind(repository.next_generation).bind(now_ms).bind(now_ms).bind(actor_id).bind(actor_kind).bind(actor_id).bind(actor_kind).execute(&mut *tx).await?;
        sqlx::query("UPDATE repositories SET next_generation = ? WHERE tenant_id = ? AND id = ?")
            .bind(next)
            .bind(tenant.as_str())
            .bind(repo.as_str())
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query("SELECT * FROM compactions WHERE tenant_id = ? AND id = ?")
            .bind(tenant.as_str())
            .bind(id.as_str())
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        compaction(&row)
    }

    async fn running_compaction(
        &self,
        tenant: &TenantId,
        repo: &RepositoryId,
    ) -> StoreResult<Option<CompactionRecord>> {
        sqlx::query("SELECT * FROM compactions WHERE tenant_id = ? AND repository_id = ? AND completed_at_ms IS NULL")
            .bind(tenant.as_str()).bind(repo.as_str()).fetch_optional(&self.readers).await?.as_ref().map(compaction).transpose()
    }
    async fn get_compaction(
        &self,
        tenant: &TenantId,
        repo: &RepositoryId,
        id: &CompactionId,
    ) -> StoreResult<CompactionRecord> {
        let row = sqlx::query(
            "SELECT * FROM compactions WHERE tenant_id = ? AND repository_id = ? AND id = ?",
        )
        .bind(tenant.as_str())
        .bind(repo.as_str())
        .bind(id.as_str())
        .fetch_optional(&self.readers)
        .await?
        .ok_or_else(|| request(ErrorCode::NotFound))?;
        compaction(&row)
    }
    async fn latest_compaction(
        &self,
        tenant: &TenantId,
        repo: &RepositoryId,
    ) -> StoreResult<Option<CompactionRecord>> {
        sqlx::query("SELECT * FROM compactions WHERE tenant_id = ? AND repository_id = ? ORDER BY rowid DESC LIMIT 1")
            .bind(tenant.as_str()).bind(repo.as_str()).fetch_optional(&self.readers).await?.as_ref().map(compaction).transpose()
    }
    async fn save_compaction(&self, record: &CompactionRecord, now_ms: i64) -> StoreResult<()> {
        let mut tx = self.writer.begin().await?;
        let updated = sqlx::query("UPDATE compactions SET stage = ?, state_json = ?, updated_at_ms = ?, updated_by = 'idn_00000000000000000000000000', updated_by_kind = 'system' WHERE tenant_id = ? AND id = ? AND completed_at_ms IS NULL")
            .bind(&record.stage).bind(record.state.to_string()).bind(now_ms).bind(record.tenant_id.as_str()).bind(record.id.as_str()).execute(&mut *tx).await?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::Corrupt);
        }
        if record.stage == "aborting" {
            sqlx::query("UPDATE repositories SET fenced = 0 WHERE tenant_id = ? AND id = ? AND active_generation = ?")
                .bind(record.tenant_id.as_str()).bind(record.repository_id.as_str()).bind(record.old_generation).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM internal_tokens WHERE tenant_id = ? AND compaction_id = ?")
                .bind(record.tenant_id.as_str())
                .bind(record.id.as_str())
                .execute(&mut *tx)
                .await?;
        }
        if record.stage == "sealed" {
            sqlx::query("UPDATE generations SET state = 'sealed', updated_at_ms = ?, updated_by = 'idn_00000000000000000000000000', updated_by_kind = 'system' WHERE tenant_id = ? AND repository_id = ? AND number = ? AND state = 'draining'")
                .bind(now_ms).bind(record.tenant_id.as_str()).bind(record.repository_id.as_str()).bind(record.old_generation).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    async fn fence_compaction(
        &self,
        record: &CompactionRecord,
        now_ms: i64,
    ) -> StoreResult<CompactionRecord> {
        let mut tx = self.writer.begin().await?;
        let changed = sqlx::query("UPDATE repositories SET fenced = 1 WHERE tenant_id = ? AND id = ? AND state = 'ready' AND active_generation = ? AND NOT EXISTS(SELECT 1 FROM branches b WHERE b.tenant_id = repositories.tenant_id AND b.repository_id = repositories.id AND b.state = 'creating')")
            .bind(record.tenant_id.as_str()).bind(record.repository_id.as_str()).bind(record.old_generation).execute(&mut *tx).await?;
        if changed.rows_affected() != 1 {
            return Err(request(ErrorCode::RepositoryBusy));
        }
        let ids: Vec<String> = sqlx::query_scalar("SELECT branch_id FROM expiries WHERE tenant_id = ? AND repository_id = ? AND reclaimed_at_ms IS NULL ORDER BY branch_id").bind(record.tenant_id.as_str()).bind(record.repository_id.as_str()).fetch_all(&mut *tx).await?;
        let mut record = record.clone();
        record.stage = "catching_up".into();
        record.state["fenced_at_ms"] = json!(now_ms);
        record.state["expiry_ids"] = json!(ids);
        let owners = sqlx::query("SELECT name, id FROM branches WHERE tenant_id = ? AND repository_id = ? AND state = 'active'").bind(record.tenant_id.as_str()).bind(record.repository_id.as_str()).fetch_all(&mut *tx).await?;
        let mut branch_owners = serde_json::Map::new();
        for row in owners {
            branch_owners.insert(
                row.try_get::<String, _>("name")?,
                json!(row.try_get::<String, _>("id")?),
            );
        }
        record.state["branch_owners"] = serde_json::Value::Object(branch_owners);
        let excluded: Vec<String> = sqlx::query_scalar("SELECT DISTINCT b.name FROM branches b JOIN expiries e ON e.tenant_id = b.tenant_id AND e.branch_id = b.id WHERE b.tenant_id = ? AND b.repository_id = ? AND e.reclaimed_at_ms IS NULL AND NOT EXISTS(SELECT 1 FROM branches newer WHERE newer.tenant_id = b.tenant_id AND newer.repository_id = b.repository_id AND newer.name = b.name AND newer.rowid > b.rowid AND newer.state IN ('creating', 'active', 'promoted'))")
            .bind(record.tenant_id.as_str()).bind(record.repository_id.as_str()).fetch_all(&mut *tx).await?;
        record.state["excluded_names"] = json!(excluded);
        sqlx::query("UPDATE compactions SET stage = 'catching_up', state_json = ?, updated_at_ms = ?, updated_by = 'idn_00000000000000000000000000', updated_by_kind = 'system' WHERE tenant_id = ? AND id = ? AND completed_at_ms IS NULL")
            .bind(record.state.to_string()).bind(now_ms).bind(record.tenant_id.as_str()).bind(record.id.as_str()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(record)
    }
    async fn switch_generation(&self, record: &CompactionRecord, now_ms: i64) -> StoreResult<()> {
        let mut tx = self.writer.begin().await?;
        let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM branches WHERE tenant_id = ? AND repository_id = ? AND state = 'creating'").bind(record.tenant_id.as_str()).bind(record.repository_id.as_str()).fetch_one(&mut *tx).await?;
        if pending > 0 {
            return Err(request(ErrorCode::RepositoryBusy));
        }
        let changed = sqlx::query("UPDATE repositories SET active_generation = ?, fenced = 0, updated_at_ms = ? WHERE tenant_id = ? AND id = ? AND active_generation = ? AND state = 'ready' AND fenced = 1")
            .bind(record.new_generation).bind(now_ms).bind(record.tenant_id.as_str()).bind(record.repository_id.as_str()).bind(record.old_generation).execute(&mut *tx).await?;
        if changed.rows_affected() != 1 {
            return Err(StoreError::Corrupt);
        }
        for (number, state) in [
            (record.old_generation, "draining"),
            (record.new_generation, "active"),
        ] {
            sqlx::query("UPDATE generations SET state = ?, updated_at_ms = ?, updated_by = 'idn_00000000000000000000000000', updated_by_kind = 'system' WHERE tenant_id = ? AND repository_id = ? AND number = ?")
                .bind(state).bind(now_ms).bind(record.tenant_id.as_str()).bind(record.repository_id.as_str()).bind(number).execute(&mut *tx).await?;
        }
        let mut state = record.state.clone();
        state["switched_at_ms"] = json!(now_ms);
        state["last_reconciled_at_ms"] = json!(now_ms);
        let changed = sqlx::query("UPDATE compactions SET stage = 'draining', state_json = ?, updated_at_ms = ?, updated_by = 'idn_00000000000000000000000000', updated_by_kind = 'system' WHERE tenant_id = ? AND id = ? AND stage = 'verifying' AND completed_at_ms IS NULL")
            .bind(state.to_string()).bind(now_ms).bind(record.tenant_id.as_str()).bind(record.id.as_str()).execute(&mut *tx).await?;
        if changed.rows_affected() != 1 {
            return Err(StoreError::Corrupt);
        }
        tx.commit().await?;
        Ok(())
    }
    async fn finish_compaction(
        &self,
        record: &CompactionRecord,
        aborted: bool,
        now_ms: i64,
    ) -> StoreResult<()> {
        let mut tx = self.writer.begin().await?;
        let row = sqlx::query("SELECT * FROM compactions WHERE tenant_id = ? AND repository_id = ? AND id = ? AND completed_at_ms IS NULL")
            .bind(record.tenant_id.as_str()).bind(record.repository_id.as_str()).bind(record.id.as_str()).fetch_optional(&mut *tx).await?.ok_or(StoreError::Corrupt)?;
        let stored = compaction(&row)?;
        if stored.old_generation != record.old_generation
            || stored.new_generation != record.new_generation
            || stored.stage != if aborted { "aborting" } else { "deleting" }
        {
            return Err(StoreError::Corrupt);
        }
        let target = if aborted {
            record.new_generation
        } else {
            record.old_generation
        };
        let active: i64 = sqlx::query_scalar(
            "SELECT active_generation FROM repositories WHERE tenant_id = ? AND id = ?",
        )
        .bind(record.tenant_id.as_str())
        .bind(record.repository_id.as_str())
        .fetch_one(&mut *tx)
        .await?;
        if active == target {
            return Err(StoreError::Corrupt);
        }
        sqlx::query("UPDATE generations SET state = 'deleted', updated_at_ms = ?, updated_by = 'idn_00000000000000000000000000', updated_by_kind = 'system' WHERE tenant_id = ? AND repository_id = ? AND number = ?")
            .bind(now_ms).bind(record.tenant_id.as_str()).bind(record.repository_id.as_str()).bind(target).execute(&mut *tx).await?;
        if !aborted {
            let ids: Vec<BranchId> = serde_json::from_value(stored.state["expiry_ids"].clone())
                .map_err(|_| StoreError::Corrupt)?;
            for id in ids {
                sqlx::query("UPDATE expiries SET reclaimed_at_ms = ?, updated_at_ms = ?, updated_by = 'idn_00000000000000000000000000', updated_by_kind = 'system' WHERE tenant_id = ? AND repository_id = ? AND branch_id = ? AND reclaimed_at_ms IS NULL")
                    .bind(now_ms).bind(now_ms).bind(record.tenant_id.as_str()).bind(record.repository_id.as_str()).bind(id.as_str()).execute(&mut *tx).await?;
            }
        }
        sqlx::query("UPDATE compactions SET stage = ?, completed_at_ms = ?, updated_at_ms = ?, updated_by = 'idn_00000000000000000000000000', updated_by_kind = 'system' WHERE tenant_id = ? AND id = ? AND completed_at_ms IS NULL")
            .bind(if aborted { "aborted" } else { "complete" }).bind(now_ms).bind(now_ms).bind(record.tenant_id.as_str()).bind(record.id.as_str()).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM internal_tokens WHERE tenant_id = ? AND compaction_id = ?")
            .bind(record.tenant_id.as_str())
            .bind(record.id.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
    async fn deployment_recover_compactions(&self, now_ms: i64) -> StoreResult<()> {
        let mut tx = self.writer.begin().await?;
        sqlx::query("DELETE FROM internal_tokens")
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE repositories SET fenced = 0 WHERE EXISTS(SELECT 1 FROM compactions c WHERE c.tenant_id = repositories.tenant_id AND c.repository_id = repositories.id AND c.old_generation = repositories.active_generation AND c.stage IN ('preparing', 'prepare_copying', 'prepared', 'catching_up', 'catchup_copying', 'verifying', 'aborting') AND c.completed_at_ms IS NULL)").execute(&mut *tx).await?;
        sqlx::query("UPDATE compactions SET stage = 'aborting', updated_at_ms = ?, updated_by = 'idn_00000000000000000000000000', updated_by_kind = 'system' WHERE completed_at_ms IS NULL AND stage IN ('preparing', 'prepare_copying', 'prepared', 'catching_up', 'catchup_copying', 'verifying')").bind(now_ms).execute(&mut *tx).await?;
        sqlx::query("UPDATE compactions SET state_json = json_set(state_json, '$.needs_cleanup', json('true')) WHERE completed_at_ms IS NULL").execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    async fn claim_orphan_sweep(
        &self,
        tenant: &TenantId,
        repo: &RepositoryId,
        now_ms: i64,
        next_ms: i64,
    ) -> StoreResult<bool> {
        let result = sqlx::query("UPDATE repositories SET orphan_sweep_after_ms = ?, updated_at_ms = ?, updated_by = 'idn_00000000000000000000000000', updated_by_kind = 'system' WHERE tenant_id = ? AND id = ? AND state = 'ready' AND orphan_sweep_after_ms <= ? AND EXISTS(SELECT 1 FROM compactions c WHERE c.tenant_id = repositories.tenant_id AND c.repository_id = repositories.id AND c.completed_at_ms IS NOT NULL)")
            .bind(next_ms).bind(now_ms).bind(tenant.as_str()).bind(repo.as_str()).bind(now_ms).execute(&self.writer).await?;
        Ok(result.rows_affected() == 1)
    }
    async fn mint_copy_token(
        &self,
        destination: &str,
        hash: TokenHash,
        now_ms: i64,
    ) -> StoreResult<CopyToken> {
        let mut tx = self.writer.begin().await?;
        let row = sqlx::query("SELECT c.* FROM compactions c JOIN generations g ON g.tenant_id = c.tenant_id AND g.repository_id = c.repository_id AND g.number = c.new_generation JOIN repositories r ON r.tenant_id = c.tenant_id AND r.id = c.repository_id WHERE g.backend_id = ? AND c.completed_at_ms IS NULL AND c.stage != 'aborting' AND r.state = 'ready'").bind(destination).fetch_optional(&mut *tx).await?.ok_or_else(|| request(ErrorCode::PermissionDenied))?;
        let c = compaction(&row)?;
        let id = TokenId::new();
        let expiry = now_ms.checked_add(120_000).ok_or(StoreError::Corrupt)?;
        sqlx::query("INSERT INTO internal_tokens (tenant_id, id, compaction_id, destination_backend_id, secret_hash, created_at_ms, expires_at_ms, created_by, created_by_kind) VALUES (?, ?, ?, ?, ?, ?, ?, 'idn_00000000000000000000000000', 'system')")
            .bind(c.tenant_id.as_str()).bind(id.as_str()).bind(c.id.as_str()).bind(destination).bind(hash.0.as_slice()).bind(now_ms).bind(expiry).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(CopyToken {
            tenant_id: c.tenant_id,
            id,
            compaction_id: c.id,
            repository_id: c.repository_id,
            destination_backend_id: destination.into(),
            expires_at_ms: expiry,
        })
    }
    async fn authenticate_copy_token(
        &self,
        hash: TokenHash,
        now_ms: i64,
    ) -> StoreResult<CopyToken> {
        let rows = sqlx::query(&format!("{COPY_SELECT} AND t.secret_hash = ?"))
            .bind(now_ms)
            .bind(hash.0.as_slice())
            .fetch_all(&self.readers)
            .await?;
        if rows.len() != 1 {
            return Err(request(ErrorCode::Unauthenticated));
        }
        copy_token(&rows[0])
    }
    async fn revalidate_copy_token(
        &self,
        tenant: &TenantId,
        id: &TokenId,
        now_ms: i64,
    ) -> StoreResult<CopyToken> {
        let row = sqlx::query(&format!("{COPY_SELECT} AND t.tenant_id = ? AND t.id = ?"))
            .bind(now_ms)
            .bind(tenant.as_str())
            .bind(id.as_str())
            .fetch_optional(&self.readers)
            .await?
            .ok_or_else(|| request(ErrorCode::Unauthenticated))?;
        copy_token(&row)
    }
    async fn revoke_copy_token(&self, hash: TokenHash) -> StoreResult<()> {
        sqlx::query("DELETE FROM internal_tokens WHERE secret_hash = ?")
            .bind(hash.0.as_slice())
            .execute(&self.writer)
            .await?;
        Ok(())
    }
}
