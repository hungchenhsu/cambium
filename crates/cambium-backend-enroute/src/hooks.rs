//! Signature verification precedes decoding and dispatch into backend-neutral policy.

use std::sync::Arc;

use cambium_backend::{BackendRepositoryId, git_policy::*};
use http::HeaderMap;
use prost::Message as _;

use crate::{
    signature,
    wire::enroute::{common::v1alpha1 as common, hook::v1alpha1 as wire},
};

pub const MAX_HOOK_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("invalid hook signature")]
    Signature,
    #[error("invalid hook request")]
    InvalidRequest,
    #[error("hook request too large")]
    TooLarge,
    #[error("git policy unavailable")]
    Unavailable,
}

pub struct HookAdapter {
    keys: Vec<signature::VerifyingKey>,
    authority: String,
    path: String,
    policy: Arc<dyn GitPolicy>,
}

impl HookAdapter {
    pub fn new(
        keys: Vec<signature::VerifyingKey>,
        endpoint: &str,
        policy: Arc<dyn GitPolicy>,
    ) -> Result<Self, HookError> {
        let url = url::Url::parse(endpoint).map_err(|_| HookError::InvalidRequest)?;
        if keys.is_empty()
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(HookError::InvalidRequest);
        }
        let authority = url[url::Position::BeforeHost..url::Position::AfterPort].to_owned();
        Ok(Self {
            keys,
            authority,
            path: url.path().to_owned(),
            policy,
        })
    }

    /// The HTTP route must supply the actual method, authority, path and body.
    pub async fn handle(
        &self,
        method: &str,
        authority: &str,
        path: &str,
        headers: &HeaderMap,
        body: &[u8],
        now: u64,
    ) -> Result<Vec<u8>, HookError> {
        if body.len() > MAX_HOOK_BYTES {
            return Err(HookError::TooLarge);
        }
        if method != "POST" || authority != self.authority || path != self.path {
            return Err(HookError::Signature);
        }
        signature::verify(
            &self.keys,
            &signature::Covered {
                method,
                authority,
                path,
                body,
            },
            headers,
            now,
        )
        .map_err(|_| HookError::Signature)?;
        let request = wire::HookRequest::decode(body).map_err(|_| HookError::InvalidRequest)?;
        use wire::{hook_request::Call, hook_response::Answer};
        let answer = match request.call.ok_or(HookError::InvalidRequest)? {
            Call::Authorize(request) => {
                let operation = match wire::Access::try_from(request.access) {
                    Ok(wire::Access::Read) => GitOperation::Read,
                    Ok(wire::Access::Write) => GitOperation::Write,
                    _ => return Err(HookError::InvalidRequest),
                };
                let result = self
                    .policy
                    .authorize(&GitRequest {
                        path: request.repo_path,
                        operation,
                        headers: request
                            .headers
                            .into_iter()
                            .map(|header| (header.name, header.value.into_bytes()))
                            .collect(),
                    })
                    .await;
                use wire::authorize_response::Outcome;
                let outcome = match result {
                    Ok(grant) if grant.context.len() <= 8192 => Outcome::Granted(wire::Granted {
                        repo: Some(common::RepoKey {
                            key: grant.repository.as_str().to_owned(),
                        }),
                        actor: grant.actor,
                        context: grant.context,
                    }),
                    Ok(_) | Err(GitDenial::Unavailable) => return Err(HookError::Unavailable),
                    Err(denial) => {
                        let denial = match denial {
                            GitDenial::Unauthorized => wire::Denial::Unauthorized,
                            GitDenial::Forbidden => wire::Denial::Forbidden,
                            GitDenial::NotFound => wire::Denial::NotFound,
                            GitDenial::Unavailable => unreachable!(),
                        };
                        Outcome::Denied(wire::Denied {
                            denial: denial as i32,
                            challenge: (denial == wire::Denial::Unauthorized).then(|| {
                                wire::Challenge {
                                    www_authenticate: "Basic realm=\"cambium\"".into(),
                                    help: "Provide a valid Cambium token as the password.\n".into(),
                                }
                            }),
                        })
                    }
                };
                Answer::Authorize(wire::AuthorizeResponse {
                    outcome: Some(outcome),
                })
            }
            Call::VisibleRefs(request) => {
                let repository = repository(request.repo)?;
                let visible = self
                    .policy
                    .visible_refs(&request.context, &repository, &request.refnames)
                    .await
                    .map_err(|_| HookError::Unavailable)?;
                if visible.iter().any(|name| !request.refnames.contains(name)) {
                    return Err(HookError::Unavailable);
                }
                Answer::VisibleRefs(wire::VisibleRefsResponse { refnames: visible })
            }
            Call::PreReceive(request) => {
                let repository = repository(request.repo)?;
                let commands = commands(request.commands)?;
                let verdicts = self
                    .policy
                    .pre_receive(&request.context, &repository, &commands)
                    .await
                    .map_err(|_| HookError::Unavailable)?;
                if verdicts.len() != commands.len() {
                    return Err(HookError::Unavailable);
                }
                let judgements = commands
                    .into_iter()
                    .zip(verdicts)
                    .map(|(command, reason)| wire::RefJudgement {
                        refname: command.name,
                        judgement: if reason.is_some() {
                            wire::Judgement::Refuse
                        } else {
                            wire::Judgement::Allow
                        } as i32,
                        reason: reason.unwrap_or_default(),
                    })
                    .collect();
                Answer::PreReceive(wire::PreReceiveResponse { judgements })
            }
            Call::PostReceive(request) => {
                let repository = repository(request.repo)?;
                self.policy
                    .post_receive(&request.context, &repository, &commands(request.commands)?)
                    .await
                    .map_err(|_| HookError::Unavailable)?;
                Answer::PostReceive(wire::PostReceiveResponse {
                    messages: Vec::new(),
                })
            }
        };
        Ok(wire::HookResponse {
            answer: Some(answer),
        }
        .encode_to_vec())
    }
}

fn repository(value: Option<common::RepoKey>) -> Result<BackendRepositoryId, HookError> {
    value
        .ok_or(HookError::InvalidRequest)?
        .key
        .try_into()
        .map_err(|_| HookError::InvalidRequest)
}

fn commands(values: Vec<wire::RefCommand>) -> Result<Vec<GitCommand>, HookError> {
    values
        .into_iter()
        .map(|value| {
            Ok(GitCommand {
                name: value.refname,
                old: value
                    .old_object_id
                    .map(|id| id.hex.parse().map_err(|_| HookError::InvalidRequest))
                    .transpose()?,
                new: value
                    .new_object_id
                    .map(|id| id.hex.parse().map_err(|_| HookError::InvalidRequest))
                    .transpose()?,
                force: value.force,
            })
        })
        .collect()
}
