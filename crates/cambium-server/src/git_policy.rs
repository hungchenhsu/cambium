//! Tenant policy for signed backend hooks. Context contains IDs, never credentials.

use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cambium_backend::{BackendRepositoryId, git_policy::*};
use cambium_protocol::{
    ApiError, ErrorCode, IdentityId, Principal, RepositoryId, Scope, TenantId, TokenId,
};
use serde::{Deserialize, Serialize};

use crate::service::{App, backend_id};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientContext {
    tenant_id: TenantId,
    token_id: TokenId,
    identity_id: IdentityId,
    repository_id: RepositoryId,
    backend_id: BackendRepositoryId,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Context {
    Client(ClientContext),
    Copy(cambium_store::compaction::CopyToken),
}

pub struct Policy {
    pub app: Arc<App>,
}

impl Policy {
    async fn internal_context(
        &self,
        bytes: &[u8],
        repository: &BackendRepositoryId,
    ) -> Result<bool, GitDenial> {
        if bytes.len() > 8192 {
            return Err(GitDenial::Unauthorized);
        }
        let context: Context =
            serde_json::from_slice(bytes).map_err(|_| GitDenial::Unauthorized)?;
        let Context::Copy(context) = context else {
            return Ok(false);
        };
        let current = self
            .app
            .store
            .revalidate_copy_token(&context.tenant_id, &context.id, self.app.clock.now_ms())
            .await
            .map_err(|e| denial(e.code()))?;
        if current.destination_backend_id != repository.as_str()
            || current.compaction_id != context.compaction_id
            || current.repository_id != context.repository_id
            || current.destination_backend_id != context.destination_backend_id
        {
            return Err(GitDenial::NotFound);
        }
        Ok(true)
    }

    async fn context(
        &self,
        bytes: &[u8],
        repository: &BackendRepositoryId,
        scope: Scope,
    ) -> Result<(ClientContext, Principal), GitDenial> {
        if bytes.len() > 8192 {
            return Err(GitDenial::Unauthorized);
        }
        let context: Context =
            serde_json::from_slice(bytes).map_err(|_| GitDenial::Unauthorized)?;
        let Context::Client(context) = context else {
            return Err(GitDenial::Unauthorized);
        };
        if &context.backend_id != repository {
            return Err(GitDenial::NotFound);
        }
        let principal = self
            .app
            .store
            .revalidate(
                &context.tenant_id,
                &context.token_id,
                self.app.clock.now_ms(),
            )
            .await
            .map_err(|e| denial(e.code()))?;
        if principal.actor.identity_id != context.identity_id {
            return Err(GitDenial::Unauthorized);
        }
        self.app
            .authorized_repository(&principal, &context.repository_id, scope)
            .await
            .map_err(|e| denial(e.code))?;
        Ok((context, principal))
    }
}

#[async_trait]
impl GitPolicy for Policy {
    async fn authorize(&self, request: &GitRequest) -> Result<GitGrant, GitDenial> {
        let values: Vec<_> = request
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.as_slice())
            .collect();
        if values.len() != 1 {
            return Err(GitDenial::Unauthorized);
        }
        let secret = git_credential(values[0])?;
        if secret.starts_with("cmi_") {
            let hash = crate::internal_copy::copy_token_hash(&secret)
                .map_err(|_| GitDenial::Unauthorized)?;
            let token = self
                .app
                .store
                .authenticate_copy_token(hash, self.app.clock.now_ms())
                .await
                .map_err(|e| denial(e.code()))?;
            let path = request.path.strip_prefix('/').unwrap_or(&request.path);
            let path = path.strip_suffix(".git").unwrap_or(path);
            if path != token.destination_backend_id {
                return Err(GitDenial::NotFound);
            }
            let repository = token
                .destination_backend_id
                .clone()
                .try_into()
                .map_err(|_| GitDenial::Unavailable)?;
            let actor = format!("copy:{}", token.compaction_id);
            let context =
                serde_json::to_vec(&Context::Copy(token)).map_err(|_| GitDenial::Unavailable)?;
            return Ok(GitGrant {
                repository,
                actor,
                context,
            });
        }
        // Authentication precedes parsing or looking up a repository path.
        let principal = self
            .app
            .authenticate(&secret)
            .await
            .map_err(|e| denial(e.code))?;
        let path = request.path.strip_prefix('/').unwrap_or(&request.path);
        let path = path.strip_suffix(".git").unwrap_or(path);
        let mut parts = path.split('/');
        let (Some(slug), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(GitDenial::NotFound);
        };
        if self
            .app
            .store
            .tenant_slug(&principal.tenant_id)
            .await
            .map_err(|e| denial(e.code()))?
            != slug
        {
            return Err(GitDenial::NotFound);
        }
        let repository = self
            .app
            .store
            .find_repository(&principal.tenant_id, name)
            .await
            .map_err(|e| denial(e.code()))?;
        let scope = match request.operation {
            GitOperation::Read => Scope::RepoRead,
            GitOperation::Write => Scope::RepoWrite,
        };
        principal
            .require_repository(&repository.id, scope)
            .map_err(|e| denial(e.code))?;
        if repository.state != "ready" {
            return Err(GitDenial::NotFound);
        }
        let backend_id = backend_id(&repository).map_err(|_| GitDenial::Unavailable)?;
        let context = ClientContext {
            tenant_id: principal.tenant_id,
            token_id: principal.token_id,
            identity_id: principal.actor.identity_id.clone(),
            repository_id: repository.id,
            backend_id: backend_id.clone(),
        };
        Ok(GitGrant {
            repository: backend_id,
            actor: principal.actor.identity_id.to_string(),
            context: serde_json::to_vec(&Context::Client(context))
                .map_err(|_| GitDenial::Unavailable)?,
        })
    }

    async fn visible_refs(
        &self,
        bytes: &[u8],
        repository: &BackendRepositoryId,
        refs: &[String],
    ) -> Result<Vec<String>, GitDenial> {
        if self.internal_context(bytes, repository).await? {
            return Ok(refs.to_vec());
        }
        let (context, _) = self.context(bytes, repository, Scope::RepoRead).await?;
        let blocked = self
            .app
            .store
            .blocked_branch_names(
                &context.tenant_id,
                &context.repository_id,
                self.app.clock.now_ms(),
            )
            .await
            .map_err(|e| denial(e.code()))?;
        Ok(refs
            .iter()
            .filter(|name| !name.starts_with("refs/cambium/") && !blocked.contains(name))
            .cloned()
            .collect())
    }

    async fn pre_receive(
        &self,
        bytes: &[u8],
        repository: &BackendRepositoryId,
        commands: &[GitCommand],
    ) -> Result<Vec<Option<String>>, GitDenial> {
        if self.internal_context(bytes, repository).await? {
            return Ok(vec![None; commands.len()]);
        }
        let (context, principal) = self.context(bytes, repository, Scope::RepoWrite).await?;
        let current = self
            .app
            .store
            .get_repository(&principal.tenant_id, &context.repository_id)
            .await
            .map_err(|e| denial(e.code()))?;
        if current.state != "ready"
            || current.fenced
            || backend_id(&current).map_err(|_| GitDenial::Unavailable)? != *repository
        {
            return Ok(vec![
                Some(
                    "Repository generation changed or is busy; retry the push".into()
                );
                commands.len()
            ]);
        }
        let blocked = self
            .app
            .store
            .blocked_branch_names(
                &principal.tenant_id,
                &context.repository_id,
                self.app.clock.now_ms(),
            )
            .await
            .map_err(|e| denial(e.code()))?;
        Ok(commands
            .iter()
            .map(|command| {
                if command.name.starts_with("refs/cambium/")
                    || command.name.starts_with("refs/cambium-rescue/")
                {
                    Some("Reserved ref namespace".into())
                } else if command.name.contains('\u{fffd}') {
                    Some("Ref names must be valid UTF-8".into())
                } else if blocked.contains(&command.name) {
                    Some("Ephemeral branch is unavailable or awaiting reclamation".into())
                } else {
                    None
                }
            })
            .collect())
    }

    async fn post_receive(
        &self,
        bytes: &[u8],
        repository: &BackendRepositoryId,
        commands: &[GitCommand],
    ) -> Result<(), GitDenial> {
        if self.internal_context(bytes, repository).await? {
            return Ok(());
        }
        // No lifecycle decision depends on this best-effort notification.
        let (context, principal) = self.context(bytes, repository, Scope::RepoWrite).await?;
        self.app
            .store
            .record_branch_activity(
                &principal,
                &context.repository_id,
                &commands
                    .iter()
                    .map(|command| command.name.clone())
                    .collect::<Vec<_>>(),
                self.app.clock.now_ms(),
            )
            .await
            .map_err(|e| denial(e.code()))?;
        Ok(())
    }
}

pub fn bearer(value: &[u8]) -> Result<String, ApiError> {
    let value =
        std::str::from_utf8(value).map_err(|_| ApiError::new(ErrorCode::Unauthenticated))?;
    let (scheme, secret) = value
        .split_once(' ')
        .ok_or_else(|| ApiError::new(ErrorCode::Unauthenticated))?;
    if !scheme.eq_ignore_ascii_case("bearer")
        || secret.is_empty()
        || secret.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return Err(ApiError::new(ErrorCode::Unauthenticated));
    }
    Ok(secret.to_owned())
}

fn git_credential(value: &[u8]) -> Result<String, GitDenial> {
    if let Ok(secret) = bearer(value) {
        return Ok(secret);
    }
    let value = std::str::from_utf8(value).map_err(|_| GitDenial::Unauthorized)?;
    let (scheme, encoded) = value.split_once(' ').ok_or(GitDenial::Unauthorized)?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return Err(GitDenial::Unauthorized);
    }
    let decoded = STANDARD
        .decode(encoded)
        .map_err(|_| GitDenial::Unauthorized)?;
    let decoded = std::str::from_utf8(&decoded).map_err(|_| GitDenial::Unauthorized)?;
    let (_, secret) = decoded.split_once(':').ok_or(GitDenial::Unauthorized)?;
    Ok(secret.to_owned())
}

fn denial(code: ErrorCode) -> GitDenial {
    match code {
        ErrorCode::Unauthenticated => GitDenial::Unauthorized,
        ErrorCode::PermissionDenied => GitDenial::Forbidden,
        ErrorCode::NotFound => GitDenial::NotFound,
        _ => GitDenial::Unavailable,
    }
}
