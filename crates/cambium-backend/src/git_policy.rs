use async_trait::async_trait;
use cambium_protocol::ObjectId;

use crate::{BackendRepositoryId, GitRef};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitOperation {
    Read,
    Write,
}

#[derive(Clone, Debug)]
pub struct GitRequest {
    pub path: String,
    pub headers: Vec<(String, Vec<u8>)>,
    pub operation: GitOperation,
}

#[derive(Clone, Debug)]
pub struct GitGrant {
    pub repository: BackendRepositoryId,
    pub actor: String,
    pub context: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitDenial {
    Unauthorized,
    Forbidden,
    NotFound,
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct GitCommand {
    pub name: String,
    pub old: Option<ObjectId>,
    pub new: Option<ObjectId>,
}

/// Policy failure must deny access. Adapter-specific wire types stay outside.
#[async_trait]
pub trait GitPolicy: Send + Sync {
    async fn authorize(&self, request: &GitRequest) -> Result<GitGrant, GitDenial>;
    async fn visible_refs(
        &self,
        context: &[u8],
        repository: &BackendRepositoryId,
        refs: &[GitRef],
    ) -> Result<Vec<String>, GitDenial>;
    async fn pre_receive(
        &self,
        context: &[u8],
        repository: &BackendRepositoryId,
        commands: &[GitCommand],
    ) -> Result<Vec<Option<String>>, GitDenial>;
    async fn post_receive(
        &self,
        context: &[u8],
        repository: &BackendRepositoryId,
        commands: &[GitCommand],
    ) -> Result<(), GitDenial>;
}
