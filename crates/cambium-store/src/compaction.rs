//! Durable generation transitions and short-lived, generation-scoped copy credentials.

use async_trait::async_trait;
use cambium_protocol::{CompactionId, Principal, RepositoryId, TenantId, TokenId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{StoreResult, TokenHash, lifecycle::LifecycleStore};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompactionRecord {
    pub tenant_id: TenantId,
    pub id: CompactionId,
    pub repository_id: RepositoryId,
    pub old_generation: i64,
    pub new_generation: i64,
    pub stage: String,
    pub state: Value,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}
impl CompactionRecord {
    pub fn old_backend_id(&self) -> String {
        format!(
            "{}/{}/g{}",
            self.tenant_id, self.repository_id, self.old_generation
        )
    }
    pub fn new_backend_id(&self) -> String {
        format!(
            "{}/{}/g{}",
            self.tenant_id, self.repository_id, self.new_generation
        )
    }
}

#[derive(Clone, Debug)]
pub struct ExpirySummary {
    pub count: i64,
    pub oldest_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CopyToken {
    pub tenant_id: TenantId,
    pub id: TokenId,
    pub compaction_id: CompactionId,
    pub repository_id: RepositoryId,
    pub destination_backend_id: String,
    pub expires_at_ms: i64,
}

#[async_trait]
pub trait CompactionStore: LifecycleStore {
    async fn expiry_summary(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
    ) -> StoreResult<ExpirySummary>;
    async fn start_compaction(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
        principal: Option<&Principal>,
        requested_id: Option<&CompactionId>,
        now_ms: i64,
    ) -> StoreResult<CompactionRecord>;
    async fn running_compaction(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
    ) -> StoreResult<Option<CompactionRecord>>;
    async fn get_compaction(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
        id: &CompactionId,
    ) -> StoreResult<CompactionRecord>;
    async fn latest_compaction(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
    ) -> StoreResult<Option<CompactionRecord>>;
    async fn save_compaction(&self, record: &CompactionRecord, now_ms: i64) -> StoreResult<()>;
    async fn fence_compaction(
        &self,
        record: &CompactionRecord,
        now_ms: i64,
    ) -> StoreResult<CompactionRecord>;
    async fn switch_generation(&self, record: &CompactionRecord, now_ms: i64) -> StoreResult<()>;
    async fn finish_compaction(
        &self,
        record: &CompactionRecord,
        aborted: bool,
        now_ms: i64,
    ) -> StoreResult<()>;
    /// Marks pre-switch work aborted and revokes copies; only before accepting requests.
    async fn deployment_recover_compactions(&self, now_ms: i64) -> StoreResult<()>;
    async fn completed_compactions(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
    ) -> StoreResult<Vec<CompactionRecord>>;
    async fn mint_copy_token(
        &self,
        destination: &str,
        hash: TokenHash,
        now_ms: i64,
    ) -> StoreResult<CopyToken>;
    async fn authenticate_copy_token(&self, hash: TokenHash, now_ms: i64)
    -> StoreResult<CopyToken>;
    async fn revalidate_copy_token(
        &self,
        tenant: &TenantId,
        id: &TokenId,
        now_ms: i64,
    ) -> StoreResult<CopyToken>;
    async fn revoke_copy_token(&self, hash: TokenHash) -> StoreResult<()>;
}
