use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use cambium_backend::{BackendRepositoryId, git_policy::*};
use cambium_backend_enroute::{
    hooks::{HookAdapter, HookError},
    signature,
    wire::enroute::{common::v1alpha1 as common, hook::v1alpha1 as wire},
};
use http::HeaderMap;
use prost::Message as _;

struct Policy {
    calls: AtomicUsize,
}

#[async_trait]
impl GitPolicy for Policy {
    async fn authorize(&self, request: &GitRequest) -> Result<GitGrant, GitDenial> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if request.path == "denied" {
            return Err(GitDenial::Unauthorized);
        }
        assert_eq!(request.path, "tenant/repository.git");
        assert_eq!(
            request.headers,
            [("authorization".into(), b"Bearer credential".to_vec())]
        );
        assert_eq!(request.operation, GitOperation::Write);
        Ok(GitGrant {
            repository: "opaque/g1".to_owned().try_into().unwrap(),
            actor: "identity-id".into(),
            context: b"context".to_vec(),
        })
    }
    async fn visible_refs(
        &self,
        context: &[u8],
        repository: &BackendRepositoryId,
        refs: &[String],
    ) -> Result<Vec<String>, GitDenial> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(context, b"context");
        assert_eq!(repository.as_str(), "opaque/g1");
        Ok(refs
            .iter()
            .filter(|name| name.as_str() == "refs/heads/main")
            .cloned()
            .collect())
    }
    async fn pre_receive(
        &self,
        context: &[u8],
        repository: &BackendRepositoryId,
        commands: &[GitCommand],
    ) -> Result<Vec<Option<String>>, GitDenial> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(context, b"context");
        assert_eq!(repository.as_str(), "opaque/g1");
        assert!(commands[0].force);
        if commands.len() == 1 {
            return Ok(vec![]);
        }
        Ok(vec![None, Some("ref_forbidden".into())])
    }
    async fn post_receive(
        &self,
        context: &[u8],
        repository: &BackendRepositoryId,
        commands: &[GitCommand],
    ) -> Result<(), GitDenial> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(context, b"context");
        assert_eq!(repository.as_str(), "opaque/g1");
        assert_eq!(commands[0].name, "refs/heads/main");
        Ok(())
    }
}

fn signed(key: &signature::SigningKey, request: wire::hook_request::Call) -> (Vec<u8>, HeaderMap) {
    let body = wire::HookRequest {
        call: Some(request),
    }
    .encode_to_vec();
    let mut headers = HeaderMap::new();
    for (name, value) in signature::sign(
        key,
        &signature::Covered {
            method: "POST",
            authority: "cambium:8080",
            path: "/internal/enroute",
            body: &body,
        },
        1_700_000_000,
    ) {
        headers.insert(name, value.parse().unwrap());
    }
    (body, headers)
}
fn authorize(path: &str) -> wire::hook_request::Call {
    wire::hook_request::Call::Authorize(wire::AuthorizeRequest {
        repo_path: path.into(),
        access: wire::Access::Write as i32,
        headers: vec![wire::Header {
            name: "authorization".into(),
            value: "Bearer credential".into(),
        }],
    })
}
fn repo() -> Option<common::RepoKey> {
    Some(common::RepoKey {
        key: "opaque/g1".into(),
    })
}
fn commands() -> Vec<wire::RefCommand> {
    ["refs/heads/main", "refs/cambium/internal"]
        .into_iter()
        .map(|name| wire::RefCommand {
            refname: name.into(),
            old_object_id: Some(common::ObjectId {
                hex: "1".repeat(40),
            }),
            new_object_id: Some(common::ObjectId {
                hex: "2".repeat(40),
            }),
            force: true,
        })
        .collect()
}

#[tokio::test]
async fn hooks_reject_untrusted_bytes_before_policy_and_preserve_all_four_calls() {
    let key = signature::SigningKey::from_seed(&[7; 32]);
    let policy = Arc::new(Policy {
        calls: AtomicUsize::new(0),
    });
    let adapter = HookAdapter::new(
        vec![key.verifying_key()],
        "http://cambium:8080/internal/enroute",
        policy.clone(),
    )
    .unwrap();
    let (body, headers) = signed(&key, authorize("tenant/repository.git"));
    for (method, authority, path, body, headers, now) in [
        (
            "POST",
            "cambium:8080",
            "/internal/enroute",
            body.clone(),
            HeaderMap::new(),
            1_700_000_000,
        ),
        (
            "POST",
            "cambium:8080",
            "/internal/enroute",
            vec![0],
            headers.clone(),
            1_700_000_000,
        ),
        (
            "GET",
            "cambium:8080",
            "/internal/enroute",
            body.clone(),
            headers.clone(),
            1_700_000_000,
        ),
        (
            "POST",
            "other:8080",
            "/internal/enroute",
            body.clone(),
            headers.clone(),
            1_700_000_000,
        ),
        (
            "POST",
            "cambium:8080",
            "/wrong",
            body.clone(),
            headers.clone(),
            1_700_000_000,
        ),
        (
            "POST",
            "cambium:8080",
            "/internal/enroute",
            body.clone(),
            headers.clone(),
            1_700_000_301,
        ),
    ] {
        assert!(matches!(
            adapter
                .handle(method, authority, path, &headers, &body, now)
                .await,
            Err(HookError::Signature)
        ));
    }
    assert_eq!(policy.calls.load(Ordering::SeqCst), 0);
    let result = adapter
        .handle(
            "POST",
            "cambium:8080",
            "/internal/enroute",
            &headers,
            &body,
            1_700_000_000,
        )
        .await
        .unwrap();
    let response = wire::HookResponse::decode(result.as_slice()).unwrap();
    let Some(wire::hook_response::Answer::Authorize(response)) = response.answer else {
        panic!("authorize response")
    };
    let Some(wire::authorize_response::Outcome::Granted(grant)) = response.outcome else {
        panic!("grant")
    };
    assert_eq!(grant.context, b"context");
    assert_eq!(grant.actor, "identity-id");
    assert_eq!(grant.repo.unwrap().key, "opaque/g1");

    let calls = [
        authorize("denied"),
        wire::hook_request::Call::VisibleRefs(wire::VisibleRefsRequest {
            repo: repo(),
            actor: "identity-id".into(),
            access: wire::Access::Read as i32,
            refnames: vec!["refs/heads/main".into(), "refs/cambium/internal".into()],
            context: b"context".to_vec(),
        }),
        wire::hook_request::Call::PreReceive(wire::PreReceiveRequest {
            repo: repo(),
            actor: "identity-id".into(),
            commands: commands(),
            context: b"context".to_vec(),
        }),
        wire::hook_request::Call::PostReceive(wire::PostReceiveRequest {
            repo: repo(),
            actor: "identity-id".into(),
            commands: commands(),
            context: b"context".to_vec(),
        }),
    ];
    for call in calls {
        let (body, headers) = signed(&key, call);
        let result = adapter
            .handle(
                "POST",
                "cambium:8080",
                "/internal/enroute",
                &headers,
                &body,
                1_700_000_000,
            )
            .await
            .unwrap();
        match wire::HookResponse::decode(result.as_slice())
            .unwrap()
            .answer
            .unwrap()
        {
            wire::hook_response::Answer::Authorize(answer) => {
                let Some(wire::authorize_response::Outcome::Denied(denial)) = answer.outcome else {
                    panic!("denial")
                };
                assert_eq!(denial.denial, wire::Denial::Unauthorized as i32);
                assert!(
                    denial
                        .challenge
                        .unwrap()
                        .www_authenticate
                        .starts_with("Basic ")
                );
            }
            wire::hook_response::Answer::VisibleRefs(answer) => {
                assert_eq!(answer.refnames, ["refs/heads/main"])
            }
            wire::hook_response::Answer::PreReceive(answer) => {
                assert_eq!(
                    answer.judgements[0].judgement,
                    wire::Judgement::Allow as i32
                );
                assert_eq!(
                    answer.judgements[1].judgement,
                    wire::Judgement::Refuse as i32
                );
                assert_eq!(answer.judgements[1].reason, "ref_forbidden");
            }
            wire::hook_response::Answer::PostReceive(answer) => assert!(answer.messages.is_empty()),
        }
    }
    let (body, headers) = signed(
        &key,
        wire::hook_request::Call::PreReceive(wire::PreReceiveRequest {
            repo: repo(),
            actor: "identity-id".into(),
            commands: commands()[..1].to_vec(),
            context: b"context".to_vec(),
        }),
    );
    assert!(matches!(
        adapter
            .handle(
                "POST",
                "cambium:8080",
                "/internal/enroute",
                &headers,
                &body,
                1_700_000_000
            )
            .await,
        Err(HookError::Unavailable)
    ));
}
