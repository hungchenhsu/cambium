//! Crash recovery and cold backup of both state directories (docs/backup.md).
//!
//! Enroute is killed with SIGKILL after acknowledged writes and restarted on
//! the same directory. Then both the metadata directory and Enroute's directory
//! are copied while stopped, and a fresh deployment on the copies must serve
//! the same data, replay the same idempotent responses, and accept new writes.

use std::{
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};

use cambium_backend_enroute::EnrouteBackend;
use cambium_server::{
    secrets::{new_token, token_hash},
    service::{App, Clock, Reply},
};
use cambium_store::{Bootstrap, SqliteStore, Store, TokenSettings};
use serde_json::{Value, json};

const NOW: i64 = 1_800_000_000_000;
const KEY: [u8; 32] = [42; 32];

struct FixedClock;
impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        NOW
    }
}

/// Enroute on a caller-owned directory, so it can be killed and restarted.
/// `inline_layer = 0` puts every layer in the object bucket, so both the node
/// state and the bucket must survive.
struct Enroute {
    child: Child,
    git: String,
    backend: EnrouteBackend,
}

impl Drop for Enroute {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Enroute {
    async fn start(root: &Path) -> Self {
        let binary =
            std::env::var("ENROUTE_BIN").expect("ENROUTE_BIN must name the patched binary");
        std::fs::create_dir_all(root).unwrap();
        let sockets: Vec<_> = (0..3)
            .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
            .collect();
        let ports: Vec<_> = sockets
            .iter()
            .map(|socket| socket.local_addr().unwrap().port())
            .collect();
        let api = format!("http://127.0.0.1:{}", ports[0]);
        let git = format!("http://127.0.0.1:{}/", ports[1]);
        let config = root.join("enroute.toml");
        std::fs::write(
            &config,
            format!(
                "[bucket]\nuri = 'file://{root}/objects'\n[node]\ndata_dir = '{root}/node'\ninternal_listen = '127.0.0.1:{internal}'\ncpu_threads = 2\n[listen]\napi = '127.0.0.1:{api}'\ngit = '127.0.0.1:{git}'\n[git]\nallow_private_remotes = true\ncreate_on_push = false\n[layers]\ninline_layer = 0\n",
                root = root.display(),
                internal = ports[2],
                api = ports[0],
                git = ports[1],
            ),
        )
        .unwrap();
        drop(sockets);
        let mut child = Command::new(binary)
            .args(["--config", &format!("file://{}", config.display()), "serve"])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        for _ in 0..150 {
            if let Ok(backend) = EnrouteBackend::connect(&api, &git, None).await {
                return Self {
                    child,
                    git,
                    backend,
                };
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "Enroute exited before readiness"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("Enroute did not become ready");
    }

    /// SIGKILL: no shutdown handler runs.
    fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

struct Deployment {
    enroute: Enroute,
    store: Arc<SqliteStore>,
    app: Arc<App>,
}

impl Deployment {
    async fn start(root: &Path) -> Self {
        let enroute = Enroute::start(&root.join("enroute")).await;
        std::fs::create_dir_all(root.join("metadata")).unwrap();
        let store = Arc::new(
            SqliteStore::open(
                root.join("metadata/metadata.sqlite"),
                TokenSettings::default(),
            )
            .await
            .unwrap(),
        );
        let app = Arc::new(App::new(
            store.clone(),
            Arc::new(enroute.backend.clone()),
            Arc::new(FixedClock),
            &KEY,
            enroute.git.clone(),
        ));
        app.recover_at_startup().await.unwrap();
        Self {
            enroute,
            store,
            app,
        }
    }

    async fn call(&self, token: &str, operation: &str, input: Value) -> Reply {
        self.app
            .call(token.to_owned(), operation.to_owned(), input, None)
            .await
    }

    async fn ok(&self, token: &str, operation: &str, input: Value) -> Value {
        let reply = self.call(token, operation, input).await;
        assert_eq!(reply.status, 200, "{operation}: {}", reply.body);
        reply.body
    }

    /// A cold stop: the store closes cleanly, then Enroute is killed.
    async fn stop(self) {
        self.store.close().await;
        self.enroute.kill();
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        let kind = entry.file_type().unwrap();
        assert!(!kind.is_symlink(), "unexpected symlink {:?}", entry.path());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    found.sort();
    found
}

#[tokio::test]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn acknowledged_writes_survive_sigkill_and_a_cold_copy_of_both_directories() {
    let original = tempfile::tempdir().unwrap();
    let deployment = Deployment::start(original.path()).await;
    let secret = new_token().unwrap();
    deployment
        .store
        .deployment_bootstrap(
            &Bootstrap {
                tenant_slug: "durable".into(),
                human_name: "Operator".into(),
                token_hash: token_hash(&secret).unwrap(),
                expires_at_ms: NOW + 86_400_000,
            },
            NOW,
        )
        .await
        .unwrap();
    let token = secret.as_str();
    let repository = deployment
        .ok(
            token,
            "create_repository",
            json!({"name": "project", "idempotency_key": "create"}),
        )
        .await;
    let repo = repository["repository_id"].clone();
    let write = json!({"repository_id": repo, "message": "Durable\n", "changes": [{"action": "put", "path": "a.txt", "contents": "acknowledged\n"}], "provenance": {"model": "example-model-1"}, "idempotency_key": "write-1"});
    let written = deployment.ok(token, "write_changeset", write.clone()).await;

    // SIGKILL Enroute right after the acknowledged write, then restart on the same data.
    let Deployment { enroute, store, .. } = deployment;
    store.close().await;
    enroute.kill();
    let deployment = Deployment::start(original.path()).await;
    let check = |deployment: &Deployment| {
        let (repo, written, write) = (repo.clone(), written.clone(), write.clone());
        let app = deployment.app.clone();
        async move {
            let read = app
                .call(
                    token.to_owned(),
                    "read_file".into(),
                    json!({"repository_id": repo, "path": "a.txt", "revision": written["commit_id"]}),
                    None,
                )
                .await;
            assert_eq!(read.status, 200, "{}", read.body);
            assert_eq!(read.body["contents"], "acknowledged\n");
            assert_eq!(read.body["revision"], written["commit_id"]);
            let replay = app
                .call(token.to_owned(), "write_changeset".into(), write, None)
                .await;
            assert!(replay.replayed, "the stored response is replayed");
            assert_eq!(replay.body, written);
            let provenance = app
                .call(
                    token.to_owned(),
                    "get_commit_provenance".into(),
                    json!({"repository_id": repo, "commit_id": written["commit_id"]}),
                    None,
                )
                .await;
            assert_eq!(
                provenance.body["claims"]["model"], "example-model-1",
                "{}",
                provenance.body
            );
        }
    };
    check(&deployment).await;
    let second = deployment
        .ok(token, "write_changeset", json!({"repository_id": repo, "message": "After restart\n", "changes": [{"action": "put", "path": "b.txt", "contents": "b\n"}], "idempotency_key": "write-2"}))
        .await;
    assert_eq!(second["parent_commit_id"], written["commit_id"]);

    // Cold backup: stop everything, copy both directories, start on the copy.
    deployment.stop().await;
    let restored = tempfile::tempdir().unwrap();
    copy_tree(original.path(), restored.path());
    assert_eq!(files(original.path()), files(restored.path()));
    let copy = Deployment::start(restored.path()).await;
    check(&copy).await;
    let tip = copy
        .ok(
            token,
            "read_file",
            json!({"repository_id": repo, "path": "b.txt"}),
        )
        .await;
    assert_eq!(tip["revision"], second["commit_id"]);
    let after = copy
        .ok(token, "write_changeset", json!({"repository_id": repo, "message": "On the copy\n", "changes": [{"action": "put", "path": "c.txt", "contents": "c\n"}], "idempotency_key": "write-3"}))
        .await;
    assert_eq!(after["parent_commit_id"], second["commit_id"]);
    copy.stop().await;

    // The original is untouched by writes to the copy.
    let original_again = Deployment::start(original.path()).await;
    let missing = original_again
        .call(
            token,
            "read_file",
            json!({"repository_id": repo, "path": "c.txt"}),
        )
        .await;
    assert_eq!(
        missing.body["error"]["code"], "not_found",
        "{}",
        missing.body
    );
    original_again.stop().await;
}
