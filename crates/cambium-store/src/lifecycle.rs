//! Tenant-scoped lifecycle records. Deployment scans are restricted to the lock owner.

use async_trait::async_trait;
use cambium_protocol::{Actor, BranchId, ObjectId, Principal, RepositoryId, TenantId};
use serde::{Deserialize, Serialize};

use crate::{RepositoryRecord, StoreResult, operations::OperationsStore};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BranchCreation {
    pub id: BranchId,
    pub repository_id: RepositoryId,
    pub name: String,
    pub start_commit: ObjectId,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BranchRecord {
    pub tenant_id: TenantId,
    pub id: BranchId,
    pub repository_id: RepositoryId,
    pub name: String,
    pub start_commit: ObjectId,
    pub creation_backend_id: String,
    pub expires_at_ms: i64,
    pub state: String,
    pub created_at_ms: i64,
    pub created_by: Actor,
    pub last_activity_at_ms: Option<i64>,
}

#[async_trait]
pub trait LifecycleStore: OperationsStore {
    async fn reserve_branch(
        &self,
        principal: &Principal,
        input: &BranchCreation,
        now_ms: i64,
    ) -> StoreResult<BranchRecord>;
    async fn finish_branch_creation(
        &self,
        tenant: &TenantId,
        id: &BranchId,
        created: bool,
        now_ms: i64,
    ) -> StoreResult<()>;
    async fn get_branch(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
        id: &BranchId,
    ) -> StoreResult<BranchRecord>;
    async fn list_branches(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
        after: Option<&BranchId>,
        limit: u32,
    ) -> StoreResult<Vec<BranchRecord>>;
    async fn change_branch(
        &self,
        principal: &Principal,
        repository: &RepositoryId,
        id: &BranchId,
        expires_at_ms: Option<i64>,
        now_ms: i64,
    ) -> StoreResult<BranchRecord>;
    async fn pending_branches(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
        now_ms: i64,
    ) -> StoreResult<Vec<BranchRecord>>;
    async fn begin_branch_expiry(
        &self,
        tenant: &TenantId,
        id: &BranchId,
        now_ms: i64,
    ) -> StoreResult<()>;
    async fn finish_branch_expiry(
        &self,
        tenant: &TenantId,
        id: &BranchId,
        now_ms: i64,
    ) -> StoreResult<()>;
    async fn expired_branch_names(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
    ) -> StoreResult<Vec<String>>;
    async fn deployment_lifecycle_repositories(&self) -> StoreResult<Vec<RepositoryRecord>>;
}
