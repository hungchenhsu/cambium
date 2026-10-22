//! Backend and git policy contracts (ADR-0007 and ADR-0013).

use async_trait::async_trait;
use cambium_protocol::ObjectId;
use serde::{Deserialize, Serialize};

pub mod git_policy;

/// Opaque storage key. Tenant names, repository names, and credentials stay out.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BackendRepositoryId(String);

impl BackendRepositoryId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BackendRepositoryId {
    type Error = BackendError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = !value.is_empty()
            && value.len() <= 200
            && value.split('/').all(|part| {
                let bytes = part.as_bytes();
                bytes.first().is_some_and(u8::is_ascii_alphanumeric)
                    && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
                    && bytes
                        .iter()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(b))
            });
        if !valid {
            return Err(BackendError::InvalidRequest {
                field: "repository_id".into(),
                reason: "Invalid backend repository ID".into(),
            });
        }
        Ok(Self(value))
    }
}

impl From<BackendRepositoryId> for String {
    fn from(value: BackendRepositoryId) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectFormat {
    Sha1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capabilities {
    pub object_format: ObjectFormat,
    pub max_commit_request_bytes: u64,
    pub non_utf8_paths: bool,
    pub merge_commits: bool,
    pub atomic_ref_updates: bool,
    pub reclaims_unreachable_objects: bool,
    pub durable_seal: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectKind {
    Blob,
    Tree,
    Commit,
    Tag,
}

impl ObjectKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blob => "blob",
            Self::Tree => "tree",
            Self::Commit => "commit",
            Self::Tag => "tag",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawObject {
    pub kind: ObjectKind,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GitRef {
    pub name: String,
    pub object_id: ObjectId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RefSnapshot {
    pub head: String,
    pub refs: Vec<GitRef>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "object_id", rename_all = "snake_case")]
pub enum RefExpectation {
    Absent,
    At(ObjectId),
    Any,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RefUpdate {
    pub name: String,
    pub expected: RefExpectation,
    pub new: Option<ObjectId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefRejection {
    Stale,
    AlreadyExists,
    MissingObject,
    InvalidName,
    NotACommit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefOutcome {
    pub name: String,
    pub rejection: Option<RefRejection>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileMode {
    Regular,
    Executable,
    Symlink,
}

impl FileMode {
    pub const fn git_mode(self) -> u32 {
        match self {
            Self::Regular => 0o100644,
            Self::Executable => 0o100755,
            Self::Symlink => 0o120000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FileChange {
    Put {
        path: Vec<u8>,
        contents: Vec<u8>,
        mode: FileMode,
    },
    Delete {
        path: Vec<u8>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GitIdentity {
    pub name: String,
    pub email: String,
    pub timestamp_seconds: i64,
    pub utc_offset_seconds: i32,
}

/// Changeset writes always carry CAS, including an explicitly absent branch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommitRequest {
    pub branch: String,
    pub expected_parent: Option<ObjectId>,
    pub message: String,
    pub author: GitIdentity,
    pub committer: GitIdentity,
    pub changes: Vec<FileChange>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommitResult {
    pub commit_id: ObjectId,
    pub tree_id: ObjectId,
    pub parent_commit_id: Option<ObjectId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefCopy {
    pub source: String,
    pub destination: String,
    pub allow_non_fast_forward: bool,
}

#[derive(Clone, Debug)]
pub struct RepositoryPage {
    pub repositories: Vec<BackendRepositoryId>,
    pub next_page_token: Option<String>,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum BackendError {
    #[error("repository not found")]
    RepositoryNotFound,
    #[error("object not found")]
    ObjectNotFound,
    #[error("ref expectation did not match")]
    StaleExpectation { actual: Option<ObjectId> },
    #[error("invalid backend request: {field}: {reason}")]
    InvalidRequest { field: String, reason: String },
    #[error("unsupported path")]
    UnsupportedPath { path: Vec<u8> },
    #[error("backend request exceeds {limit} bytes")]
    TooLarge { limit: u64 },
    #[error("contention")]
    Contention,
    #[error("backend unavailable")]
    Unavailable,
    #[error("repository is sealed")]
    RepositorySealed,
    #[error("internal backend error")]
    Internal,
}

impl BackendError {
    pub const fn retryable(&self) -> bool {
        matches!(self, Self::Contention | Self::Unavailable)
    }
}

/// Adapters implement storage only; authorization belongs to the operation layer.
#[async_trait]
pub trait Backend: Send + Sync {
    fn capabilities(&self) -> Capabilities;
    async fn create_repository(
        &self,
        id: &BackendRepositoryId,
        default_branch: &str,
    ) -> Result<(), BackendError>;
    async fn delete_repository(&self, id: &BackendRepositoryId) -> Result<(), BackendError>;
    async fn seal_repository(&self, id: &BackendRepositoryId) -> Result<(), BackendError>;
    async fn list_repositories(
        &self,
        prefix: &str,
        page_token: Option<&str>,
    ) -> Result<RepositoryPage, BackendError>;
    async fn list_refs(
        &self,
        id: &BackendRepositoryId,
        prefixes: &[String],
    ) -> Result<RefSnapshot, BackendError>;
    async fn update_refs(
        &self,
        id: &BackendRepositoryId,
        updates: &[RefUpdate],
    ) -> Result<Vec<RefOutcome>, BackendError>;
    async fn read_object(
        &self,
        id: &BackendRepositoryId,
        object_id: &ObjectId,
    ) -> Result<RawObject, BackendError>;
    async fn is_ancestor(
        &self,
        id: &BackendRepositoryId,
        ancestor: &ObjectId,
        descendant: &ObjectId,
    ) -> Result<bool, BackendError>;
    async fn create_commit(
        &self,
        id: &BackendRepositoryId,
        request: &CommitRequest,
    ) -> Result<CommitResult, BackendError>;
    async fn copy_refs(
        &self,
        source: &BackendRepositoryId,
        destination: &BackendRepositoryId,
        copies: &[RefCopy],
        atomic: bool,
    ) -> Result<(), BackendError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_keys_reject_ambiguous_or_external_paths() {
        for invalid in [
            "",
            "/absolute",
            "../escape",
            "trailing/",
            "two//parts",
            "a/..",
            "a\\b",
            "tenant name",
            "a/%2f",
        ] {
            assert!(BackendRepositoryId::try_from(invalid.to_owned()).is_err());
        }
        assert!(BackendRepositoryId::try_from("a".repeat(201)).is_err());
        assert!(BackendRepositoryId::try_from("ten_01/rep_01/g1".to_owned()).is_ok());
    }
}
