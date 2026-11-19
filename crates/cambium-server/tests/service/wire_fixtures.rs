use super::{NOW, TestClock, call, common};
use cambium_server::{
    secrets::{new_token, token_hash},
    service::App,
    transport::tools,
};
use cambium_store::{Bootstrap, SqliteStore, Store, TokenSettings};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, atomic::AtomicI64},
};

struct Capture {
    app: Arc<App>,
    token: String,
    samples: BTreeMap<String, Vec<Value>>,
}

impl Capture {
    async fn record(&mut self, name: &str, body: Value) -> Value {
        let reply = call(&self.app, &self.token, name, body).await;
        assert_eq!(reply.status, 200, "{name}: {}", reply.body);
        self.samples
            .entry(name.into())
            .or_default()
            .push(reply.body.clone());
        reply.body
    }
}

/// The original responses were captured at b19627b. Replace only generated
/// identifiers and the disposable credential. All other bytes, including Git
/// object IDs, field omissions and timestamps, are part of the comparison.
#[tokio::test]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn success_bodies_match_the_pre_schema_wire_fixtures() {
    let server = common::Server::start().await;
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteStore::open(
            directory.path().join("data.sqlite"),
            TokenSettings::default(),
        )
        .await
        .unwrap(),
    );
    let token = new_token().unwrap();
    let principal = store
        .deployment_bootstrap(
            &Bootstrap {
                tenant_slug: "fixtures".into(),
                human_name: "Operator".into(),
                token_hash: token_hash(&token).unwrap(),
                expires_at_ms: NOW + 86_400_000,
            },
            NOW,
        )
        .await
        .unwrap();
    let app = Arc::new(App::new(
        store,
        Arc::new(server.backend.clone()),
        Arc::new(TestClock(AtomicI64::new(NOW))),
        &[42; 32],
        "https://git.example.invalid".into(),
    ));
    let mut capture = Capture {
        app,
        token,
        samples: BTreeMap::new(),
    };
    let repo = capture
        .record("create_repository", json!({"name":"wire"}))
        .await["repository_id"]
        .clone();
    let r = json!({"repository_id":repo});
    capture.record("get_repository", r.clone()).await;
    capture.record("list_repositories", json!({})).await;
    let identity =
        json!({"name":"Fixture","email":"fixture@example.invalid","time":"2025-01-01T00:00:00Z"});
    let write = |changes: Value| json!({"repository_id":repo,"author":identity,"committer":identity,"message":"fixture","changes":changes,"provenance":{"model":"fixture-model","session_id":"fixture-run"}});
    let first = capture.record("write_changeset",write(json!([{"action":"put","path":"text","contents":"old"},{"action":"put","path":"gone","contents":"removed"}]))).await["commit_id"].clone();
    let second = capture.record("write_changeset",write(json!([{"action":"put","path":"text","contents":"updated"},{"action":"delete","path":"gone"},{"action":"put","path":"TEXT","contents_b64":"AP8="}]))).await["commit_id"].clone();
    for path in ["text", "TEXT"] {
        capture
            .record("read_file", json!({"repository_id":repo,"path":path}))
            .await;
    }
    capture.record("list_tree", r.clone()).await;
    capture
        .record(
            "diff",
            json!({"repository_id":repo,"from":first,"to":second}),
        )
        .await;
    capture
        .record(
            "get_commit_provenance",
            json!({"repository_id":repo,"commit_id":second}),
        )
        .await;
    let branch = capture
        .record(
            "create_ephemeral_branch",
            json!({"repository_id":repo,"name":"temporary","start_commit":second,"ttl_seconds":10}),
        )
        .await["branch_id"]
        .clone();
    let b = json!({"repository_id":repo,"branch_id":branch});
    capture.record("get_branch", b.clone()).await;
    capture.record("list_branches", r.clone()).await;
    capture
        .record(
            "extend_branch",
            json!({"repository_id":repo,"branch_id":branch,"ttl_seconds":120}),
        )
        .await;
    capture.record("promote_branch", b).await;
    let agent = capture
        .record(
            "create_identity",
            json!({"kind":"agent","name":"Fixture","owner_id":principal.actor.identity_id}),
        )
        .await["identity_id"]
        .clone();
    let who = json!({"identity_id":agent});
    capture.record("get_identity", who.clone()).await;
    let mint = capture
        .record(
            "mint_token",
            json!({"identity_id":agent,"scope":"repo:read","ttl_seconds":60,"repositories":[repo]}),
        )
        .await;
    capture
        .record("revoke_token", json!({"token_id":mint["token_id"]}))
        .await;
    capture.record("disable_identity", who.clone()).await;
    capture.record("get_identity", who).await;
    capture.record("get_reclamation", r.clone()).await;
    let compaction = capture.record("request_compaction", r.clone()).await["compaction_id"].clone();
    capture.record("get_reclamation", r.clone()).await;
    capture.record("delete_repository", r).await;
    let names: BTreeSet<_> = tools()
        .into_iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, capture.samples.keys().cloned().collect());
    let mut actual = serde_json::to_string_pretty(&capture.samples).unwrap() + "\n";
    for (original, replacement) in [
        (
            json!(principal.actor.identity_id),
            "idn_00000000000000000000000001",
        ),
        (agent, "idn_00000000000000000000000002"),
        (repo, "rep_00000000000000000000000001"),
        (branch, "brn_00000000000000000000000001"),
        (compaction, "cmp_00000000000000000000000001"),
        (mint["token_id"].clone(), "tok_00000000000000000000000001"),
        (mint["token"].clone(), "redacted-disposable-test-token"),
    ] {
        assert!(original.is_string());
        actual = actual.replace(&original.to_string(), &json!(replacement).to_string());
    }
    assert!(!actual.contains(mint["token"].as_str().unwrap()));
    assert_eq!(actual, include_str!("success.json"));
}
