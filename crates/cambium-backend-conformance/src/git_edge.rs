//! Stock-Git checks for ADR-0007 sections 5 and 7.
use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use crate::{paths::PATH_CORPUS, suite::files};
use cambium_backend::{Backend, BackendRepositoryId, RefExpectation, RefUpdate};
use cambium_protocol::ObjectId;

fn invoke(directory: &Path, args: &[&str], input: &[u8]) -> Output {
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
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn git(directory: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let result = invoke(directory, args, input);
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

/// Populate an empty directory with a bare repository containing the complete
/// byte-path corpus. Return the commit at refs/heads/main.
pub fn init_corpus(directory: &Path) -> String {
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
    commit
}

/// An adapter configures policy on a disposable endpoint and implements these
/// two transitions. Initially the policy grants access, hides refs/heads/hidden
/// and refuses updates to refs/heads/refused. The endpoint must keep serving Git
/// after its policy listener has stopped.
pub trait PolicyControl {
    fn deny_access(&self, deny: bool);
    fn disconnect(&self) -> impl Future<Output = ()> + Send;
}

/// Exercise a fresh, empty repository. Run with an independent listener thread
/// or a multi-thread runtime because the stock Git subprocess is synchronous.
pub async fn run(
    backend: &dyn Backend,
    repository: &BackendRepositoryId,
    endpoint: &str,
    policy: &impl PolicyControl,
    directory: &Path,
) {
    let commit = init_corpus(directory);
    git(directory, &["push", endpoint, "refs/heads/main"], b"");
    let tree = text(git(
        directory,
        &["rev-parse", "refs/heads/main^{tree}"],
        b"",
    ));
    let tip: ObjectId = commit.parse().unwrap();
    let refs = backend.list_refs(repository, &[]).await.unwrap().refs;
    assert!(
        refs.iter()
            .any(|r| r.name == "refs/heads/main" && r.object_id == tip),
        "allowed push must update the expected repository"
    );

    let mut expected: Vec<_> = PATH_CORPUS
        .iter()
        .map(|f| (f.path.to_vec(), f.mode, f.contents.to_vec()))
        .collect();
    expected.sort();
    assert_eq!(
        files(backend, repository, &tree.parse().unwrap()).await,
        expected,
        "stock Git corpus paths, modes and contents"
    );

    let hidden = "refs/heads/hidden";
    let update = RefUpdate {
        name: hidden.into(),
        expected: RefExpectation::Absent,
        new: Some(tip),
    };
    let setup = backend.update_refs(repository, &[update]).await.unwrap();
    assert!(
        matches!(&setup[..], [outcome] if outcome.rejection.is_none()),
        "seed the hidden ref"
    );
    let refs_before = backend.list_refs(repository, &[]).await.unwrap();
    let advertise = || {
        let out = invoke(directory, &["ls-remote", "--refs", endpoint], b"");
        out.status.success().then(|| text(out.stdout))
    };
    let push = |destination: &str| {
        invoke(
            directory,
            &["push", endpoint, &format!("refs/heads/main:{destination}")],
            b"",
        )
        .status
        .success()
    };
    check_policy(policy, &commit, advertise, push)
        .await
        .expect("git edge contract");
    assert_eq!(
        backend.list_refs(repository, &[]).await.unwrap(),
        refs_before,
        "rejected pushes changed refs"
    );
}

async fn check_policy(
    policy: &impl PolicyControl,
    commit: &str,
    advertise: impl Fn() -> Option<String>,
    push: impl Fn(&str) -> bool,
) -> Result<(), &'static str> {
    let expected = format!("{commit}\trefs/heads/main");
    if advertise().as_ref() != Some(&expected) {
        return Err("advertise exactly the visible ref");
    }
    if push("refs/heads/refused") {
        return Err("refused update succeeded");
    }
    policy.deny_access(true);
    if advertise().is_some() {
        return Err("denied read succeeded");
    }
    if push("refs/heads/denied") {
        return Err("denied write succeeded");
    }
    policy.deny_access(false);
    if advertise().as_ref() != Some(&expected) {
        return Err("restoring policy did not restore access");
    }
    policy.disconnect().await;
    if advertise().is_some() {
        return Err("offline read succeeded");
    }
    if push("refs/heads/offline") {
        return Err("offline write succeeded");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU8, Ordering};

    /// Model the adapter's reported Git outcomes in memory. Each mutation
    /// changes one observation, including failures that only affect writes.
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Fault {
        None,
        HideMain,
        ShowHidden,
        AcceptRefused,
        DeniedRead,
        DeniedWrite,
        StayDenied,
        OfflineRead,
        OfflineWrite,
    }

    struct Model {
        state: AtomicU8,
        fault: Fault,
    }

    impl PolicyControl for Model {
        fn deny_access(&self, deny: bool) {
            self.state.store(if deny { 1 } else { 2 }, Ordering::SeqCst);
        }
        async fn disconnect(&self) {
            self.state.store(3, Ordering::SeqCst);
        }
    }

    impl Model {
        fn advertise(&self) -> Option<String> {
            match (self.state.load(Ordering::SeqCst), self.fault) {
                (0, Fault::HideMain) => Some(String::new()),
                (0, Fault::ShowHidden) => {
                    Some("tip\trefs/heads/main\ntip\trefs/heads/hidden".into())
                }
                (2, Fault::StayDenied) => None,
                (0 | 2, _) | (1, Fault::DeniedRead) | (3, Fault::OfflineRead) => {
                    Some("tip\trefs/heads/main".into())
                }
                _ => None,
            }
        }
        fn push(&self, name: &str) -> bool {
            matches!(
                (self.state.load(Ordering::SeqCst), self.fault, name),
                (0, Fault::AcceptRefused, "refs/heads/refused")
                    | (1, Fault::DeniedWrite, "refs/heads/denied")
                    | (3, Fault::OfflineWrite, "refs/heads/offline")
            )
        }
    }

    #[tokio::test]
    async fn rejects_each_broken_git_adapter_observation() {
        for (fault, expected) in [
            (Fault::None, Ok(())),
            (Fault::HideMain, Err("advertise exactly the visible ref")),
            (Fault::ShowHidden, Err("advertise exactly the visible ref")),
            (Fault::AcceptRefused, Err("refused update succeeded")),
            (Fault::DeniedRead, Err("denied read succeeded")),
            (Fault::DeniedWrite, Err("denied write succeeded")),
            (
                Fault::StayDenied,
                Err("restoring policy did not restore access"),
            ),
            (Fault::OfflineRead, Err("offline read succeeded")),
            (Fault::OfflineWrite, Err("offline write succeeded")),
        ] {
            let model = Model {
                state: AtomicU8::new(0),
                fault,
            };
            assert_eq!(
                check_policy(&model, "tip", || model.advertise(), |r| model.push(r)).await,
                expected
            );
        }
    }
}
