use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use cambium_backend::{
    Backend as _, BackendRepositoryId,
    git_policy::{GitCommand, GitDenial, GitGrant, GitPolicy, GitRequest},
};
use cambium_backend_conformance::git_edge::{PolicyControl, run};
use cambium_backend_enroute::{hooks::HookAdapter, signature::SigningKey};
use cambium_server::transport::hook_router;

struct FixturePolicy {
    repository: BackendRepositoryId,
    denied: AtomicBool,
}

#[async_trait]
impl GitPolicy for FixturePolicy {
    async fn authorize(&self, request: &GitRequest) -> Result<GitGrant, GitDenial> {
        if self.denied.load(Ordering::SeqCst)
            || request.path.trim_start_matches('/') != "suite/project.git"
        {
            return Err(GitDenial::NotFound);
        }
        Ok(GitGrant {
            repository: self.repository.clone(),
            actor: "conformance".into(),
            context: b"fixture".to_vec(),
        })
    }

    async fn visible_refs(
        &self,
        context: &[u8],
        repository: &BackendRepositoryId,
        refs: &[String],
    ) -> Result<Vec<String>, GitDenial> {
        assert_eq!(context, b"fixture");
        assert_eq!(repository, &self.repository);
        Ok(refs
            .iter()
            .filter(|r| r.as_str() != "refs/heads/hidden")
            .cloned()
            .collect())
    }

    async fn pre_receive(
        &self,
        context: &[u8],
        repository: &BackendRepositoryId,
        commands: &[GitCommand],
    ) -> Result<Vec<Option<String>>, GitDenial> {
        assert_eq!(context, b"fixture");
        assert_eq!(repository, &self.repository);
        Ok(commands
            .iter()
            .map(|c| (c.name == "refs/heads/refused").then(|| "ref_forbidden".into()))
            .collect())
    }

    async fn post_receive(
        &self,
        context: &[u8],
        repository: &BackendRepositoryId,
        _: &[GitCommand],
    ) -> Result<(), GitDenial> {
        assert_eq!(context, b"fixture");
        assert_eq!(repository, &self.repository);
        Ok(())
    }
}

struct Wiring {
    policy: Arc<FixturePolicy>,
    listener: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl PolicyControl for Wiring {
    fn deny_access(&self, deny: bool) {
        self.policy.denied.store(deny, Ordering::SeqCst);
    }

    async fn disconnect(&self) {
        let listener = self.listener.lock().unwrap().take().unwrap();
        listener.abort();
        assert!(listener.await.unwrap_err().is_cancelled());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn enroute_serves_the_stock_git_edge_contract() {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/api/enroute/hooks", socket.local_addr().unwrap());
    let signing = SigningKey::from_seed(&[39; 32]);
    let server =
        super::common::Server::start_with_hooks(Some((&endpoint, &signing.to_pem().unwrap())))
            .await;
    let repository: BackendRepositoryId = "suite/physical".to_owned().try_into().unwrap();
    server
        .backend
        .create_repository(&repository, "refs/heads/main")
        .await
        .unwrap();
    let policy = Arc::new(FixturePolicy {
        repository: repository.clone(),
        denied: AtomicBool::new(false),
    });
    let adapter =
        HookAdapter::new(vec![signing.verifying_key()], &endpoint, policy.clone()).unwrap();
    let listener = tokio::spawn(async move {
        axum::serve(socket, hook_router(Arc::new(adapter)))
            .await
            .unwrap();
    });
    let wiring = Wiring {
        policy,
        listener: Mutex::new(Some(listener)),
    };
    let directory = tempfile::tempdir().unwrap();
    run(
        &server.backend,
        &repository,
        &format!("{}suite/project.git", server.git),
        &wiring,
        directory.path(),
    )
    .await;
}
