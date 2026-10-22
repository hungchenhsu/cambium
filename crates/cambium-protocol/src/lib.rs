//! Shared, locale-independent API contracts (ADR-0004 and ADR-0012).

pub mod bytes;
pub mod errors;
pub mod identity;
pub mod ids;

pub use errors::{ApiError, ErrorCode, ErrorResponse};
pub use identity::{Actor, IdentityKind, Principal, Scope};
pub use ids::{
    BranchId, CompactionId, IdentityId, ObjectId, RepositoryId, RequestId, TenantId, TokenId,
};
