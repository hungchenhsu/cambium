//! CLI behavior against a scripted HTTP server: requests, output, and exit status.

use std::sync::{Arc, Mutex};

use cambium_cli::{Environment, Io, exit, run};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[derive(Clone, Debug, Default)]
struct Seen {
    head: String,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    }
    fn request_line(&self) -> &str {
        self.head.lines().next().unwrap_or_default()
    }
}

/// Answers every request with one scripted response and records the requests.
async fn server(
    status: u16,
    headers: &'static str,
    body: String,
) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0; 4096];
            let (head, mut rest) = loop {
                let read = socket.read(&mut chunk).await.unwrap();
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    break (
                        String::from_utf8(buffer[..end].to_vec()).unwrap(),
                        buffer[end + 4..].to_vec(),
                    );
                }
            };
            let length: usize = head
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap_or(0);
            while rest.len() < length {
                let read = socket.read(&mut chunk).await.unwrap();
                rest.extend_from_slice(&chunk[..read]);
            }
            record.lock().unwrap().push(Seen {
                head,
                body: String::from_utf8(rest).unwrap(),
            });
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{headers}\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            let _ = socket.shutdown().await;
        }
    });
    (url, seen)
}

struct Outcome {
    status: i32,
    stdout: String,
    stderr: String,
}

async fn cli(args: &[&str], token: Option<&str>, stdin: &str) -> Outcome {
    let mut input = stdin.as_bytes();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let status = run(
        std::iter::once("cambium-cli").chain(args.iter().copied()),
        Environment {
            token: token.map(str::to_owned),
            url: None,
        },
        Io {
            stdin: &mut input,
            stdout: &mut stdout,
            stderr: &mut stderr,
        },
    )
    .await;
    Outcome {
        status,
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
    }
}

const REQUEST_ID: &str = "req_01jabcdefghjkmnpqrstvwxyz0";

#[tokio::test]
async fn a_call_sends_one_authenticated_operation_and_json_prints_the_body_exactly() {
    let body = r#"{"repository_id":"rep_01","name":"project"}"#.to_owned();
    let (url, seen) = server(200, "", body.clone()).await;
    let out = cli(
        &[
            "--url",
            &url,
            "--json",
            "call",
            "create_repository",
            "--input",
            r#"{"name":"project"}"#,
            "--idempotency-key",
            "create-1",
        ],
        Some("secret-token"),
        "",
    )
    .await;
    assert_eq!(out.status, exit::SUCCESS, "{}", out.stderr);
    assert_eq!(out.stdout, format!("{body}\n"));
    assert_eq!(out.stderr, "");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].request_line(),
        "POST /v1/operations/create_repository HTTP/1.1"
    );
    assert_eq!(
        seen[0].header("authorization").as_deref(),
        Some("Bearer secret-token")
    );
    assert_eq!(
        seen[0].header("idempotency-key").as_deref(),
        Some("create-1")
    );
    assert_eq!(
        seen[0].header("content-type").as_deref(),
        Some("application/json")
    );
    assert_eq!(
        serde_json::from_str::<Value>(&seen[0].body).unwrap(),
        json!({"name": "project"})
    );
}

#[tokio::test]
async fn api_errors_exit_one_with_the_exact_body_or_catalog_messages() {
    let body = json!({"error": {"code": "ref_conflict", "message": "The ref does not match the expected value", "retryable": false, "details": {"ref": "main"}, "request_id": REQUEST_ID}}).to_string();
    let (url, _) = server(409, "", body.clone()).await;
    let machine = cli(
        &["--url", &url, "--json", "call", "write_changeset"],
        Some("t"),
        "",
    )
    .await;
    assert_eq!(machine.status, exit::API_ERROR);
    assert_eq!(machine.stdout, format!("{body}\n"));
    assert_eq!(machine.stderr, "");
    let human = cli(&["--url", &url, "call", "write_changeset"], Some("t"), "").await;
    assert_eq!(human.status, exit::API_ERROR);
    assert_eq!(human.stdout, "");
    assert!(
        human.stderr.starts_with(
            "Error ref_conflict (HTTP 409): The ref does not match the expected value\n"
        ),
        "{}",
        human.stderr
    );
    assert!(human.stderr.contains("\"ref\": \"main\""));
    assert!(human.stderr.contains(&format!("Request ID: {REQUEST_ID}")));
    assert!(!human.stderr.contains("retryable"));

    let retryable = json!({"error": {"code": "backend_unavailable", "message": "The storage backend did not answer", "retryable": true, "details": {}, "request_id": REQUEST_ID}}).to_string();
    let (url, _) = server(503, "retry-after: 1\r\n", retryable).await;
    let human = cli(&["--url", &url, "call", "read_file"], Some("t"), "").await;
    assert_eq!(human.status, exit::API_ERROR);
    assert!(
        human.stderr.contains("This error is retryable"),
        "{}",
        human.stderr
    );
}

#[tokio::test]
async fn human_output_indents_results_and_notes_replays_on_stderr() {
    let (url, _) = server(
        200,
        "idempotent-replayed: true\r\n",
        r#"{"commit_id":"abc"}"#.to_owned(),
    )
    .await;
    let human = cli(
        &["--url", &url, "call", "write_changeset", "--input", "-"],
        Some("t"),
        r#"{"repository_id":"rep_01"}"#,
    )
    .await;
    assert_eq!(human.status, exit::SUCCESS);
    assert_eq!(human.stdout, "{\n  \"commit_id\": \"abc\"\n}\n");
    assert!(human.stderr.contains("stored response"), "{}", human.stderr);
    let machine = cli(
        &["--url", &url, "--json", "call", "write_changeset"],
        Some("t"),
        "",
    )
    .await;
    assert_eq!(machine.stderr, "", "--json adds nothing outside the body");
}

#[tokio::test]
async fn usage_errors_exit_two_without_sending_anything() {
    let (url, seen) = server(200, "", "{}".to_owned()).await;
    for (args, token) in [
        (vec!["--url", url.as_str(), "call", "get_repository"], None),
        (
            vec!["--url", url.as_str(), "call", "get_repository"],
            Some("  \n"),
        ),
        (
            vec![
                "--url",
                url.as_str(),
                "call",
                "get_repository",
                "--input",
                "[1]",
            ],
            Some("t"),
        ),
        (
            vec![
                "--url",
                url.as_str(),
                "call",
                "get_repository",
                "--input",
                "{bad",
            ],
            Some("t"),
        ),
        (vec!["--url", url.as_str(), "call", "../mcp"], Some("t")),
        (
            vec![
                "--url",
                url.as_str(),
                "call",
                "get_repository",
                "--input",
                "@/nonexistent/input.json",
            ],
            Some("t"),
        ),
        (
            vec![
                "--url",
                url.as_str(),
                "--token-file",
                "/nonexistent/token",
                "call",
                "get_repository",
            ],
            Some("t"),
        ),
        (vec!["--url", url.as_str(), "frobnicate"], Some("t")),
    ] {
        let out = cli(&args, token, "").await;
        assert_eq!(out.status, exit::USAGE, "{args:?}: {}", out.stderr);
        assert_eq!(out.stdout, "", "{args:?}");
        assert!(!out.stderr.is_empty(), "{args:?}");
    }
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn token_and_input_files_are_read_and_trimmed() {
    let (url, seen) = server(200, "", "{}".to_owned()).await;
    let directory = tempfile::tempdir().unwrap();
    let token = directory.path().join("token");
    std::fs::write(&token, "from-file\n").unwrap();
    let input = directory.path().join("input.json");
    std::fs::write(&input, r#"{"repository_id":"rep_01"}"#).unwrap();
    let out = cli(
        &[
            "--url",
            &url,
            "--token-file",
            token.to_str().unwrap(),
            "call",
            "get_repository",
            "--input",
            &format!("@{}", input.display()),
        ],
        Some("from-environment"),
        "",
    )
    .await;
    assert_eq!(out.status, exit::SUCCESS, "{}", out.stderr);
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen[0].header("authorization").as_deref(),
        Some("Bearer from-file")
    );
    assert_eq!(seen[0].body, r#"{"repository_id":"rep_01"}"#);
}

#[tokio::test]
async fn unreachable_or_non_api_answers_exit_three_with_nothing_on_stdout() {
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let out = cli(
        &["--url", &url, "--json", "call", "get_repository"],
        Some("t"),
        "",
    )
    .await;
    assert_eq!(out.status, exit::TRANSPORT);
    assert_eq!(out.stdout, "");
    assert!(out.stderr.starts_with("Could not reach"), "{}", out.stderr);

    let (url, _) = server(502, "", "<html>bad gateway</html>".to_owned()).await;
    let out = cli(
        &["--url", &url, "--json", "call", "get_repository"],
        Some("t"),
        "",
    )
    .await;
    assert_eq!(out.status, exit::TRANSPORT);
    assert_eq!(out.stdout, "");
    assert!(out.stderr.contains("HTTP 502"), "{}", out.stderr);
}

#[tokio::test]
async fn operations_come_from_the_mcp_tool_list() {
    let tools = json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": [{"name": "create_repository", "description": "Create a repository.", "inputSchema": {"type": "object"}}]}}).to_string();
    let (url, seen) = server(200, "", tools).await;
    let machine = cli(
        &["--url", &format!("{url}/"), "--json", "operations"],
        Some("t"),
        "",
    )
    .await;
    assert_eq!(machine.status, exit::SUCCESS, "{}", machine.stderr);
    assert_eq!(
        serde_json::from_str::<Value>(&machine.stdout).unwrap()["tools"][0]["name"],
        "create_repository"
    );
    let human = cli(&["--url", &url, "operations"], Some("t"), "").await;
    assert!(human.stdout.contains("create_repository"));
    assert!(human.stdout.contains("Create a repository."));
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].request_line(), "POST /mcp HTTP/1.1");
    assert_eq!(
        serde_json::from_str::<Value>(&seen[0].body).unwrap()["method"],
        "tools/list"
    );
}
