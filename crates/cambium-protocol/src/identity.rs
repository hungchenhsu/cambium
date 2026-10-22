use serde::{Deserialize, Serialize};

use crate::{ApiError, ErrorCode, IdentityId, RepositoryId, TenantId, TokenId};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityKind {
    Human,
    Agent,
    System,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub enum Scope {
    #[serde(rename = "repo:read")]
    RepoRead,
    #[serde(rename = "repo:write")]
    RepoWrite,
    #[serde(rename = "repo:admin")]
    RepoAdmin,
    #[serde(rename = "tenant:admin")]
    TenantAdmin,
}

impl Scope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RepoRead => "repo:read",
            Self::RepoWrite => "repo:write",
            Self::RepoAdmin => "repo:admin",
            Self::TenantAdmin => "tenant:admin",
        }
    }

    pub fn includes(self, other: Self) -> bool {
        self >= other
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Actor {
    pub identity_id: IdentityId,
    pub kind: IdentityKind,
}

/// Constructed by authentication, never deserialized from a request.
#[derive(Clone, Debug)]
pub struct Principal {
    pub tenant_id: TenantId,
    pub token_id: TokenId,
    pub actor: Actor,
    pub scope: Scope,
    /// None allows all repositories; Some(empty) allows none.
    pub repositories: Option<Vec<RepositoryId>>,
    pub expires_at_ms: i64,
}

impl Principal {
    pub fn require(&self, scope: Scope) -> Result<(), ApiError> {
        if self.scope.includes(scope) {
            Ok(())
        } else {
            Err(ApiError::new(ErrorCode::PermissionDenied))
        }
    }

    pub fn can_see(&self, repository: &RepositoryId) -> bool {
        self.repositories
            .as_ref()
            .is_none_or(|allowed| allowed.contains(repository))
    }

    pub fn require_repository(
        &self,
        repository: &RepositoryId,
        scope: Scope,
    ) -> Result<(), ApiError> {
        if !self.can_see(repository) {
            return Err(ApiError::new(ErrorCode::NotFound));
        }
        self.require(scope)
    }
}
