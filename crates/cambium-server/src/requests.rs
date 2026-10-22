//! HTTP and MCP decode the same strict request shapes.

use cambium_backend::FileMode;
use cambium_protocol::{
    BranchId, CompactionId, IdentityId, IdentityKind, ObjectId, RepositoryId, Scope, TokenId,
};
use serde::{Deserialize, Serialize};

fn main_branch() -> String {
    "main".into()
}
fn head() -> String {
    "HEAD".into()
}
fn limit() -> u32 {
    100
}
fn yes() -> bool {
    true
}
fn hour() -> u64 {
    3600
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(tag = "operation", content = "input", rename_all = "snake_case")]
pub enum Operation {
    CreateRepository(CreateRepository),
    GetRepository(Repository),
    ListRepositories(ListRepositories),
    DeleteRepository(Repository),
    RequestCompaction(Repository),
    GetReclamation(GetReclamation),
    CreateEphemeralBranch(CreateEphemeralBranch),
    GetBranch(Branch),
    ListBranches(ListBranches),
    ExtendBranch(ExtendBranch),
    PromoteBranch(Branch),
    WriteChangeset(WriteChangeset),
    ReadFile(ReadFile),
    ListTree(ListTree),
    Diff(Diff),
    CreateIdentity(CreateIdentity),
    GetIdentity(Identity),
    MintToken(MintToken),
    RevokeToken(Token),
    DisableIdentity(Identity),
}

impl Operation {
    pub fn repository(&self) -> Option<&RepositoryId> {
        match self {
            Self::GetRepository(v) | Self::DeleteRepository(v) | Self::RequestCompaction(v) => {
                Some(&v.repository_id)
            }
            Self::GetReclamation(v) => Some(&v.repository_id),
            Self::CreateEphemeralBranch(v) => Some(&v.repository_id),
            Self::GetBranch(v) | Self::PromoteBranch(v) => Some(&v.repository_id),
            Self::ListBranches(v) => Some(&v.repository_id),
            Self::ExtendBranch(v) => Some(&v.repository_id),
            Self::WriteChangeset(v) => Some(&v.repository_id),
            Self::ReadFile(v) => Some(&v.repository_id),
            Self::ListTree(v) => Some(&v.repository_id),
            Self::Diff(v) => Some(&v.repository_id),
            _ => None,
        }
    }
    pub fn scope(&self) -> Scope {
        match self {
            Self::CreateRepository(_) | Self::DeleteRepository(_) | Self::RequestCompaction(_) => {
                Scope::RepoAdmin
            }
            Self::WriteChangeset(_)
            | Self::CreateEphemeralBranch(_)
            | Self::ExtendBranch(_)
            | Self::PromoteBranch(_) => Scope::RepoWrite,
            Self::CreateIdentity(_)
            | Self::GetIdentity(_)
            | Self::RevokeToken(_)
            | Self::DisableIdentity(_) => Scope::TenantAdmin,
            _ => Scope::RepoRead,
        }
    }
    pub fn mutating(&self) -> bool {
        !matches!(
            self,
            Self::GetRepository(_)
                | Self::ListRepositories(_)
                | Self::ReadFile(_)
                | Self::ListTree(_)
                | Self::Diff(_)
                | Self::GetIdentity(_)
                | Self::GetReclamation(_)
                | Self::GetBranch(_)
                | Self::ListBranches(_)
        )
    }
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateRepository {
    pub name: String,
    #[serde(default = "main_branch")]
    pub default_branch: String,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    pub repository_id: RepositoryId,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListRepositories {
    #[serde(default = "limit")]
    pub limit: u32,
    pub page_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(
    tag = "kind",
    content = "object_id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ExpectedTip {
    Absent,
    At(ObjectId),
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteChangeset {
    pub repository_id: RepositoryId,
    #[serde(default = "main_branch")]
    pub branch: String,
    pub expected: Option<ExpectedTip>,
    pub message: String,
    pub author: Option<GitIdentity>,
    pub committer: Option<GitIdentity>,
    pub changes: Vec<FileChange>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GitIdentity {
    pub name: String,
    pub email: String,
    pub time: String,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileChange {
    Put {
        path: Option<String>,
        path_b64: Option<String>,
        contents: Option<String>,
        contents_b64: Option<String>,
        mode: Option<FileMode>,
    },
    Delete {
        path: Option<String>,
        path_b64: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadFile {
    pub repository_id: RepositoryId,
    #[serde(default = "head")]
    pub revision: String,
    pub path: Option<String>,
    pub path_b64: Option<String>,
    #[serde(default)]
    pub always_base64: bool,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListTree {
    pub repository_id: RepositoryId,
    #[serde(default = "head")]
    pub revision: String,
    #[serde(default = "yes")]
    pub recursive: bool,
    #[serde(default = "limit")]
    pub limit: u32,
    pub page_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Diff {
    pub repository_id: RepositoryId,
    pub from: String,
    pub to: String,
    #[serde(default = "limit")]
    pub limit: u32,
    pub page_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateIdentity {
    #[schemars(with = "PublicIdentityKind")]
    pub kind: IdentityKind,
    pub name: String,
    pub owner_id: Option<IdentityId>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub identity_id: IdentityId,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MintToken {
    pub identity_id: IdentityId,
    pub scope: Scope,
    pub repositories: Option<Vec<RepositoryId>>,
    #[serde(default = "hour")]
    pub ttl_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Token {
    pub token_id: TokenId,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateEphemeralBranch {
    pub repository_id: RepositoryId,
    pub name: String,
    pub start_commit: ObjectId,
    #[serde(default = "hour")]
    pub ttl_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Branch {
    pub repository_id: RepositoryId,
    pub branch_id: BranchId,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtendBranch {
    pub repository_id: RepositoryId,
    pub branch_id: BranchId,
    pub ttl_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListBranches {
    pub repository_id: RepositoryId,
    #[serde(default = "limit")]
    pub limit: u32,
    pub page_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetReclamation {
    pub repository_id: RepositoryId,
    pub compaction_id: Option<CompactionId>,
    #[serde(default = "limit")]
    pub limit: u32,
    pub page_token: Option<String>,
}

/// System actors are internal and cannot be created through an operation.
#[derive(schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PublicIdentityKind {
    Human,
    Agent,
}

#[cfg(test)]
mod tests {
    #[test]
    fn create_identity_schema_only_advertises_public_kinds() {
        let schema = serde_json::to_value(schemars::schema_for!(super::CreateIdentity)).unwrap();
        assert_eq!(
            schema["$defs"]["PublicIdentityKind"]["enum"],
            serde_json::json!(["human", "agent"])
        );
    }
}
