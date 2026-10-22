//! Durable work reservations. Response payloads are opaque encrypted envelopes.

use async_trait::async_trait;
use cambium_protocol::{IdentityId, Principal, RepositoryId, RequestId, TenantId, TokenId};
use serde_json::Value;

use crate::{MintToken, RepositoryRecord, Store, StoreResult};

#[derive(Clone, Debug)]
pub enum Claim {
    Run {
        run_id: RequestId,
        reservation: Option<Value>,
    },
    Replay {
        status: u16,
        encrypted_response: String,
    },
}

#[derive(Clone, Debug)]
pub struct OperationKey {
    pub key: String,
    pub fingerprint: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct Completion {
    pub status: u16,
    pub encrypted_response: String,
}

#[async_trait]
pub trait OperationsStore: Store {
    async fn revalidate(
        &self,
        tenant: &TenantId,
        token: &TokenId,
        now_ms: i64,
    ) -> StoreResult<Principal>;
    async fn tenant_slug(&self, tenant: &TenantId) -> StoreResult<String>;
    async fn check_mint_token(
        &self,
        principal: &Principal,
        request: &MintToken,
        now_ms: i64,
    ) -> StoreResult<()>;
    async fn token_identity(&self, tenant: &TenantId, token: &TokenId) -> StoreResult<IdentityId>;
    async fn get_repository_including_deleted(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
    ) -> StoreResult<RepositoryRecord>;
    async fn finish_repository_creation(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
        now_ms: i64,
    ) -> StoreResult<()>;
    async fn find_repository(&self, tenant: &TenantId, name: &str)
    -> StoreResult<RepositoryRecord>;
    async fn list_repositories(
        &self,
        tenant: &TenantId,
        after: Option<&RepositoryId>,
        limit: u32,
    ) -> StoreResult<Vec<RepositoryRecord>>;
    async fn begin_repository_deletion(
        &self,
        principal: &Principal,
        id: &RepositoryId,
        now_ms: i64,
    ) -> StoreResult<RepositoryRecord>;
    async fn generation_ids(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
    ) -> StoreResult<Vec<String>>;
    async fn finish_repository_deletion(
        &self,
        tenant: &TenantId,
        repository: &RepositoryId,
        now_ms: i64,
    ) -> StoreResult<()>;
    /// Called only by the single process that owns the deployment lock.
    async fn deployment_pending_repositories(&self) -> StoreResult<Vec<RepositoryRecord>>;
    async fn deployment_recover_operations(&self, now_ms: i64) -> StoreResult<()>;
    async fn claim_operation(
        &self,
        principal: &Principal,
        key: &OperationKey,
        now_ms: i64,
    ) -> StoreResult<Claim>;
    async fn reserve_operation(
        &self,
        principal: &Principal,
        key: &OperationKey,
        run_id: &RequestId,
        reservation: &Value,
        now_ms: i64,
    ) -> StoreResult<()>;
    async fn finish_operation(
        &self,
        principal: &Principal,
        key: &OperationKey,
        run_id: &RequestId,
        completion: Option<&Completion>,
        now_ms: i64,
    ) -> StoreResult<()>;
}
