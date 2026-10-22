//! Internal copy credentials never resolve to a public principal.

use crate::{
    secrets::{new_token, token_hash},
    service::Clock,
};
use async_trait::async_trait;
use cambium_backend::{BackendError, BackendRepositoryId, RefCopy};
use cambium_backend_enroute::CopyCredentials;
use cambium_protocol::{ApiError, ErrorCode};
use cambium_store::{TokenHash, compaction::CompactionStore};
use sha2::{Digest as _, Sha256};
use std::sync::Arc;

pub struct CopyTokens {
    pub store: Arc<dyn CompactionStore>,
    pub clock: Arc<dyn Clock>,
}

pub fn copy_token_hash(secret: &str) -> Result<TokenHash, ApiError> {
    let suffix = secret
        .strip_prefix("cmi_")
        .ok_or_else(|| ApiError::new(ErrorCode::Unauthenticated))?;
    token_hash(&format!("cmb_{suffix}"))?;
    Ok(TokenHash(Sha256::digest(secret.as_bytes()).into()))
}

#[async_trait]
impl CopyCredentials for CopyTokens {
    async fn for_copy(
        &self,
        destination: &BackendRepositoryId,
        copies: &[RefCopy],
    ) -> Result<String, BackendError> {
        let secret = new_token()
            .map_err(|_| BackendError::Internal)?
            .replacen("cmb_", "cmi_", 1);
        let hash = copy_token_hash(&secret).map_err(|_| BackendError::Internal)?;
        let token = self
            .store
            .mint_copy_token(destination.as_str(), hash, self.clock.now_ms())
            .await
            .map_err(|_| BackendError::Unavailable)?;
        let prefix = format!("refs/cambium/{}/", token.compaction_id);
        if copies
            .iter()
            .any(|copy| !copy.destination.starts_with(&prefix))
        {
            self.store
                .revoke_copy_token(hash)
                .await
                .map_err(|_| BackendError::Unavailable)?;
            return Err(BackendError::InvalidRequest {
                field: "copies".into(),
                reason: "Internal copies target their compaction's temporary namespace".into(),
            });
        }
        Ok(secret)
    }
    async fn finished_copy(&self, secret: &str) -> Result<(), BackendError> {
        self.store
            .revoke_copy_token(copy_token_hash(secret).map_err(|_| BackendError::Internal)?)
            .await
            .map_err(|_| BackendError::Unavailable)
    }
}
