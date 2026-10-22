//! Tenant-scoped metadata, token authorization, and durable operation state.

use async_trait::async_trait;
use cambium_protocol::{
    Actor, ApiError, ErrorCode, IdentityId, IdentityKind, Principal, RepositoryId, Scope, TenantId,
    TokenId,
};
use serde::{Deserialize, Serialize};

pub mod operations;
pub mod sqlite;
pub use sqlite::SqliteStore;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Request(#[from] ApiError),
    #[error("metadata storage failed")]
    Database(#[from] sqlx::Error),
    #[error("metadata migration failed")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("invalid stored metadata")]
    Corrupt,
}

impl StoreError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Request(error) => error.code,
            _ => ErrorCode::Internal,
        }
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Clone, Copy)]
pub struct TokenHash(pub [u8; 32]);

impl std::fmt::Debug for TokenHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenHash([redacted])")
    }
}

#[derive(Clone, Debug)]
pub struct TokenSettings {
    pub human_max_lifetime_ms: i64,
    pub agent_max_lifetime_ms: i64,
    pub max_delegation_depth: usize,
}

impl Default for TokenSettings {
    fn default() -> Self {
        Self {
            human_max_lifetime_ms: 30 * 24 * 60 * 60 * 1000,
            agent_max_lifetime_ms: 24 * 60 * 60 * 1000,
            max_delegation_depth: 64,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityRecord {
    pub tenant_id: TenantId,
    pub id: IdentityId,
    pub kind: IdentityKind,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<IdentityId>,
    pub created_at_ms: i64,
    pub created_by: Actor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_at_ms: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct Bootstrap {
    pub tenant_slug: String,
    pub human_name: String,
    pub token_hash: TokenHash,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug)]
pub struct CreateIdentity {
    pub kind: IdentityKind,
    pub name: String,
    pub owner_id: Option<IdentityId>,
}

#[derive(Clone, Debug)]
pub struct MintToken {
    pub identity_id: IdentityId,
    pub token_hash: TokenHash,
    pub scope: Scope,
    pub repositories: Option<Vec<RepositoryId>>,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositoryRecord {
    pub tenant_id: TenantId,
    pub id: RepositoryId,
    pub name: String,
    pub default_branch: String,
    pub active_generation: i64,
    pub next_generation: i64,
    pub state: String,
    pub fenced: bool,
    pub created_at_ms: i64,
    pub created_by: Actor,
}

impl RepositoryRecord {
    pub fn backend_id(&self) -> String {
        format!("{}/{}/g{}", self.tenant_id, self.id, self.active_generation)
    }
}

/// Deployment methods are deliberately named separately from tenant operations.
#[async_trait]
pub trait Store: Send + Sync {
    async fn deployment_bootstrap(
        &self,
        request: &Bootstrap,
        now_ms: i64,
    ) -> StoreResult<Principal>;
    async fn deployment_authenticate(&self, hash: TokenHash, now_ms: i64)
    -> StoreResult<Principal>;
    async fn deployment_issue_human_token(
        &self,
        tenant: &TenantId,
        request: &MintToken,
        now_ms: i64,
    ) -> StoreResult<Principal>;
    async fn create_identity(
        &self,
        principal: &Principal,
        request: &CreateIdentity,
        now_ms: i64,
    ) -> StoreResult<IdentityRecord>;
    async fn get_identity(&self, tenant: &TenantId, id: &IdentityId)
    -> StoreResult<IdentityRecord>;
    async fn mint_token(
        &self,
        principal: &Principal,
        request: &MintToken,
        now_ms: i64,
    ) -> StoreResult<Principal>;
    async fn revoke_token(
        &self,
        principal: &Principal,
        token: &TokenId,
        now_ms: i64,
    ) -> StoreResult<()>;
    async fn disable_identity(
        &self,
        principal: &Principal,
        identity: &IdentityId,
        now_ms: i64,
    ) -> StoreResult<()>;
    async fn reserve_repository(
        &self,
        principal: &Principal,
        name: &str,
        default_branch: &str,
        now_ms: i64,
    ) -> StoreResult<RepositoryRecord>;
    async fn get_repository(
        &self,
        tenant: &TenantId,
        id: &RepositoryId,
    ) -> StoreResult<RepositoryRecord>;
    async fn mark_repository_ready(
        &self,
        principal: &Principal,
        id: &RepositoryId,
        now_ms: i64,
    ) -> StoreResult<()>;
}
