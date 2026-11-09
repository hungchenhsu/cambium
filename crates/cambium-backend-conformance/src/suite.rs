//! Backend-independent conformance checks (ADR-0007).
//!
//! Each check takes a backend and a [`Namespace`] of fresh repository IDs, and panics
//! with a description of the first violated contract. Adapters call [`run`]
//! from an integration test against a real, pinned backend.

use cambium_backend::{
    Backend, BackendError, BackendRepositoryId, CommitRequest, FileChange, FileMode, GitIdentity,
    ObjectKind, RefCopy, RefExpectation, RefRejection, RefUpdate,
};
use cambium_protocol::ObjectId;
use sha1::{Digest as _, Sha1};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::paths::PATH_CORPUS;

/// Allocates fresh backend repository IDs under one prefix that no other
/// process uses, so `list_repositories` can be checked exactly.
pub struct Namespace {
    prefix: String,
    next: AtomicUsize,
}

impl Namespace {
    /// `prefix` is a valid backend repository ID, such as `conformance/run-1`.
    pub fn new(prefix: &str) -> Self {
        BackendRepositoryId::try_from(prefix.to_owned()).expect("a valid ID prefix");
        Self {
            prefix: format!("{prefix}/"),
            next: AtomicUsize::new(0),
        }
    }

    pub fn fresh(&self) -> BackendRepositoryId {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        format!("{}r{n}", self.prefix)
            .try_into()
            .expect("namespace IDs are valid")
    }

    /// Every ID this namespace allocates starts with this prefix.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }
}

const MAIN: &str = "refs/heads/main";

/// Runs every backend-independent check in ADR-0007's suite.
///
/// Section 5's stock-git push and section 7's git edge need a git endpoint and
/// policy wiring, so adapters cover them in their own tests.
pub async fn run(backend: &dyn Backend, ids: &Namespace) {
    lifecycle(backend, ids).await;
    refs(backend, ids).await;
    commits(backend, ids).await;
    raw_reads(backend, ids).await;
    exact_paths(backend, ids).await;
    copy(backend, ids).await;
    if backend.capabilities().durable_seal {
        sealing(backend, ids).await;
    }
    if backend.capabilities().durable_ref_creation {
        ref_receipts(backend, ids).await;
    }
}

fn identity() -> GitIdentity {
    GitIdentity {
        name: "Conformance".into(),
        email: "conformance@example.invalid".into(),
        timestamp_seconds: 1_700_000_000,
        utc_offset_seconds: 0,
    }
}

fn commit_request(
    branch: &str,
    parent: Option<ObjectId>,
    changes: Vec<FileChange>,
) -> CommitRequest {
    CommitRequest {
        branch: branch.into(),
        expected_parent: parent,
        message: "Conformance\n".into(),
        author: identity(),
        committer: identity(),
        changes,
    }
}

fn put(path: &[u8], contents: &[u8], mode: FileMode) -> FileChange {
    FileChange::Put {
        path: path.to_vec(),
        contents: contents.to_vec(),
        mode,
    }
}

fn unknown_object() -> ObjectId {
    "f".repeat(40).parse().expect("a valid object ID")
}

async fn fresh(backend: &dyn Backend, ids: &Namespace) -> BackendRepositoryId {
    let id = ids.fresh();
    backend
        .create_repository(&id, MAIN)
        .await
        .expect("create_repository succeeds for a fresh ID");
    id
}

async fn tip(backend: &dyn Backend, id: &BackendRepositoryId, name: &str) -> Option<ObjectId> {
    backend
        .list_refs(id, &[name.to_owned()])
        .await
        .expect("list_refs succeeds")
        .refs
        .into_iter()
        .find(|r| r.name == name)
        .map(|r| r.object_id)
}

async fn listed(backend: &dyn Backend, prefix: &str) -> Vec<BackendRepositoryId> {
    let mut all = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let page = backend
            .list_repositories(prefix, token.as_deref())
            .await
            .expect("list_repositories succeeds");
        all.extend(page.repositories);
        match page.next_page_token {
            Some(next) => token = Some(next),
            None => break,
        }
    }
    all.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    all
}

/// ADR-0007 section 1: create and delete are idempotent, prefix listing is
/// exact, and a deleted ID can be created again empty.
pub async fn lifecycle(backend: &dyn Backend, ids: &Namespace) {
    let id = fresh(backend, ids).await;
    backend
        .create_repository(&id, MAIN)
        .await
        .expect("a repeated create_repository succeeds");
    let first = backend
        .create_commit(
            &id,
            &commit_request(MAIN, None, vec![put(b"a", b"a", FileMode::Regular)]),
        )
        .await
        .expect("a root commit succeeds");
    backend
        .create_repository(&id, MAIN)
        .await
        .expect("create_repository after a write succeeds");
    assert_eq!(
        tip(backend, &id, MAIN).await,
        Some(first.commit_id.clone()),
        "a repeated create_repository changes nothing"
    );
    let other = fresh(backend, ids).await;
    let mut expected = vec![id.clone(), other.clone()];
    expected.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    assert_eq!(
        listed(backend, ids.prefix()).await,
        expected,
        "list_repositories returns every ID under the prefix"
    );
    backend
        .delete_repository(&other)
        .await
        .expect("delete succeeds");
    backend
        .delete_repository(&id)
        .await
        .expect("delete succeeds");
    assert!(
        listed(backend, ids.prefix()).await.is_empty(),
        "deleted IDs are no longer listed"
    );
    backend
        .delete_repository(&id)
        .await
        .expect("a repeated delete succeeds");
    assert!(
        matches!(
            backend.list_refs(&id, &[]).await,
            Err(BackendError::RepositoryNotFound)
        ),
        "a deleted ID stops resolving"
    );
    backend
        .create_repository(&id, MAIN)
        .await
        .expect("a deleted ID can be created again");
    let snapshot = backend
        .list_refs(&id, &[])
        .await
        .expect("list_refs succeeds");
    assert!(
        snapshot.refs.is_empty(),
        "a recreated repository starts empty"
    );
    assert!(
        matches!(
            backend.read_object(&id, &first.commit_id).await,
            Err(BackendError::ObjectNotFound)
        ),
        "a recreated repository does not hold the old objects"
    );
    backend.delete_repository(&id).await.expect("cleanup");
}

/// ADR-0007 section 2: every expectation and rejection reason, outcomes per
/// update in request order, and `is_ancestor` answering false for an unknown
/// commit.
pub async fn refs(backend: &dyn Backend, ids: &Namespace) {
    let id = fresh(backend, ids).await;
    let first = backend
        .create_commit(
            &id,
            &commit_request(MAIN, None, vec![put(b"a", b"1", FileMode::Regular)]),
        )
        .await
        .expect("a root commit succeeds");
    let second = backend
        .create_commit(
            &id,
            &commit_request(
                MAIN,
                Some(first.commit_id.clone()),
                vec![put(b"a", b"2", FileMode::Regular)],
            ),
        )
        .await
        .expect("a child commit succeeds");
    let update = |name: &str, expected: RefExpectation, new: Option<&ObjectId>| RefUpdate {
        name: name.into(),
        expected,
        new: new.cloned(),
    };
    let topic = "refs/heads/topic";
    let updates = [
        update(topic, RefExpectation::Absent, Some(&first.commit_id)),
        update(topic, RefExpectation::Absent, Some(&second.commit_id)),
        update(
            topic,
            RefExpectation::At(second.commit_id.clone()),
            Some(&first.commit_id),
        ),
        update(
            topic,
            RefExpectation::At(first.commit_id.clone()),
            Some(&second.commit_id),
        ),
        update(topic, RefExpectation::Any, Some(&first.commit_id)),
        update(topic, RefExpectation::Absent, None),
        update("refs/heads/never", RefExpectation::Absent, None),
        update("refs/heads/never", RefExpectation::Any, None),
        update(
            "refs/heads/missing",
            RefExpectation::Absent,
            Some(&unknown_object()),
        ),
        update(
            "refs/heads/bad..name",
            RefExpectation::Absent,
            Some(&first.commit_id),
        ),
        update(
            "refs/heads/tree",
            RefExpectation::Absent,
            Some(&first.tree_id),
        ),
    ];
    let outcomes = backend
        .update_refs(&id, &updates)
        .await
        .expect("update_refs reports per-update outcomes");
    assert_eq!(
        outcomes.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(),
        updates.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(),
        "outcomes are reported in request order"
    );
    let rejections: Vec<_> = outcomes.iter().map(|o| o.rejection).collect();
    let expected = [
        None,
        Some(RefRejection::AlreadyExists),
        Some(RefRejection::Stale),
        None,
        None,
        Some(RefRejection::AlreadyExists),
        None,
        None,
        Some(RefRejection::MissingObject),
        Some(RefRejection::InvalidName),
    ];
    assert_eq!(
        &rejections[..10],
        &expected,
        "rejection reasons follow ADR-0007"
    );
    // A backend that cannot tell a non-commit from a missing object reports the broader one.
    assert!(
        matches!(
            rejections[10],
            Some(RefRejection::NotACommit | RefRejection::MissingObject)
        ),
        "a branch pointed at a tree is rejected: {:?}",
        rejections[10]
    );
    assert_eq!(
        tip(backend, &id, topic).await,
        Some(first.commit_id.clone()),
        "rejected updates change nothing"
    );
    assert_eq!(tip(backend, &id, "refs/heads/tree").await, None);
    assert!(
        backend
            .is_ancestor(&id, &first.commit_id, &second.commit_id)
            .await
            .expect("is_ancestor succeeds")
    );
    assert!(
        !backend
            .is_ancestor(&id, &second.commit_id, &first.commit_id)
            .await
            .expect("is_ancestor succeeds")
    );
    assert!(
        !backend
            .is_ancestor(&id, &unknown_object(), &second.commit_id)
            .await
            .expect("an unknown commit is not an error"),
        "an unknown commit is never an ancestor"
    );
    let snapshot = backend
        .list_refs(&id, &["refs/heads/".to_owned()])
        .await
        .expect("list_refs succeeds");
    let mut names: Vec<_> = snapshot.refs.iter().map(|r| r.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [MAIN, topic],
        "list_refs returns every ref under the prefix"
    );
    assert_eq!(snapshot.head, MAIN, "HEAD points at the default branch");
    backend.delete_repository(&id).await.expect("cleanup");
}

/// One parsed tree entry: mode, name bytes, and object ID.
type Entry = (u32, Vec<u8>, ObjectId);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_tree(bytes: &[u8]) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .position(|b| *b == b' ')
            .expect("tree entry has a mode");
        let mode = u32::from_str_radix(std::str::from_utf8(&rest[..space]).expect("octal mode"), 8)
            .expect("octal mode");
        let nul = rest
            .iter()
            .position(|b| *b == 0)
            .expect("tree entry has a name");
        let name = rest[space + 1..nul].to_vec();
        let id = hex(&rest[nul + 1..nul + 21])
            .parse()
            .expect("tree entry has an object ID");
        entries.push((mode, name, id));
        rest = &rest[nul + 21..];
    }
    entries
}

fn commit_tree(bytes: &[u8]) -> ObjectId {
    let text = std::str::from_utf8(bytes).expect("generated commit headers are UTF-8");
    text.lines()
        .next()
        .and_then(|line| line.strip_prefix("tree "))
        .expect("a commit starts with its tree")
        .parse()
        .expect("a tree object ID")
}

/// Reads an object and checks that its bytes hash to its ID (section 4).
async fn verified(
    backend: &dyn Backend,
    id: &BackendRepositoryId,
    object: &ObjectId,
    kind: ObjectKind,
) -> Vec<u8> {
    let raw = backend
        .read_object(id, object)
        .await
        .expect("read_object succeeds");
    assert_eq!(raw.kind, kind, "object {object} has the expected kind");
    let mut hasher = Sha1::new();
    hasher.update(format!("{} {}\0", raw.kind.as_str(), raw.bytes.len()));
    hasher.update(&raw.bytes);
    assert_eq!(
        hex(&hasher.finalize()),
        object.as_str(),
        "the bytes of {object} hash to its ID"
    );
    raw.bytes
}

/// Lists every file under a tree as (path, mode, contents), recursively.
async fn files(
    backend: &dyn Backend,
    id: &BackendRepositoryId,
    tree: &ObjectId,
) -> Vec<(Vec<u8>, u32, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![(Vec::new(), tree.clone())];
    while let Some((prefix, tree)) = pending.pop() {
        for (mode, name, object) in
            parse_tree(&verified(backend, id, &tree, ObjectKind::Tree).await)
        {
            let mut path = prefix.clone();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(&name);
            if mode == 0o40000 {
                pending.push((path, object));
            } else {
                let contents = verified(backend, id, &object, ObjectKind::Blob).await;
                found.push((path, mode, contents));
            }
        }
    }
    found.sort();
    found
}

/// ADR-0007 section 3: modes, nested directories, deletes that empty a
/// directory, compare-and-swap, and root commits.
pub async fn commits(backend: &dyn Backend, ids: &Namespace) {
    let id = fresh(backend, ids).await;
    let root = backend
        .create_commit(
            &id,
            &commit_request(
                MAIN,
                None,
                vec![
                    put(b"nested/deeper/file", b"content\0\xff", FileMode::Regular),
                    put(b"run", b"#!/bin/sh\n", FileMode::Executable),
                    put(b"link", b"nested/deeper/file", FileMode::Symlink),
                    put(b"keep", b"keep", FileMode::Regular),
                ],
            ),
        )
        .await
        .expect("a root commit on a new branch succeeds");
    assert_eq!(root.parent_commit_id, None, "a root commit has no parent");
    assert_eq!(tip(backend, &id, MAIN).await, Some(root.commit_id.clone()));
    let commit = verified(backend, &id, &root.commit_id, ObjectKind::Commit).await;
    assert_eq!(
        commit_tree(&commit),
        root.tree_id,
        "the result names the commit's tree"
    );
    assert_eq!(
        files(backend, &id, &root.tree_id).await,
        vec![
            (b"keep".to_vec(), 0o100644, b"keep".to_vec()),
            (b"link".to_vec(), 0o120000, b"nested/deeper/file".to_vec()),
            (
                b"nested/deeper/file".to_vec(),
                0o100644,
                b"content\0\xff".to_vec()
            ),
            (b"run".to_vec(), 0o100755, b"#!/bin/sh\n".to_vec()),
        ],
        "regular, executable, and symbolic-link entries round-trip"
    );

    // A stale expectation changes nothing.
    let stale = backend
        .create_commit(
            &id,
            &commit_request(MAIN, None, vec![put(b"x", b"x", FileMode::Regular)]),
        )
        .await;
    assert!(
        matches!(&stale, Err(BackendError::StaleExpectation { actual: Some(actual) }) if *actual == root.commit_id),
        "an absent expectation on an existing branch is stale: {stale:?}"
    );
    let wrong = backend
        .create_commit(
            &id,
            &commit_request(
                MAIN,
                Some(unknown_object()),
                vec![put(b"x", b"x", FileMode::Regular)],
            ),
        )
        .await;
    assert!(
        matches!(&wrong, Err(BackendError::StaleExpectation { actual: Some(actual) }) if *actual == root.commit_id),
        "a wrong parent is stale: {wrong:?}"
    );
    assert_eq!(
        tip(backend, &id, MAIN).await,
        Some(root.commit_id.clone()),
        "stale writes change nothing"
    );

    // Deleting the only file in a directory removes the directory.
    let child = backend
        .create_commit(
            &id,
            &commit_request(
                MAIN,
                Some(root.commit_id.clone()),
                vec![FileChange::Delete {
                    path: b"nested/deeper/file".to_vec(),
                }],
            ),
        )
        .await
        .expect("a delete succeeds");
    assert_eq!(child.parent_commit_id, Some(root.commit_id.clone()));
    let names: Vec<_> = parse_tree(&verified(backend, &id, &child.tree_id, ObjectKind::Tree).await)
        .into_iter()
        .map(|(_, name, _)| name)
        .collect();
    assert_eq!(
        names,
        [b"keep".to_vec(), b"link".to_vec(), b"run".to_vec()],
        "a directory left empty by deletes disappears"
    );

    // A root commit on a second branch leaves the first branch alone.
    let other = backend
        .create_commit(
            &id,
            &commit_request(
                "refs/heads/other",
                None,
                vec![put(b"o", b"o", FileMode::Regular)],
            ),
        )
        .await
        .expect("a root commit on another new branch succeeds");
    assert_eq!(other.parent_commit_id, None);
    assert_eq!(tip(backend, &id, MAIN).await, Some(child.commit_id.clone()));
    backend.delete_repository(&id).await.expect("cleanup");
}

/// ADR-0007 section 4: every object read back hashes to its ID; unknown
/// objects and deleted repositories are errors.
pub async fn raw_reads(backend: &dyn Backend, ids: &Namespace) {
    let id = fresh(backend, ids).await;
    let result = backend
        .create_commit(
            &id,
            &commit_request(MAIN, None, vec![put(b"a/b", b"bytes", FileMode::Regular)]),
        )
        .await
        .expect("a root commit succeeds");
    verified(backend, &id, &result.commit_id, ObjectKind::Commit).await;
    // `files` verifies every tree and blob hash on the way down.
    assert_eq!(files(backend, &id, &result.tree_id).await.len(), 1);
    assert!(
        matches!(
            backend.read_object(&id, &unknown_object()).await,
            Err(BackendError::ObjectNotFound)
        ),
        "an unknown object is object_not_found"
    );
    backend.delete_repository(&id).await.expect("cleanup");
    assert!(
        matches!(
            backend.read_object(&id, &result.commit_id).await,
            Err(BackendError::RepositoryNotFound)
        ),
        "a deleted repository is repository_not_found"
    );
}

fn mode(git_mode: u32) -> FileMode {
    match git_mode {
        0o100755 => FileMode::Executable,
        0o120000 => FileMode::Symlink,
        _ => FileMode::Regular,
    }
}

/// ADR-0007 section 5: the path corpus written with `create_commit` reads back
/// byte for byte with modes. Without `non_utf8_paths`, a non-UTF-8 path is `unsupported_path` and
/// changes nothing. The stock-git half of section 5 needs a git endpoint and
/// stays in each adapter's own tests.
pub async fn exact_paths(backend: &dyn Backend, ids: &Namespace) {
    let id = fresh(backend, ids).await;
    let supported = backend.capabilities().non_utf8_paths;
    let corpus: Vec<_> = PATH_CORPUS
        .iter()
        .filter(|fixture| supported || std::str::from_utf8(fixture.path).is_ok())
        .collect();
    let result = backend
        .create_commit(
            &id,
            &commit_request(
                MAIN,
                None,
                corpus
                    .iter()
                    .map(|f| put(f.path, f.contents, mode(f.mode)))
                    .collect(),
            ),
        )
        .await
        .expect("the supported path corpus can be committed");
    let mut expected: Vec<_> = corpus
        .iter()
        .map(|f| (f.path.to_vec(), f.mode, f.contents.to_vec()))
        .collect();
    expected.sort();
    assert_eq!(
        files(backend, &id, &result.tree_id).await,
        expected,
        "corpus paths, modes, and contents read back exactly"
    );
    if !supported {
        for fixture in PATH_CORPUS
            .iter()
            .filter(|f| std::str::from_utf8(f.path).is_err())
        {
            let refused = backend
                .create_commit(
                    &id,
                    &commit_request(
                        MAIN,
                        Some(result.commit_id.clone()),
                        vec![put(fixture.path, fixture.contents, FileMode::Regular)],
                    ),
                )
                .await;
            assert!(
                matches!(&refused, Err(BackendError::UnsupportedPath { path }) if path == fixture.path),
                "a non-UTF-8 path is unsupported_path without non_utf8_paths: {refused:?}"
            );
        }
        assert_eq!(
            tip(backend, &id, MAIN).await,
            Some(result.commit_id.clone()),
            "a refused path changes nothing"
        );
    }
    backend.delete_repository(&id).await.expect("cleanup");
}

/// ADR-0007 section 6: `copy_refs` keeps object IDs, copies exactly the named
/// refs under their destination names, honors the non-fast-forward flag, accepts an empty list,
/// and leaves behind objects that no named ref reaches.
pub async fn copy(backend: &dyn Backend, ids: &Namespace) {
    let source = fresh(backend, ids).await;
    let destination = fresh(backend, ids).await;
    let base = backend
        .create_commit(
            &source,
            &commit_request(MAIN, None, vec![put(b"a", b"base", FileMode::Regular)]),
        )
        .await
        .expect("a root commit succeeds");
    let ahead = backend
        .create_commit(
            &source,
            &commit_request(
                MAIN,
                Some(base.commit_id.clone()),
                vec![put(b"a", b"ahead", FileMode::Regular)],
            ),
        )
        .await
        .expect("a child commit succeeds");
    let side = backend
        .create_commit(
            &source,
            &commit_request(
                "refs/heads/side",
                None,
                vec![put(b"s", b"side only", FileMode::Regular)],
            ),
        )
        .await
        .expect("an unrelated root commit succeeds");
    let copy = |from: &str, to: &str, force: bool| RefCopy {
        source: from.into(),
        destination: to.into(),
        allow_non_fast_forward: force,
    };
    backend
        .copy_refs(&source, &destination, &[], true)
        .await
        .expect("an empty copy succeeds");
    assert!(
        backend
            .list_refs(&destination, &[])
            .await
            .expect("list_refs")
            .refs
            .is_empty(),
        "an empty copy copies nothing"
    );
    backend
        .copy_refs(
            &source,
            &destination,
            &[copy(MAIN, "refs/heads/copied", false)],
            true,
        )
        .await
        .expect("a new ref is copied");
    let snapshot = backend
        .list_refs(&destination, &[])
        .await
        .expect("list_refs");
    assert_eq!(
        snapshot
            .refs
            .iter()
            .map(|r| (r.name.as_str(), &r.object_id))
            .collect::<Vec<_>>(),
        [("refs/heads/copied", &ahead.commit_id)],
        "exactly the named ref is copied under its destination name"
    );
    for object in [
        &base.commit_id,
        &base.tree_id,
        &ahead.commit_id,
        &ahead.tree_id,
    ] {
        assert_eq!(
            backend
                .read_object(&destination, object)
                .await
                .expect("copied objects are readable"),
            backend
                .read_object(&source, object)
                .await
                .expect("source objects are readable"),
            "copies keep object IDs and bytes"
        );
    }
    let left_behind = backend.read_object(&destination, &side.commit_id).await;
    assert!(
        matches!(left_behind, Err(BackendError::ObjectNotFound)),
        "objects reachable from no named ref are not copied: {left_behind:?}"
    );

    // Moving the destination to an unrelated history needs the non-fast-forward flag.
    let refused = backend
        .copy_refs(
            &source,
            &destination,
            &[copy("refs/heads/side", "refs/heads/copied", false)],
            true,
        )
        .await;
    assert!(
        refused.is_err(),
        "a non-fast-forward copy without the flag fails"
    );
    assert_eq!(
        tip(backend, &destination, "refs/heads/copied").await,
        Some(ahead.commit_id.clone()),
        "a refused copy changes nothing"
    );
    backend
        .copy_refs(
            &source,
            &destination,
            &[copy("refs/heads/side", "refs/heads/copied", true)],
            true,
        )
        .await
        .expect("a non-fast-forward copy with the flag succeeds");
    assert_eq!(
        tip(backend, &destination, "refs/heads/copied").await,
        Some(side.commit_id.clone())
    );
    backend.delete_repository(&source).await.expect("cleanup");
    backend
        .delete_repository(&destination)
        .await
        .expect("cleanup");
}

/// ADR-0013, for backends with `durable_seal`: sealing is idempotent and refuses
/// every later mutation, while reads, copies out, and deletion remain available,
/// and a recreated ID is unsealed.
pub async fn sealing(backend: &dyn Backend, ids: &Namespace) {
    let sealed = fresh(backend, ids).await;
    let other = fresh(backend, ids).await;
    let first = backend
        .create_commit(
            &sealed,
            &commit_request(MAIN, None, vec![put(b"a", b"a", FileMode::Regular)]),
        )
        .await
        .expect("a root commit succeeds");
    // The copy source must hold the ref, so a refused copy can only mean the seal.
    backend
        .create_commit(
            &other,
            &commit_request(MAIN, None, vec![put(b"o", b"o", FileMode::Regular)]),
        )
        .await
        .expect("a root commit in the copy source succeeds");
    backend
        .seal_repository(&sealed)
        .await
        .expect("seal succeeds");
    backend
        .seal_repository(&sealed)
        .await
        .expect("a repeated seal succeeds");
    let write = backend
        .create_commit(
            &sealed,
            &commit_request(
                MAIN,
                Some(first.commit_id.clone()),
                vec![put(b"b", b"b", FileMode::Regular)],
            ),
        )
        .await;
    assert!(
        matches!(write, Err(BackendError::RepositorySealed)),
        "create_commit is refused: {write:?}"
    );
    let update = backend
        .update_refs(
            &sealed,
            &[RefUpdate {
                name: "refs/heads/new".into(),
                expected: RefExpectation::Absent,
                new: Some(first.commit_id.clone()),
            }],
        )
        .await;
    assert!(
        matches!(update, Err(BackendError::RepositorySealed)),
        "update_refs is refused: {update:?}"
    );
    let copy_in = backend
        .copy_refs(
            &other,
            &sealed,
            &[RefCopy {
                source: MAIN.into(),
                destination: "refs/heads/in".into(),
                allow_non_fast_forward: false,
            }],
            true,
        )
        .await;
    assert!(
        matches!(copy_in, Err(BackendError::RepositorySealed)),
        "a copy into a sealed repository is refused by the seal: {copy_in:?}"
    );
    assert_eq!(tip(backend, &sealed, "refs/heads/in").await, None);
    assert_eq!(
        tip(backend, &sealed, MAIN).await,
        Some(first.commit_id.clone()),
        "reads still work"
    );
    verified(backend, &sealed, &first.commit_id, ObjectKind::Commit).await;
    backend
        .copy_refs(
            &sealed,
            &other,
            &[RefCopy {
                source: MAIN.into(),
                destination: "refs/heads/out".into(),
                allow_non_fast_forward: false,
            }],
            true,
        )
        .await
        .expect("copying from a sealed repository succeeds");
    backend
        .delete_repository(&sealed)
        .await
        .expect("a sealed repository can be deleted");
    let missing = backend.seal_repository(&sealed).await;
    assert!(
        matches!(missing, Err(BackendError::RepositoryNotFound)),
        "sealing a deleted ID: {missing:?}"
    );
    backend
        .create_repository(&sealed, MAIN)
        .await
        .expect("the ID can be created again");
    backend
        .create_commit(
            &sealed,
            &commit_request(MAIN, None, vec![put(b"a", b"a", FileMode::Regular)]),
        )
        .await
        .expect("a recreated ID is not sealed");
    backend.delete_repository(&sealed).await.expect("cleanup");
    backend.delete_repository(&other).await.expect("cleanup");
}

/// ADR-0015, for backends with `durable_ref_creation`: an operation ID records
/// its applied or refused outcome, which a retry returns without a new mutation.
pub async fn ref_receipts(backend: &dyn Backend, ids: &Namespace) {
    let id = fresh(backend, ids).await;
    let first = backend
        .create_commit(
            &id,
            &commit_request(MAIN, None, vec![put(b"a", b"1", FileMode::Regular)]),
        )
        .await
        .expect("a root commit succeeds");
    let second = backend
        .create_commit(
            &id,
            &commit_request(
                MAIN,
                Some(first.commit_id.clone()),
                vec![put(b"a", b"2", FileMode::Regular)],
            ),
        )
        .await
        .expect("a child commit succeeds");
    let name = "refs/heads/receipt";
    assert!(
        backend
            .create_ref_once(&id, "created", name, &first.commit_id)
            .await
            .expect("creation"),
        "an absent ref is created"
    );
    assert!(
        !backend
            .create_ref_once(&id, "refused", name, &first.commit_id)
            .await
            .expect("refusal"),
        "an existing ref refuses a new operation"
    );
    let removed = backend
        .update_refs(
            &id,
            &[RefUpdate {
                name: name.into(),
                expected: RefExpectation::At(first.commit_id.clone()),
                new: None,
            }],
        )
        .await
        .expect("update_refs succeeds");
    assert_eq!(removed[0].rejection, None);
    assert!(
        backend
            .create_ref_once(&id, "created", name, &first.commit_id)
            .await
            .expect("replay"),
        "a retry returns the applied outcome"
    );
    assert_eq!(
        tip(backend, &id, name).await,
        None,
        "a replayed creation does not recreate the ref"
    );
    assert!(
        !backend
            .create_ref_once(&id, "refused", name, &first.commit_id)
            .await
            .expect("replay"),
        "a refusal is remembered after the conflict disappears"
    );
    let different = backend
        .create_ref_once(&id, "created", name, &second.commit_id)
        .await;
    assert!(
        matches!(different, Err(BackendError::InvalidRequest { .. })),
        "the same operation ID with different parameters is invalid: {different:?}"
    );
    backend.delete_repository(&id).await.expect("cleanup");
    backend
        .create_repository(&id, MAIN)
        .await
        .expect("recreate");
    let root = backend
        .create_commit(
            &id,
            &commit_request(MAIN, None, vec![put(b"a", b"1", FileMode::Regular)]),
        )
        .await
        .expect("a root commit succeeds");
    assert!(
        backend
            .create_ref_once(&id, "refused", name, &root.commit_id)
            .await
            .expect("fresh receipts"),
        "a new incarnation starts with no receipts"
    );
    backend.delete_repository(&id).await.expect("cleanup");
}
