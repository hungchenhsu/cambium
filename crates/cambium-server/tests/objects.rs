use std::{
    collections::HashMap,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use async_trait::async_trait;
use cambium_backend::*;
use cambium_backend_conformance::paths::PATH_CORPUS;
use cambium_protocol::ObjectId;
use cambium_server::objects::{GitObjects, collisions, validate_path};

struct Fixture {
    objects: HashMap<ObjectId, RawObject>,
    tip: ObjectId,
}

#[async_trait]
impl Backend for Fixture {
    fn capabilities(&self) -> Capabilities {
        unreachable!()
    }
    async fn read_object(
        &self,
        _: &BackendRepositoryId,
        id: &ObjectId,
    ) -> Result<RawObject, BackendError> {
        self.objects
            .get(id)
            .cloned()
            .ok_or(BackendError::ObjectNotFound)
    }
    async fn list_refs(
        &self,
        _: &BackendRepositoryId,
        _: &[String],
    ) -> Result<RefSnapshot, BackendError> {
        Ok(RefSnapshot {
            head: "refs/heads/main".into(),
            refs: vec![GitRef {
                name: "refs/heads/main".into(),
                object_id: self.tip.clone(),
            }],
        })
    }
    async fn create_repository(
        &self,
        _: &BackendRepositoryId,
        _: &str,
    ) -> Result<(), BackendError> {
        unreachable!()
    }
    async fn delete_repository(&self, _: &BackendRepositoryId) -> Result<(), BackendError> {
        unreachable!()
    }
    async fn seal_repository(&self, _: &BackendRepositoryId) -> Result<(), BackendError> {
        unreachable!()
    }
    async fn list_repositories(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<RepositoryPage, BackendError> {
        unreachable!()
    }
    async fn update_refs(
        &self,
        _: &BackendRepositoryId,
        _: &[RefUpdate],
    ) -> Result<Vec<RefOutcome>, BackendError> {
        unreachable!()
    }
    async fn is_ancestor(
        &self,
        _: &BackendRepositoryId,
        _: &ObjectId,
        _: &ObjectId,
    ) -> Result<bool, BackendError> {
        unreachable!()
    }
    async fn create_commit(
        &self,
        _: &BackendRepositoryId,
        _: &CommitRequest,
    ) -> Result<CommitResult, BackendError> {
        unreachable!()
    }
    async fn copy_refs(
        &self,
        _: &BackendRepositoryId,
        _: &BackendRepositoryId,
        _: &[RefCopy],
        _: bool,
    ) -> Result<(), BackendError> {
        unreachable!()
    }
}

fn git(directory: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    let mut child = command
        .current_dir(directory)
        .args([
            "-c",
            "core.precomposeUnicode=false",
            "-c",
            "core.ignorecase=false",
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", directory.join("no-config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}
fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap().trim().to_owned()
}

#[tokio::test]
async fn git_corpus_reads_exact_bytes_and_diff_preserves_mode_only_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    git(
        path,
        &["init", "--bare", "--object-format=sha1", "--template="],
        b"",
    );
    let mut index = Vec::new();
    let mut blob_ids = HashMap::new();
    for item in PATH_CORPUS {
        let blob = text(git(path, &["hash-object", "-w", "--stdin"], item.contents));
        index.extend_from_slice(format!("{:o} {blob}\t", item.mode).as_bytes());
        index.extend_from_slice(item.path);
        index.push(0);
        blob_ids.insert(item.path.to_vec(), blob);
    }
    git(path, &["update-index", "-z", "--index-info"], &index);
    let tree = text(git(path, &["write-tree"], b""));
    let first = text(git(path, &["commit-tree", &tree], b"Corpus\n"));
    let mode_change = format!("100755 {}\tREADME\0", blob_ids[b"README".as_slice()]);
    git(
        path,
        &["update-index", "-z", "--index-info"],
        mode_change.as_bytes(),
    );
    let second_tree = text(git(path, &["write-tree"], b""));
    let second = text(git(
        path,
        &["commit-tree", &second_tree, "-p", &first],
        b"Mode only\n",
    ));
    git(path, &["update-ref", "refs/heads/main", &second], b"");
    git(
        path,
        &["tag", "-a", "release", "-m", "Annotated tag", &second],
        b"",
    );
    let tag = text(git(path, &["rev-parse", "refs/tags/release"], b""));
    let ids = text(git(
        path,
        &["rev-list", "--objects", "--no-object-names", "--all"],
        b"",
    ));
    let mut objects = HashMap::new();
    for id in ids.lines() {
        let kind = text(git(path, &["cat-file", "-t", id], b""));
        let bytes = git(path, &["cat-file", &kind, id], b"");
        let kind = match kind.as_str() {
            "blob" => ObjectKind::Blob,
            "tree" => ObjectKind::Tree,
            "commit" => ObjectKind::Commit,
            "tag" => ObjectKind::Tag,
            _ => panic!(),
        };
        objects.insert(id.parse().unwrap(), RawObject { kind, bytes });
    }
    let mut fixture = Fixture {
        objects,
        tip: second.parse().unwrap(),
    };
    let repository = "corpus".to_owned().try_into().unwrap();
    let reads = GitObjects {
        backend: &fixture,
        repository: &repository,
    };
    for revision in ["HEAD", "main", "refs/heads/main", &second] {
        assert_eq!(reads.resolve(revision).await.unwrap(), fixture.tip);
    }
    let first = first.parse().unwrap();
    let second = second.parse().unwrap();
    let entries = reads.tree(&first, true).await.unwrap();
    let leaves: Vec<_> = entries.iter().filter(|entry| !entry.is_tree()).collect();
    assert_eq!(leaves.len(), PATH_CORPUS.len());
    for item in PATH_CORPUS {
        let entry = leaves.iter().find(|entry| entry.path == item.path).unwrap();
        assert_eq!(entry.mode, item.mode);
        let (read_entry, bytes) = reads.file(&first, item.path).await.unwrap();
        assert_eq!(read_entry, **entry);
        assert_eq!(bytes, item.contents);
    }
    assert_eq!(collisions(&entries).len(), 2);
    assert_eq!(
        reads
            .root_tree(&tag.parse().unwrap())
            .await
            .unwrap()
            .as_str(),
        second_tree
    );
    let differences = reads.diff(&first, &second).await.unwrap();
    assert_eq!(differences.len(), 1);
    assert_eq!(differences[0].path, b"README");
    assert_eq!(
        differences[0].old.as_ref().unwrap().object_id,
        differences[0].new.as_ref().unwrap().object_id
    );
    assert_eq!(differences[0].old.as_ref().unwrap().mode, 0o100644);
    assert_eq!(differences[0].new.as_ref().unwrap().mode, 0o100755);
    assert!(matches!(
        reads.file(&first, b"readme-link/child").await,
        Err(BackendError::ObjectNotFound)
    ));
    assert!(matches!(
        reads.file(&first, b"../README").await,
        Err(BackendError::InvalidRequest { .. })
    ));
    // An adapter that returns corrupted bytes must not reach a parser or caller.
    fixture.objects.get_mut(&first).unwrap().bytes.push(0);
    let reads = GitObjects {
        backend: &fixture,
        repository: &repository,
    };
    assert!(matches!(
        reads.root_tree(&first).await,
        Err(BackendError::Internal)
    ));
}

#[tokio::test]
async fn a_file_listed_at_the_tree_depth_limit_can_be_read() {
    let content = b"deep file";
    let mut child =
        gix_object::compute_hash(gix_hash::Kind::Sha1, gix_object::Kind::Blob, content).unwrap();
    let blob_id: ObjectId = child.to_string().parse().unwrap();
    let mut objects = HashMap::from([(
        blob_id.clone(),
        RawObject {
            kind: ObjectKind::Blob,
            bytes: content.to_vec(),
        },
    )]);
    let mut roots = Vec::new();
    for directories in 0..=257 {
        let mut bytes = if directories == 0 {
            b"100644 leaf\0".to_vec()
        } else {
            b"40000 d\0".to_vec()
        };
        bytes.extend_from_slice(child.as_slice());
        child =
            gix_object::compute_hash(gix_hash::Kind::Sha1, gix_object::Kind::Tree, &bytes).unwrap();
        let id: ObjectId = child.to_string().parse().unwrap();
        objects.insert(
            id.clone(),
            RawObject {
                kind: ObjectKind::Tree,
                bytes,
            },
        );
        roots.push(id);
    }
    let fixture = Fixture {
        objects,
        tip: roots[256].clone(),
    };
    let repository = "deep".to_owned().try_into().unwrap();
    let reads = GitObjects {
        backend: &fixture,
        repository: &repository,
    };
    let listed = reads.tree(&roots[256], true).await.unwrap();
    let leaf = listed.iter().find(|entry| !entry.is_tree()).unwrap();
    assert_eq!(leaf.path, format!("{}leaf", "d/".repeat(256)).as_bytes());
    let (entry, data) = reads.file(&roots[256], &leaf.path).await.unwrap();
    assert_eq!(&entry, leaf);
    assert_eq!(entry.object_id, blob_id);
    assert_eq!(data, content);
    assert!(matches!(
        reads.tree(&roots[257], true).await,
        Err(BackendError::InvalidRequest { .. })
    ));
    let too_deep = format!("{}leaf", "d/".repeat(257));
    assert!(matches!(
        reads.file(&roots[257], too_deep.as_bytes()).await,
        Err(BackendError::InvalidRequest { .. })
    ));
}

#[test]
fn the_path_length_limit_counts_bytes_before_traversal() {
    let mut path = "é".repeat(32_768).into_bytes();
    validate_path(&path).unwrap();
    path.push(b'x');
    assert!(matches!(
        validate_path(&path),
        Err(BackendError::InvalidRequest { .. })
    ));
}
