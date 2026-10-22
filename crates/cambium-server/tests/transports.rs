mod common;

use std::{path::Path, process::Command, sync::Arc};

use cambium_backend::git_policy::{GitCommand, GitDenial, GitOperation, GitPolicy, GitRequest};
use cambium_backend_enroute::{hooks::HookAdapter, signature::SigningKey};
use cambium_protocol::RepositoryId;
use cambium_server::{
    git_policy::Policy,
    secrets::{new_token, token_hash},
    service::{App, Clock, SystemClock},
    transport::{PublicState, hook_router, public_router},
};
use cambium_store::{Bootstrap, SqliteStore, Store, TokenSettings};
use reqwest::{Client, Response};
use serde_json::{Value, json};

struct Api {
    client: Client,
    base: String,
}

impl Api {
    async fn operation(&self, token: &str, name: &str, input: Value) -> Response {
        self.client
            .post(format!("{}/v1/operations/{name}", self.base))
            .bearer_auth(token)
            .json(&input)
            .send()
            .await
            .unwrap()
    }

    async fn success(&self, token: &str, name: &str, input: Value) -> Value {
        let response = self.operation(token, name, input).await;
        let status = response.status();
        let value: Value = response.json().await.unwrap();
        assert_eq!(status, 200, "{value}");
        value
    }

    async fn rpc(&self, token: &str, method: &str, params: Value) -> Value {
        let response = self
            .client
            .post(format!("{}/mcp", self.base))
            .bearer_auth(token)
            .header("mcp-protocol-version", "2025-11-25")
            .json(&json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.json().await.unwrap()
    }
}

fn git(directory: &Path, token: Option<&str>, arguments: &[&str], succeeds: bool) -> String {
    let mut command = Command::new("git");
    command
        .current_dir(directory)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(token) = token {
        command
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.extraHeader")
            .env(
                "GIT_CONFIG_VALUE_0",
                format!("Authorization: Bearer {token}"),
            );
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.success(),
        succeeds,
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn http_mcp_and_stock_git_share_live_authorization() {
    let hook_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/api/enroute/hooks",
        hook_listener.local_addr().unwrap()
    );
    let signing = SigningKey::from_seed(&[39; 32]);
    let server =
        common::Server::start_with_hooks(Some((&endpoint, &signing.to_pem().unwrap()))).await;
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteStore::open(
            directory.path().join("metadata.sqlite"),
            TokenSettings::default(),
        )
        .await
        .unwrap(),
    );
    let now = SystemClock.now_ms();
    let mut users = Vec::new();
    for slug in ["tenant-a", "tenant-b"] {
        let token = new_token().unwrap();
        let principal = store
            .deployment_bootstrap(
                &Bootstrap {
                    tenant_slug: slug.into(),
                    human_name: "Operator".into(),
                    token_hash: token_hash(&token).unwrap(),
                    expires_at_ms: now + 86_400_000,
                },
                now,
            )
            .await
            .unwrap();
        users.push((token, principal));
    }
    let (token, principal) = &users[0];
    let other = &users[1].0;
    let app = Arc::new(App::new(
        store.clone(),
        Arc::new(server.backend.clone()),
        Arc::new(SystemClock),
        &[51; 32],
        server.git.clone(),
    ));
    let policy = Arc::new(Policy { app: app.clone() });
    let adapter = Arc::new(
        HookAdapter::new(vec![signing.verifying_key()], &endpoint, policy.clone()).unwrap(),
    );
    let hook_task = tokio::spawn(async move {
        axum::serve(hook_listener, hook_router(adapter))
            .await
            .unwrap();
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = Api {
        client: Client::builder().no_proxy().build().unwrap(),
        base: format!("http://{}", listener.local_addr().unwrap()),
    };
    let public_task = tokio::spawn(async move {
        axum::serve(
            listener,
            public_router(PublicState {
                app,
                allowed_origins: vec!["https://allowed.example".into()],
            }),
        )
        .await
        .unwrap();
    });

    for path in ["/mcp", "/v1/operations/create_repository", "/unknown"] {
        let response = api
            .client
            .get(format!("{}{path}", api.base))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        assert_eq!(response.headers()["www-authenticate"], "Bearer");
        assert!(response.headers().contains_key("request-id"));
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "unauthenticated"
        );
    }
    assert_eq!(
        api.client
            .post(&endpoint)
            .body("not protobuf")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let origin = api
        .client
        .post(format!("{}/mcp", api.base))
        .bearer_auth(token)
        .header("origin", "https://untrusted.example")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(origin.status(), 403);
    let wrong_method = api
        .client
        .get(format!("{}/v1/operations/create_repository", api.base))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_method.status(), 400);
    let invalid = api
        .operation(
            token,
            "create_repository",
            json!({"name": "project", "typo": true}),
        )
        .await;
    assert_eq!(invalid.status(), 400);
    let created = api
        .success(
            token,
            "create_repository",
            json!({"name": "project", "idempotency_key": "create-over-http"}),
        )
        .await;
    let repo = &created["repository_id"];
    let initialized = api.rpc(token, "initialize", json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}})).await;
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    let tools = api.rpc(token, "tools/list", json!({})).await;
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 13);
    for tool in tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert_eq!(
            tool["inputSchema"]["properties"]["idempotency_key"]["type"],
            "string"
        );
    }
    let replay = api.rpc(token, "tools/call", json!({"name": "create_repository", "arguments": {"name": "project", "idempotency_key": "create-over-http"}})).await;
    assert_eq!(replay["result"]["structuredContent"], created);
    assert_eq!(replay["result"]["isError"], false);
    let notification = api
        .client
        .post(format!("{}/mcp", api.base))
        .bearer_auth(token)
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await
        .unwrap();
    assert_eq!(notification.status(), 202);
    assert!(notification.bytes().await.unwrap().is_empty());

    if let Ok(python) = std::env::var("MCP_CLIENT_PYTHON") {
        let output = Command::new(python)
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/mcp_client_smoke.py"
            ))
            .env("MCP_TEST_URL", format!("{}/mcp", api.base))
            .env("MCP_TEST_TOKEN", token)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        println!("{}", String::from_utf8_lossy(&output.stdout));
    }

    for (name, arguments) in [
        ("get_repository", json!({"repository_id": repo})),
        ("delete_repository", json!({"repository_id": repo})),
        (
            "write_changeset",
            json!({"repository_id": repo, "message": "denied", "changes": []}),
        ),
        (
            "read_file",
            json!({"repository_id": repo, "path": "README"}),
        ),
        ("list_tree", json!({"repository_id": repo})),
        (
            "get_identity",
            json!({"identity_id": principal.actor.identity_id}),
        ),
        (
            "disable_identity",
            json!({"identity_id": principal.actor.identity_id}),
        ),
        ("revoke_token", json!({"token_id": principal.token_id})),
        (
            "mint_token",
            json!({"identity_id": principal.actor.identity_id, "scope": "repo:read"}),
        ),
        (
            "create_identity",
            json!({"kind": "agent", "name": "Cross-tenant owner", "owner_id": principal.actor.identity_id}),
        ),
        (
            "diff",
            json!({"repository_id": repo, "from": "HEAD", "to": "HEAD"}),
        ),
    ] {
        let response = api.operation(other, name, arguments.clone()).await;
        assert_eq!(response.status(), 404);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "not_found"
        );
        let response = api
            .rpc(
                other,
                "tools/call",
                json!({"name": name, "arguments": arguments}),
            )
            .await;
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["structuredContent"]["error"]["code"],
            "not_found"
        );
    }

    let change = json!({"repository_id": repo, "message": "First commit", "changes": [{"action": "put", "path": "README", "contents": "hello\n"}], "idempotency_key": "write-over-mcp"});
    let write = api
        .rpc(
            token,
            "tools/call",
            json!({"name": "write_changeset", "arguments": change}),
        )
        .await;
    assert_eq!(write["result"]["isError"], false, "{write}");
    let write = &write["result"]["structuredContent"];
    assert_eq!(api.success(token, "write_changeset", change).await, *write);
    let first = write["commit_id"].as_str().unwrap();
    let read = api
        .success(
            token,
            "read_file",
            json!({"repository_id": repo, "path": "README"}),
        )
        .await;
    assert_eq!(read["contents"], "hello\n");
    api.success(token, "list_tree", json!({"repository_id": repo}))
        .await;
    assert_eq!(
        api.success(token, "list_repositories", json!({})).await["repositories"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let reader = api.success(token, "mint_token", json!({"identity_id": principal.actor.identity_id, "scope": "repo:read", "repositories": [repo]})).await;
    let read_token = reader["token"].as_str().unwrap();
    let project_url = format!("{}tenant-a/project.git", server.git);
    for (credential, service, expected) in [
        (None, "git-upload-pack", 401),
        (Some("invalid"), "git-upload-pack", 401),
        (Some(other.as_str()), "git-upload-pack", 404),
        (Some(other.as_str()), "git-receive-pack", 404),
        (Some(read_token), "git-receive-pack", 403),
        (Some(read_token), "git-upload-pack", 200),
    ] {
        let mut request = api
            .client
            .get(format!("{project_url}/info/refs?service={service}"));
        if let Some(credential) = credential {
            request = request.bearer_auth(credential);
        }
        assert_eq!(request.send().await.unwrap().status(), expected);
    }
    let basic = api
        .client
        .get(format!("{project_url}/info/refs?service=git-upload-pack"))
        .basic_auth("ignored", Some(read_token))
        .send()
        .await
        .unwrap();
    assert_eq!(basic.status(), 200);
    git(
        directory.path(),
        Some(token),
        &["clone", &project_url, "checkout"],
        true,
    );
    let checkout = directory.path().join("checkout");
    assert_eq!(
        git(&checkout, None, &["rev-parse", "HEAD"], true).trim(),
        first
    );
    std::fs::write(checkout.join("README"), "from stock git\n").unwrap();
    git(&checkout, None, &["add", "README"], true);
    git(
        &checkout,
        None,
        &[
            "-c",
            "user.name=Git Tester",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "Stock Git",
        ],
        true,
    );
    git(
        &checkout,
        Some(token),
        &["push", "origin", "HEAD:main"],
        true,
    );
    git(&checkout, Some(read_token), &["fetch", "origin"], true);
    let second = git(&checkout, None, &["rev-parse", "HEAD"], true);
    let read = api
        .success(
            token,
            "read_file",
            json!({"repository_id": repo, "path": "README"}),
        )
        .await;
    assert_eq!(read["contents"], "from stock git\n");
    let diff = api
        .success(
            token,
            "diff",
            json!({"repository_id": repo, "from": first, "to": second.trim()}),
        )
        .await;
    assert_eq!(diff["entries"].as_array().unwrap().len(), 1);
    for destination in [
        "refs/cambium/hidden",
        "refs/cambium-rescue/forged",
        "refs/heads/bad\u{fffd}",
    ] {
        git(
            &checkout,
            Some(token),
            &["push", "origin", &format!("HEAD:{destination}")],
            false,
        );
    }
    git(
        &checkout,
        Some(other),
        &["push", "origin", "HEAD:cross-tenant"],
        false,
    );
    git(
        &checkout,
        Some(read_token),
        &["push", "origin", "HEAD:read-only"],
        false,
    );
    let absent = format!("{}tenant-a/absent.git", server.git);
    git(
        &checkout,
        Some(token),
        &["push", &absent, "HEAD:main"],
        false,
    );

    let writer = api.success(token, "mint_token", json!({"identity_id": principal.actor.identity_id, "scope": "repo:write", "repositories": [repo]})).await;
    let writer_token = writer["token"].as_str().unwrap();
    let grant = policy
        .authorize(&GitRequest {
            path: "tenant-a/project.git".into(),
            operation: GitOperation::Write,
            headers: vec![(
                "authorization".into(),
                format!("Bearer {writer_token}").into_bytes(),
            )],
        })
        .await
        .unwrap();
    let commands = [GitCommand {
        name: "refs/heads/main".into(),
        old: Some(first.parse().unwrap()),
        new: Some(second.trim().parse().unwrap()),
        force: false,
    }];
    assert_eq!(
        policy
            .pre_receive(&grant.context, &grant.repository, &commands)
            .await
            .unwrap(),
        vec![None]
    );
    api.success(
        token,
        "revoke_token",
        json!({"token_id": writer["token_id"]}),
    )
    .await;
    assert!(matches!(
        policy
            .pre_receive(&grant.context, &grant.repository, &commands)
            .await,
        Err(GitDenial::Unauthorized)
    ));
    let revoked = api
        .client
        .get(format!("{project_url}/info/refs?service=git-upload-pack"))
        .bearer_auth(writer_token)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), 401);
    let agent = api.success(token, "create_identity", json!({"kind": "agent", "name": "Network agent", "owner_id": principal.actor.identity_id})).await;
    api.success(
        token,
        "get_identity",
        json!({"identity_id": agent["identity_id"]}),
    )
    .await;
    api.success(
        token,
        "disable_identity",
        json!({"identity_id": agent["identity_id"]}),
    )
    .await;
    let id: RepositoryId = repo.as_str().unwrap().parse().unwrap();
    assert_eq!(
        store
            .get_repository(&principal.tenant_id, &id)
            .await
            .unwrap()
            .state,
        "ready"
    );
    api.success(token, "delete_repository", json!({"repository_id": repo}))
        .await;
    assert_eq!(
        api.operation(token, "get_repository", json!({"repository_id": repo}))
            .await
            .status(),
        404
    );
    public_task.abort();
    hook_task.abort();
}

struct Process(std::process::Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn start_binary(path: &Path, api: &Api) -> Process {
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_cambium"))
            .arg("--data-dir")
            .arg(path)
            .arg("serve")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    for _ in 0..100 {
        if let Ok(response) = api.client.get(format!("{}/mcp", api.base)).send().await
            && response.status() == 401
        {
            return process;
        }
        assert!(
            process.0.try_wait().unwrap().is_none(),
            "Cambium exited before readiness"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("Cambium did not become ready");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires ENROUTE_BIN pointing to the pinned patched backend"]
async fn binary_uses_private_files_and_preserves_encrypted_replay_across_restart() {
    use cambium_server::deployment::{self, Config};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("private");
    deployment::initialize(&path, &Config::default())
        .await
        .unwrap();
    let public_socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let hook_socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!(
        "http://{}/api/enroute/hooks",
        hook_socket.local_addr().unwrap()
    );
    let signing = std::fs::read_to_string(path.join("hook-signing.pem")).unwrap();
    let backend = common::Server::start_with_hooks(Some((&endpoint, &signing))).await;
    let config = Config {
        api_listen: public_socket.local_addr().unwrap(),
        hooks_listen: hook_socket.local_addr().unwrap(),
        hook_endpoint: endpoint,
        backend_endpoint: backend.api.clone(),
        backend_git_url: backend.git.clone(),
        public_git_url: backend.git.clone(),
        ..Config::default()
    };
    std::fs::write(path.join("cambium.toml"), toml::to_string(&config).unwrap()).unwrap();
    let api = Api {
        client: Client::builder().no_proxy().build().unwrap(),
        base: format!("http://{}", config.api_listen),
    };
    let output = Command::new(env!("CARGO_BIN_EXE_cambium"))
        .arg("--data-dir")
        .arg(&path)
        .args([
            "admin",
            "bootstrap",
            "--tenant-slug",
            "binary",
            "--human-name",
            "Operator",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let user: Value = serde_json::from_slice(&output.stdout).unwrap();
    let token = user["token"].as_str().unwrap();
    drop(public_socket);
    drop(hook_socket);
    let process = start_binary(&path, &api).await;
    let second = Command::new(env!("CARGO_BIN_EXE_cambium"))
        .arg("--data-dir")
        .arg(&path)
        .args(["--json", "serve"])
        .output()
        .unwrap();
    assert!(!second.status.success());
    let denial: Value = serde_json::from_slice(&second.stderr).unwrap();
    assert_eq!(denial["error"]["code"], "invalid_request");
    let created = api
        .success(
            token,
            "create_repository",
            json!({"name": "restart", "idempotency_key": "create"}),
        )
        .await;
    let repository = &created["repository_id"];
    let minted = api.success(token, "mint_token", json!({"identity_id": user["identity_id"], "scope": "repo:read", "ttl_seconds": 1800, "idempotency_key": "credential"})).await;
    let write = api.success(token, "write_changeset", json!({"repository_id": repository, "message": "persistent", "changes": [{"action": "put", "path": "data", "contents": "persisted"}], "idempotency_key": "write"})).await;
    let url = format!("{}binary/restart.git", backend.git);
    git(directory.path(), Some(token), &["ls-remote", &url], true);
    drop(process);
    let _process = start_binary(&path, &api).await;
    let replay = api.operation(token, "mint_token", json!({"identity_id": user["identity_id"], "scope": "repo:read", "ttl_seconds": 1800, "idempotency_key": "credential"})).await;
    assert_eq!(replay.headers()["idempotent-replayed"], "true");
    assert_eq!(replay.json::<Value>().await.unwrap(), minted);
    let read = api
        .success(
            token,
            "read_file",
            json!({"repository_id": repository, "revision": write["commit_id"], "path": "data"}),
        )
        .await;
    assert_eq!(read["contents"], "persisted");
    let revoked = Command::new(env!("CARGO_BIN_EXE_cambium"))
        .arg("--data-dir")
        .arg(&path)
        .args([
            "admin",
            "revoke-token",
            "--tenant-id",
            user["tenant_id"].as_str().unwrap(),
            "--token-id",
            user["token_id"].as_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(revoked.status.success());
    assert_eq!(
        api.operation(
            token,
            "get_repository",
            json!({"repository_id": repository})
        )
        .await
        .status(),
        401
    );
    git(directory.path(), Some(token), &["ls-remote", &url], false);
}
