use std::{
    io::Write,
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};

use cambium_backend::*;
use cambium_backend_conformance::paths::PATH_CORPUS;
use cambium_backend_enroute::EnrouteBackend;
use cambium_protocol::ObjectId;

struct Server {
    child: Child,
    _directory: tempfile::TempDir,
    git: String,
    backend: EnrouteBackend,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    async fn start() -> Self {
        let binary =
            std::env::var("ENROUTE_BIN").expect("ENROUTE_BIN must name the patched binary");
        let directory = tempfile::tempdir().unwrap();
        let sockets: Vec<_> = (0..3)
            .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
            .collect();
        let ports: Vec<_> = sockets
            .iter()
            .map(|socket| socket.local_addr().unwrap().port())
            .collect();
        let api = format!("http://127.0.0.1:{}", ports[0]);
        let git = format!("http://127.0.0.1:{}/", ports[1]);
        let config = directory.path().join("enroute.toml");
        std::fs::write(&config, format!(
            "[bucket]\nuri = 'file://{root}/objects'\n[node]\ndata_dir = '{root}/node'\ninternal_listen = '127.0.0.1:{internal}'\ncpu_threads = 2\n[listen]\napi = '127.0.0.1:{api}'\ngit = '127.0.0.1:{git}'\n[git]\nallow_private_remotes = true\n",
            root = directory.path().display(), internal = ports[2], api = ports[0], git = ports[1],
        )).unwrap();
        drop(sockets);
        let mut child = Command::new(binary)
            .args(["--config", &format!("file://{}", config.display()), "serve"])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        for _ in 0..100 {
            if let Ok(backend) = EnrouteBackend::connect(&api, &git, None).await {
                return Self {
                    child,
                    _directory: directory,
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
        let _ = child.kill();
        let _ = child.wait();
        panic!("Enroute did not become ready");
    }
}

fn id(value: &str) -> BackendRepositoryId {
    value.to_owned().try_into().unwrap()
}
fn unknown() -> ObjectId {
    "f".repeat(40).parse().unwrap()
}
fn request(parent: Option<ObjectId>) -> CommitRequest {
    let identity = GitIdentity {
        name: "Conformance".into(),
        email: "test@example.invalid".into(),
        timestamp_seconds: 1_700_000_000,
        utc_offset_seconds: 0,
    };
    CommitRequest {
        branch: "refs/heads/main".into(),
        expected_parent: parent,
        message: "Conformance\n".into(),
        author: identity.clone(),
        committer: identity,
        changes: vec![
            FileChange::Put {
                path: b"nested/file".to_vec(),
                contents: b"content\0\xff".to_vec(),
                mode: FileMode::Regular,
            },
            FileChange::Put {
                path: b"run".to_vec(),
                contents: b"#!/bin/sh\n".to_vec(),
                mode: FileMode::Executable,
            },
            FileChange::Put {
                path: b"link".to_vec(),
                contents: b"nested/file".to_vec(),
                mode: FileMode::Symlink,
            },
        ],
    }
}

#[tokio::test]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn real_backend_lifecycle_cas_copy_and_seal() {
    let server = Server::start().await;
    let backend = &server.backend;
    let source = id("conformance/source");
    let destination = id("conformance/destination");
    backend
        .create_repository(&source, "refs/heads/main")
        .await
        .unwrap();
    backend
        .create_repository(&source, "refs/heads/ignored")
        .await
        .unwrap();
    assert_eq!(
        backend.list_refs(&source, &[]).await.unwrap().head,
        "refs/heads/main"
    );
    let first = backend
        .create_commit(&source, &request(None))
        .await
        .unwrap();
    assert!(first.parent_commit_id.is_none());
    assert!(
        matches!(backend.create_commit(&source, &request(None)).await, Err(BackendError::StaleExpectation { actual: Some(ref actual) }) if actual == &first.commit_id)
    );
    let mut invalid = request(Some(first.commit_id.clone()));
    invalid.changes = vec![FileChange::Put {
        path: b"bad\xff".to_vec(),
        contents: vec![1],
        mode: FileMode::Regular,
    }];
    assert!(matches!(
        backend.create_commit(&source, &invalid).await,
        Err(BackendError::UnsupportedPath { .. })
    ));
    let mut next = request(Some(first.commit_id.clone()));
    next.changes = vec![FileChange::Delete {
        path: b"nested/file".to_vec(),
    }];
    let second = backend.create_commit(&source, &next).await.unwrap();
    assert_eq!(second.parent_commit_id.as_ref(), Some(&first.commit_id));
    assert!(
        backend
            .is_ancestor(&source, &first.commit_id, &second.commit_id)
            .await
            .unwrap()
    );
    assert!(
        !backend
            .is_ancestor(&source, &unknown(), &second.commit_id)
            .await
            .unwrap()
    );
    assert!(matches!(
        backend.read_object(&source, &unknown()).await,
        Err(BackendError::ObjectNotFound)
    ));
    assert!(matches!(
        backend.read_object(&id("missing"), &unknown()).await,
        Err(BackendError::RepositoryNotFound)
    ));

    let updates = [
        RefUpdate {
            name: "refs/heads/new".into(),
            expected: RefExpectation::Absent,
            new: Some(first.commit_id.clone()),
        },
        RefUpdate {
            name: "refs/heads/new".into(),
            expected: RefExpectation::Absent,
            new: Some(first.commit_id.clone()),
        },
        RefUpdate {
            name: "refs/heads/new".into(),
            expected: RefExpectation::At(second.commit_id.clone()),
            new: None,
        },
        RefUpdate {
            name: "refs/heads/new".into(),
            expected: RefExpectation::Absent,
            new: None,
        },
        RefUpdate {
            name: "refs/heads/absent".into(),
            expected: RefExpectation::Absent,
            new: None,
        },
        RefUpdate {
            name: "refs/heads/new".into(),
            expected: RefExpectation::Any,
            new: Some(second.commit_id.clone()),
        },
        RefUpdate {
            name: "refs/heads/new".into(),
            expected: RefExpectation::Any,
            new: None,
        },
        RefUpdate {
            name: "refs/heads/new".into(),
            expected: RefExpectation::Any,
            new: None,
        },
        RefUpdate {
            name: "refs/heads/missing".into(),
            expected: RefExpectation::Absent,
            new: Some(unknown()),
        },
        RefUpdate {
            name: "refs/heads/bad..name".into(),
            expected: RefExpectation::Absent,
            new: Some(second.commit_id.clone()),
        },
    ];
    let results = backend.update_refs(&source, &updates).await.unwrap();
    assert_eq!(
        results
            .iter()
            .map(|result| result.rejection)
            .collect::<Vec<_>>(),
        vec![
            None,
            Some(RefRejection::AlreadyExists),
            Some(RefRejection::Stale),
            Some(RefRejection::AlreadyExists),
            None,
            None,
            None,
            None,
            Some(RefRejection::MissingObject),
            Some(RefRejection::InvalidName)
        ]
    );
    backend
        .create_repository(&destination, "refs/heads/main")
        .await
        .unwrap();
    backend
        .copy_refs(&source, &destination, &[], true)
        .await
        .unwrap();
    assert!(
        backend
            .list_refs(&destination, &[])
            .await
            .unwrap()
            .refs
            .is_empty()
    );
    assert!(backend.capabilities().durable_ref_creation);
    let receipt_ref = "refs/heads/receipt";
    assert!(
        backend
            .create_ref_once(&source, "created", receipt_ref, &first.commit_id)
            .await
            .unwrap()
    );
    assert!(
        !backend
            .create_ref_once(&source, "refused", receipt_ref, &first.commit_id)
            .await
            .unwrap()
    );
    let removed = backend
        .update_refs(
            &source,
            &[RefUpdate {
                name: receipt_ref.into(),
                expected: RefExpectation::At(first.commit_id.clone()),
                new: None,
            }],
        )
        .await
        .unwrap();
    assert!(removed[0].rejection.is_none());
    assert!(
        backend
            .create_ref_once(&source, "created", receipt_ref, &first.commit_id)
            .await
            .unwrap()
    );
    assert!(
        !backend
            .create_ref_once(&source, "refused", receipt_ref, &first.commit_id)
            .await
            .unwrap()
    );
    assert!(
        backend
            .list_refs(&source, &[receipt_ref.into()])
            .await
            .unwrap()
            .refs
            .is_empty()
    );
    assert!(matches!(
        backend
            .create_ref_once(&source, "created", receipt_ref, &second.commit_id)
            .await,
        Err(BackendError::InvalidRequest { .. })
    ));
    backend.seal_repository(&source).await.unwrap();
    backend.seal_repository(&source).await.unwrap();
    assert!(
        backend
            .create_ref_once(&source, "created", receipt_ref, &first.commit_id)
            .await
            .unwrap()
    );
    assert!(matches!(
        backend
            .create_ref_once(&source, "after-seal", receipt_ref, &first.commit_id)
            .await,
        Err(BackendError::RepositorySealed)
    ));
    assert!(matches!(
        backend
            .create_commit(&source, &request(Some(second.commit_id.clone())))
            .await,
        Err(BackendError::RepositorySealed)
    ));
    assert!(matches!(
        backend.update_refs(&source, &updates[..1]).await,
        Err(BackendError::RepositorySealed)
    ));
    backend
        .copy_refs(
            &source,
            &destination,
            &[RefCopy {
                source: "refs/heads/main".into(),
                destination: "refs/heads/copied".into(),
                allow_non_fast_forward: false,
            }],
            true,
        )
        .await
        .unwrap();
    assert_eq!(
        backend.list_refs(&destination, &[]).await.unwrap().refs,
        vec![GitRef {
            name: "refs/heads/copied".into(),
            object_id: second.commit_id.clone()
        }]
    );
    for object in [
        &first.commit_id,
        &first.tree_id,
        &second.commit_id,
        &second.tree_id,
    ] {
        assert_eq!(
            backend.read_object(&source, object).await.unwrap(),
            backend.read_object(&destination, object).await.unwrap()
        );
    }
    backend.seal_repository(&destination).await.unwrap();
    let sealed_copy = backend
        .copy_refs(
            &source,
            &destination,
            &[RefCopy {
                source: "refs/heads/main".into(),
                destination: "refs/heads/after-seal".into(),
                allow_non_fast_forward: false,
            }],
            true,
        )
        .await;
    assert!(
        matches!(sealed_copy, Err(BackendError::RepositorySealed)),
        "{sealed_copy:?}"
    );
    let listed = backend
        .list_repositories("conformance/", None)
        .await
        .unwrap();
    assert_eq!(listed.repositories.len(), 2);
    backend.delete_repository(&source).await.unwrap();
    backend.delete_repository(&source).await.unwrap();
    assert!(matches!(
        backend.seal_repository(&source).await,
        Err(BackendError::RepositoryNotFound)
    ));
    backend
        .create_repository(&source, "refs/heads/main")
        .await
        .unwrap();
    assert!(
        backend
            .list_refs(&source, &[])
            .await
            .unwrap()
            .refs
            .is_empty()
    );
    backend
        .create_commit(&source, &request(None))
        .await
        .unwrap();
}

fn git(directory: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(directory)
        .args([
            "-c",
            "core.ignorecase=false",
            "-c",
            "core.precomposeUnicode=false",
            "-c",
            "user.name=Conformance",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("no-config"))
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}
fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap().trim().to_owned()
}

#[tokio::test]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn stock_git_corpus_survives_raw_reads_and_backend_copy() {
    let server = Server::start().await;
    let source = id("paths/source");
    let destination = id("paths/destination");
    server
        .backend
        .create_repository(&source, "refs/heads/main")
        .await
        .unwrap();
    server
        .backend
        .create_repository(&destination, "refs/heads/main")
        .await
        .unwrap();
    let local = tempfile::tempdir().unwrap();
    let directory = local.path();
    git(
        directory,
        &["init", "--bare", "--object-format=sha1", "--template="],
        b"",
    );
    let mut index = Vec::new();
    for fixture in PATH_CORPUS {
        let object = text(git(
            directory,
            &["hash-object", "-w", "--stdin"],
            fixture.contents,
        ));
        index.extend_from_slice(format!("{:o} {object}\t", fixture.mode).as_bytes());
        index.extend_from_slice(fixture.path);
        index.push(0);
    }
    git(directory, &["update-index", "-z", "--index-info"], &index);
    let tree = text(git(directory, &["write-tree"], b""));
    let commit = text(git(directory, &["commit-tree", &tree], b"Path corpus\n"));
    git(directory, &["update-ref", "refs/heads/main", &commit], b"");
    git(
        directory,
        &[
            "push",
            &format!("{}{}.git", server.git, source.as_str()),
            "refs/heads/main",
        ],
        b"",
    );
    server
        .backend
        .copy_refs(
            &source,
            &destination,
            &[RefCopy {
                source: "refs/heads/main".into(),
                destination: "refs/heads/main".into(),
                allow_non_fast_forward: false,
            }],
            true,
        )
        .await
        .unwrap();
    let objects = text(git(
        directory,
        &[
            "rev-list",
            "--objects",
            "--no-object-names",
            "refs/heads/main",
        ],
        b"",
    ));
    for object in objects.lines() {
        let kind = text(git(directory, &["cat-file", "-t", object], b""));
        let raw = git(directory, &["cat-file", &kind, object], b"");
        for repository in [&source, &destination] {
            let returned = server
                .backend
                .read_object(repository, &object.parse().unwrap())
                .await
                .unwrap();
            assert_eq!(returned.kind.as_str(), kind);
            assert_eq!(returned.bytes, raw);
        }
    }
    git(
        directory,
        &[
            "fetch",
            &format!("{}{}.git", server.git, destination.as_str()),
            "refs/heads/main",
        ],
        b"",
    );
    assert_eq!(
        text(git(directory, &["rev-parse", "FETCH_HEAD"], b"")),
        commit
    );
    git(directory, &["fsck", "--strict"], b"");
}
