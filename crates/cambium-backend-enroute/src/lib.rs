//! Enroute 0.3.0-cambium.1 adapter, with wire sources pinned under `vendor/enroute`.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use cambium_backend::{
    Backend, BackendError, BackendRepositoryId, Capabilities, CommitRequest, CommitResult,
    FileChange, FileMode, GitIdentity, GitRef, ObjectFormat, ObjectKind, RawObject, RefCopy,
    RefExpectation, RefOutcome, RefRejection, RefSnapshot, RefUpdate, RepositoryPage,
};
use cambium_protocol::ObjectId;
use prost::Message as _;
use sha1::{Digest as _, Sha1};
use tonic::{Code, Status, transport::Channel};
use url::Url;

/// Unmodified Apache-2.0 source from the pinned Enroute commit.
#[path = "../../../vendor/enroute/signature.rs"]
pub mod signature;

pub mod hooks;

pub mod wire {
    pub mod enroute {
        pub mod common {
            pub mod v1alpha1 {
                tonic::include_proto!("enroute.common.v1alpha1");
            }
        }
        pub mod api {
            pub mod v1alpha1 {
                tonic::include_proto!("enroute.api.v1alpha1");
            }
        }
        pub mod hook {
            pub mod v1alpha1 {
                tonic::include_proto!("enroute.hook.v1alpha1");
            }
        }
    }
}

use api::{
    commit_service_client::CommitServiceClient, object_service_client::ObjectServiceClient,
    ref_service_client::RefServiceClient, repository_service_client::RepositoryServiceClient,
    sync_service_client::SyncServiceClient,
};
use wire::enroute::{api::v1alpha1 as api, common::v1alpha1 as common};

const COMMIT_LIMIT: u64 = 32 * 1024 * 1024;

/// The operation layer mints narrowly scoped credentials for backend copies.
/// Credentials never come from a public request or a backend repository key.
#[async_trait]
pub trait CopyCredentials: Send + Sync {
    async fn for_copy(
        &self,
        destination: &BackendRepositoryId,
        copies: &[RefCopy],
    ) -> Result<String, BackendError>;
}

#[derive(Clone)]
pub struct EnrouteBackend {
    channel: Channel,
    git_base: Url,
    credentials: Option<Arc<dyn CopyCredentials>>,
    max_object_bytes: u64,
}

impl EnrouteBackend {
    /// Connect only to a private listener. The deployment must enforce isolation.
    /// The non-mutating probe rejects binaries without the durable seal RPC.
    pub async fn connect(
        endpoint: &str,
        git_base: &str,
        credentials: Option<Arc<dyn CopyCredentials>>,
    ) -> Result<Self, BackendError> {
        let git_base = Url::parse(git_base).map_err(|_| invalid("git_base"))?;
        if !matches!(git_base.scheme(), "http" | "https")
            || git_base.host_str().is_none()
            || !git_base.username().is_empty()
            || git_base.password().is_some()
            || git_base.query().is_some()
            || git_base.fragment().is_some()
            || !git_base.path().ends_with('/')
        {
            return Err(invalid("git_base"));
        }
        let channel = Channel::from_shared(endpoint.to_owned())
            .map_err(|_| invalid("endpoint"))?
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .connect()
            .await
            .map_err(|_| BackendError::Unavailable)?;
        match RepositoryServiceClient::new(channel.clone())
            .seal_repository(api::SealRepositoryRequest { repo: None })
            .await
        {
            Err(error) if error.code() == Code::InvalidArgument => {}
            Err(error) if error.code() == Code::Unimplemented => {
                return Err(invalid("durable_seal"));
            }
            Err(error) => return Err(map_status(error)),
            Ok(_) => return Err(BackendError::Internal),
        }
        Ok(Self {
            channel,
            git_base,
            credentials,
            max_object_bytes: 128 * 1024 * 1024,
        })
    }

    pub fn with_max_object_bytes(mut self, limit: u64) -> Self {
        self.max_object_bytes = limit;
        self
    }

    fn repositories(&self) -> RepositoryServiceClient<Channel> {
        RepositoryServiceClient::new(self.channel.clone())
    }

    fn refs(&self) -> RefServiceClient<Channel> {
        RefServiceClient::new(self.channel.clone())
    }

    async fn current_ref(
        &self,
        id: &BackendRepositoryId,
        name: &str,
    ) -> Result<Option<ObjectId>, BackendError> {
        Ok(self
            .list_refs(id, &[name.to_owned()])
            .await?
            .refs
            .into_iter()
            .find(|item| item.name == name)
            .map(|item| item.object_id))
    }

    async fn update_one(
        &self,
        id: &BackendRepositoryId,
        update: &RefUpdate,
    ) -> Result<RefOutcome, BackendError> {
        if !update.name.starts_with("refs/")
            || gix_validate::reference::name(update.name.as_bytes().into()).is_err()
        {
            return Ok(RefOutcome {
                name: update.name.clone(),
                rejection: Some(RefRejection::InvalidName),
            });
        }
        // Enroute expresses an unconditional put through a read and CAS retry.
        // An absent-delete must never become its unconditional-delete encoding.
        if update.new.is_none() && update.expected == RefExpectation::Absent {
            return Ok(RefOutcome {
                name: update.name.clone(),
                rejection: self
                    .current_ref(id, &update.name)
                    .await?
                    .map(|_| RefRejection::AlreadyExists),
            });
        }
        for _ in 0..16 {
            let old = match &update.expected {
                RefExpectation::At(oid) => Some(oid.clone()),
                RefExpectation::Absent => None,
                RefExpectation::Any if update.new.is_none() => None,
                RefExpectation::Any => self.current_ref(id, &update.name).await?,
            };
            let response = self
                .refs()
                .update_refs(api::UpdateRefsRequest {
                    repo: Some(key(id)),
                    updates: vec![api::RefUpdate {
                        refname: update.name.clone(),
                        old_object_id: old.as_ref().map(oid),
                        new_object_id: update.new.as_ref().map(oid),
                    }],
                })
                .await
                .map_err(map_status)?
                .into_inner();
            if response.outcomes.len() != 1 {
                return Err(BackendError::Internal);
            }
            let result = response
                .outcomes
                .into_iter()
                .next()
                .ok_or(BackendError::Internal)?;
            if result.refname != update.name {
                return Err(BackendError::Internal);
            }
            let rejection = result
                .rejection
                .map(|rejection| {
                    use api::ref_update_outcome::rejection::Reason;
                    match Reason::try_from(rejection.reason) {
                        Ok(Reason::MissingObjects) => Ok(RefRejection::MissingObject),
                        Ok(Reason::FunnyRefname | Reason::InvalidRefname) => {
                            Ok(RefRejection::InvalidName)
                        }
                        Ok(Reason::NonCommitObject) => Ok(RefRejection::NotACommit),
                        Ok(Reason::NonFastForward) => Ok(RefRejection::Stale),
                        Ok(Reason::AlreadyExists) => Ok(RefRejection::AlreadyExists),
                        _ => Err(BackendError::Internal),
                    }
                })
                .transpose()?;
            if update.expected == RefExpectation::Any
                && update.new.is_some()
                && matches!(
                    rejection,
                    Some(RefRejection::Stale | RefRejection::AlreadyExists)
                )
            {
                continue;
            }
            return Ok(RefOutcome {
                name: update.name.clone(),
                rejection,
            });
        }
        Err(BackendError::Contention)
    }
}

#[async_trait]
impl Backend for EnrouteBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            object_format: ObjectFormat::Sha1,
            max_commit_request_bytes: COMMIT_LIMIT,
            non_utf8_paths: false,
            merge_commits: false,
            atomic_ref_updates: false,
            reclaims_unreachable_objects: false,
            durable_seal: true,
        }
    }

    async fn create_repository(
        &self,
        id: &BackendRepositoryId,
        default_branch: &str,
    ) -> Result<(), BackendError> {
        self.repositories()
            .create_repository(api::CreateRepositoryRequest {
                repo: Some(key(id)),
                default_branch: default_branch.to_owned(),
            })
            .await
            .map_err(map_status)?;
        Ok(())
    }

    async fn delete_repository(&self, id: &BackendRepositoryId) -> Result<(), BackendError> {
        self.repositories()
            .delete_repository(api::DeleteRepositoryRequest {
                repo: Some(key(id)),
            })
            .await
            .map_err(map_status)?;
        Ok(())
    }

    async fn seal_repository(&self, id: &BackendRepositoryId) -> Result<(), BackendError> {
        self.repositories()
            .seal_repository(api::SealRepositoryRequest {
                repo: Some(key(id)),
            })
            .await
            .map_err(map_status)?;
        Ok(())
    }

    async fn list_repositories(
        &self,
        prefix: &str,
        page_token: Option<&str>,
    ) -> Result<RepositoryPage, BackendError> {
        let result = self
            .repositories()
            .list_repositories(api::ListRepositoriesRequest {
                limit: 1000,
                prefix: prefix.to_owned(),
                page_token: page_token.unwrap_or_default().to_owned(),
            })
            .await
            .map_err(map_status)?
            .into_inner();
        let repositories = result
            .repositories
            .into_iter()
            .map(|repository| {
                BackendRepositoryId::try_from(repository.repo.ok_or(BackendError::Internal)?.key)
                    .map_err(|_| BackendError::Internal)
            })
            .collect::<Result<_, _>>()?;
        Ok(RepositoryPage {
            repositories,
            next_page_token: nonempty(result.next_page_token),
        })
    }

    async fn list_refs(
        &self,
        id: &BackendRepositoryId,
        prefixes: &[String],
    ) -> Result<RefSnapshot, BackendError> {
        let result = self
            .refs()
            .list_refs(api::ListRefsRequest {
                repo: Some(key(id)),
                prefixes: prefixes.to_vec(),
            })
            .await
            .map_err(map_status)?
            .into_inner();
        let refs = result
            .refs
            .into_iter()
            .map(|item| {
                Ok(GitRef {
                    name: item.name,
                    object_id: required_oid(item.object_id)?,
                })
            })
            .collect::<Result<_, BackendError>>()?;
        Ok(RefSnapshot {
            head: result.default_branch,
            refs,
        })
    }

    async fn update_refs(
        &self,
        id: &BackendRepositoryId,
        updates: &[RefUpdate],
    ) -> Result<Vec<RefOutcome>, BackendError> {
        let mut results = Vec::with_capacity(updates.len());
        for update in updates {
            results.push(self.update_one(id, update).await?);
        }
        Ok(results)
    }

    async fn read_object(
        &self,
        id: &BackendRepositoryId,
        object_id: &ObjectId,
    ) -> Result<RawObject, BackendError> {
        let response = ObjectServiceClient::new(self.channel.clone())
            .get_object(api::GetObjectRequest {
                repo: Some(key(id)),
                object_id: Some(oid(object_id)),
            })
            .await;
        let mut stream = match response {
            Ok(response) => response.into_inner(),
            Err(error) if error.code() == Code::NotFound => {
                self.repositories()
                    .get_repository(api::GetRepositoryRequest {
                        repo: Some(key(id)),
                    })
                    .await
                    .map_err(map_status)?;
                return Err(BackendError::ObjectNotFound);
            }
            Err(error) => return Err(map_status(error)),
        };
        use api::get_object_response::Chunk;
        let header = match stream
            .message()
            .await
            .map_err(map_status)?
            .and_then(|message| message.chunk)
        {
            Some(Chunk::Header(header)) => header,
            _ => return Err(BackendError::Internal),
        };
        if header.size > self.max_object_bytes {
            return Err(BackendError::TooLarge {
                limit: self.max_object_bytes,
            });
        }
        let kind = match api::ObjectKind::try_from(header.kind) {
            Ok(api::ObjectKind::Blob) => ObjectKind::Blob,
            Ok(api::ObjectKind::Tree) => ObjectKind::Tree,
            Ok(api::ObjectKind::Commit) => ObjectKind::Commit,
            Ok(api::ObjectKind::Tag) => ObjectKind::Tag,
            _ => return Err(BackendError::Internal),
        };
        let mut bytes = Vec::new();
        while let Some(message) = stream.message().await.map_err(map_status)? {
            let Some(Chunk::Data(chunk)) = message.chunk else {
                return Err(BackendError::Internal);
            };
            if (bytes.len() as u64).saturating_add(chunk.len() as u64) > header.size {
                return Err(BackendError::Internal);
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.len() as u64 != header.size {
            return Err(BackendError::Internal);
        }
        let mut hash = Sha1::new();
        hash.update(format!("{} {}\0", kind.as_str(), bytes.len()));
        hash.update(&bytes);
        if format!("{:x}", hash.finalize()) != object_id.as_str() {
            return Err(BackendError::Internal);
        }
        Ok(RawObject { kind, bytes })
    }

    async fn is_ancestor(
        &self,
        id: &BackendRepositoryId,
        ancestor: &ObjectId,
        descendant: &ObjectId,
    ) -> Result<bool, BackendError> {
        Ok(self
            .refs()
            .is_ancestor(api::IsAncestorRequest {
                repo: Some(key(id)),
                ancestor_commit_id: Some(oid(ancestor)),
                descendant_commit_id: Some(oid(descendant)),
            })
            .await
            .map_err(map_status)?
            .into_inner()
            .is_ancestor)
    }

    async fn create_commit(
        &self,
        id: &BackendRepositoryId,
        request: &CommitRequest,
    ) -> Result<CommitResult, BackendError> {
        let changes = request
            .changes
            .iter()
            .map(|change| {
                let (path, change) = match change {
                    FileChange::Put {
                        path,
                        contents,
                        mode,
                    } => (
                        path,
                        api::path_change::Change::Put(api::PutFile {
                            content: contents.clone(),
                            mode: match mode {
                                FileMode::Regular => api::FileMode::Regular,
                                FileMode::Executable => api::FileMode::Executable,
                                FileMode::Symlink => api::FileMode::Symlink,
                            } as i32,
                        }),
                    ),
                    FileChange::Delete { path } => {
                        (path, api::path_change::Change::Delete(api::DeleteFile {}))
                    }
                };
                let path = std::str::from_utf8(path)
                    .map_err(|_| BackendError::UnsupportedPath { path: path.clone() })?;
                Ok(api::PathChange {
                    path: path.to_owned(),
                    change: Some(change),
                })
            })
            .collect::<Result<Vec<_>, BackendError>>()?;
        let expectation = match &request.expected_parent {
            Some(parent) => api::expected_old_commit::Expectation::CommitId(oid(parent)),
            None => {
                api::expected_old_commit::Expectation::Absent(api::expected_old_commit::Absent {})
            }
        };
        let encoded = api::CreateCommitRequest {
            repo: Some(key(id)),
            branch: request.branch.clone(),
            expected_old_commit: Some(api::ExpectedOldCommit {
                expectation: Some(expectation),
            }),
            message: request.message.clone(),
            author: Some(identity(&request.author)),
            committer: Some(identity(&request.committer)),
            changes,
        };
        if encoded.encoded_len() as u64 > COMMIT_LIMIT {
            return Err(BackendError::TooLarge {
                limit: COMMIT_LIMIT,
            });
        }
        let result = CommitServiceClient::new(self.channel.clone())
            .max_encoding_message_size(COMMIT_LIMIT as usize)
            .create_commit(encoded)
            .await;
        let result = match result {
            Ok(result) => result.into_inner(),
            Err(error)
                if error.code() == Code::FailedPrecondition
                    && error.message() != "repository sealed" =>
            {
                return Err(BackendError::StaleExpectation {
                    actual: self.current_ref(id, &request.branch).await?,
                });
            }
            Err(error) => return Err(map_status(error)),
        };
        Ok(CommitResult {
            commit_id: required_oid(result.commit_id)?,
            tree_id: required_oid(result.tree_id)?,
            parent_commit_id: result
                .parent_commit_id
                .map(|value| required_oid(Some(value)))
                .transpose()?,
        })
    }

    async fn copy_refs(
        &self,
        source: &BackendRepositoryId,
        destination: &BackendRepositoryId,
        copies: &[RefCopy],
        atomic: bool,
    ) -> Result<(), BackendError> {
        if copies.is_empty() {
            return Ok(());
        }
        if copies
            .iter()
            .any(|copy| copy.source.is_empty() || copy.destination.is_empty())
        {
            return Err(invalid("copies"));
        }
        let url = self
            .git_base
            .join(&format!("{}.git", destination.as_str()))
            .map_err(|_| invalid("destination"))?;
        let credentials = match &self.credentials {
            Some(provider) => Some(api::remote::Credentials::BearerToken(
                provider.for_copy(destination, copies).await?,
            )),
            None => None,
        };
        let result = SyncServiceClient::new(self.channel.clone())
            .push_to_remote(api::PushToRemoteRequest {
                repo: Some(key(source)),
                remote: Some(api::Remote {
                    url: url.to_string(),
                    credentials,
                }),
                refs: copies
                    .iter()
                    .map(|copy| api::RefSpec {
                        source: copy.source.clone(),
                        destination: copy.destination.clone(),
                        force: copy.allow_non_fast_forward,
                    })
                    .collect(),
                atomic,
            })
            .await
            .map_err(map_status)?
            .into_inner();
        if result.outcomes.len() != copies.len() {
            return Err(BackendError::Internal);
        }
        if result.outcomes.iter().any(|outcome| {
            outcome.status == api::ref_push_outcome::Status::Rejected as i32
                && outcome.message == "repository sealed"
        }) {
            return Err(BackendError::RepositorySealed);
        }
        for (outcome, copy) in result.outcomes.iter().zip(copies) {
            if outcome.destination != copy.destination {
                return Err(BackendError::Internal);
            }
            use api::ref_push_outcome::Status;
            match Status::try_from(outcome.status) {
                Ok(Status::Updated | Status::UpToDate) => {}
                Ok(Status::Rejected) => {
                    return Err(BackendError::StaleExpectation {
                        actual: outcome
                            .old_object_id
                            .clone()
                            .map(|value| required_oid(Some(value)))
                            .transpose()?,
                    });
                }
                _ => return Err(BackendError::Internal),
            }
        }
        Ok(())
    }
}

fn key(id: &BackendRepositoryId) -> common::RepoKey {
    common::RepoKey {
        key: id.as_str().to_owned(),
    }
}
fn oid(id: &ObjectId) -> common::ObjectId {
    common::ObjectId {
        hex: id.as_str().to_owned(),
    }
}
fn required_oid(value: Option<common::ObjectId>) -> Result<ObjectId, BackendError> {
    value
        .ok_or(BackendError::Internal)?
        .hex
        .parse()
        .map_err(|_| BackendError::Internal)
}
fn nonempty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}
fn identity(value: &GitIdentity) -> api::Identity {
    api::Identity {
        name: value.name.clone(),
        email: value.email.clone(),
        timestamp: Some(prost_types::Timestamp {
            seconds: value.timestamp_seconds,
            nanos: 0,
        }),
        utc_offset_seconds: value.utc_offset_seconds,
    }
}
fn invalid(field: &str) -> BackendError {
    BackendError::InvalidRequest {
        field: field.to_owned(),
        reason: "Invalid backend request".to_owned(),
    }
}
fn map_status(status: Status) -> BackendError {
    match status.code() {
        Code::NotFound => BackendError::RepositoryNotFound,
        Code::InvalidArgument => invalid("request"),
        Code::OutOfRange | Code::ResourceExhausted => BackendError::TooLarge {
            limit: COMMIT_LIMIT,
        },
        Code::Aborted => BackendError::Contention,
        Code::Unavailable | Code::DeadlineExceeded | Code::Cancelled => BackendError::Unavailable,
        Code::FailedPrecondition if status.message() == "repository sealed" => {
            BackendError::RepositorySealed
        }
        _ => BackendError::Internal,
    }
}
