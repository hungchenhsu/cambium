//! Runs the backend-independent conformance suite (ADR-0007) against the pinned
//! patched Enroute. Stock-git paths and the git edge are covered by `backend.rs`
//! and `hooks.rs`.

mod common;

use cambium_backend_conformance::suite::{self, Namespace};

#[tokio::test]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn enroute_conforms_to_the_backend_suite() {
    let server = common::Server::start().await;
    suite::run(&server.backend, &Namespace::new("conformance/suite")).await;
}

use async_trait::async_trait;
use cambium_backend::*;
use cambium_protocol::ObjectId;

/// A deliberately broken adapter, to prove the suite detects each violation.
struct Broken {
    inner: cambium_backend_enroute::EnrouteBackend,
    fault: Fault,
}

#[derive(Clone, Copy, Debug)]
enum Fault {
    CorruptReads,
    ReorderOutcomes,
    UnknownAncestorIsError,
    CopyIgnoresNonFastForward,
    RecreateKeepsRefs,
    SealIsIgnored,
}

#[async_trait]
impl Backend for Broken {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    async fn create_repository(
        &self,
        id: &BackendRepositoryId,
        default_branch: &str,
    ) -> Result<(), BackendError> {
        self.inner.create_repository(id, default_branch).await
    }
    async fn delete_repository(&self, id: &BackendRepositoryId) -> Result<(), BackendError> {
        if matches!(self.fault, Fault::RecreateKeepsRefs) {
            return Ok(());
        }
        self.inner.delete_repository(id).await
    }
    async fn seal_repository(&self, id: &BackendRepositoryId) -> Result<(), BackendError> {
        if matches!(self.fault, Fault::SealIsIgnored) {
            return Ok(());
        }
        self.inner.seal_repository(id).await
    }
    async fn create_ref_once(
        &self,
        id: &BackendRepositoryId,
        operation_id: &str,
        name: &str,
        object_id: &ObjectId,
    ) -> Result<bool, BackendError> {
        self.inner
            .create_ref_once(id, operation_id, name, object_id)
            .await
    }
    async fn list_repositories(
        &self,
        prefix: &str,
        page_token: Option<&str>,
    ) -> Result<RepositoryPage, BackendError> {
        self.inner.list_repositories(prefix, page_token).await
    }
    async fn list_refs(
        &self,
        id: &BackendRepositoryId,
        prefixes: &[String],
    ) -> Result<RefSnapshot, BackendError> {
        self.inner.list_refs(id, prefixes).await
    }
    async fn update_refs(
        &self,
        id: &BackendRepositoryId,
        updates: &[RefUpdate],
    ) -> Result<Vec<RefOutcome>, BackendError> {
        let mut outcomes = self.inner.update_refs(id, updates).await?;
        if matches!(self.fault, Fault::ReorderOutcomes) {
            outcomes.reverse();
        }
        Ok(outcomes)
    }
    async fn read_object(
        &self,
        id: &BackendRepositoryId,
        object_id: &ObjectId,
    ) -> Result<RawObject, BackendError> {
        let mut object = self.inner.read_object(id, object_id).await?;
        if matches!(self.fault, Fault::CorruptReads) && object.kind == ObjectKind::Blob {
            object.bytes.push(b'!');
        }
        Ok(object)
    }
    async fn is_ancestor(
        &self,
        id: &BackendRepositoryId,
        ancestor: &ObjectId,
        descendant: &ObjectId,
    ) -> Result<bool, BackendError> {
        if matches!(self.fault, Fault::UnknownAncestorIsError)
            && ancestor.as_str() == "f".repeat(40)
        {
            return Err(BackendError::ObjectNotFound);
        }
        self.inner.is_ancestor(id, ancestor, descendant).await
    }
    async fn create_commit(
        &self,
        id: &BackendRepositoryId,
        request: &CommitRequest,
    ) -> Result<CommitResult, BackendError> {
        self.inner.create_commit(id, request).await
    }
    async fn copy_refs(
        &self,
        source: &BackendRepositoryId,
        destination: &BackendRepositoryId,
        copies: &[RefCopy],
        atomic: bool,
    ) -> Result<(), BackendError> {
        if matches!(self.fault, Fault::CopyIgnoresNonFastForward) {
            let forced: Vec<_> = copies
                .iter()
                .map(|copy| RefCopy {
                    allow_non_fast_forward: true,
                    ..copy.clone()
                })
                .collect();
            return self
                .inner
                .copy_refs(source, destination, &forced, atomic)
                .await;
        }
        self.inner
            .copy_refs(source, destination, copies, atomic)
            .await
    }
}

#[tokio::test]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn the_suite_detects_each_injected_contract_violation() {
    let server = common::Server::start().await;
    for (index, fault) in [
        Fault::CorruptReads,
        Fault::ReorderOutcomes,
        Fault::UnknownAncestorIsError,
        Fault::CopyIgnoresNonFastForward,
        Fault::RecreateKeepsRefs,
        Fault::SealIsIgnored,
    ]
    .into_iter()
    .enumerate()
    {
        let backend = Broken {
            inner: server.backend.clone(),
            fault,
        };
        let result = tokio::spawn(async move {
            suite::run(&backend, &Namespace::new(&format!("mutation/f{index}"))).await;
        })
        .await;
        assert!(
            result.as_ref().is_err_and(|error| error.is_panic()),
            "the suite missed {fault:?}"
        );
    }
}
