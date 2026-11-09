//! A thin command-line client for the Cambium API (ADR-0019).
//!
//! `call` sends one operation request; `operations` lists what the server
//! offers. With `--json`, standard output is exactly the API's response body.

use std::{
    ffi::OsString,
    io::{Read, Write},
    path::PathBuf,
};

use clap::{Parser, Subcommand};
use serde_json::{Value, json};

mod messages;

use messages::Catalog;

pub const DEFAULT_URL: &str = "http://127.0.0.1:8080";

/// Exit statuses, part of the CLI's machine contract.
pub mod exit {
    pub const SUCCESS: i32 = 0;
    pub const API_ERROR: i32 = 1;
    pub const USAGE: i32 = 2;
    pub const TRANSPORT: i32 = 3;
}

#[derive(Parser)]
#[command(
    name = "cambium-cli",
    version,
    about = "Thin client for the Cambium API"
)]
struct Cli {
    /// Base URL of the Cambium API. Defaults to CAMBIUM_URL, then http://127.0.0.1:8080.
    #[arg(long, global = true)]
    url: Option<String>,
    /// Read the bearer token from this file instead of CAMBIUM_TOKEN.
    #[arg(long, global = true)]
    token_file: Option<PathBuf>,
    /// Print exactly the API's JSON response body on one line.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Send one operation request: POST /v1/operations/<operation>.
    Call {
        /// Operation name, such as create_repository.
        operation: String,
        /// JSON object, @path to read a file, or - to read standard input. Defaults to {}.
        #[arg(long)]
        input: Option<String>,
        /// Sent as the Idempotency-Key header. Reuse it to retry a mutation safely.
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// List the server's operations and their input schemas.
    Operations,
}

/// Values the CLI reads from its environment, passed in so tests control them.
#[derive(Clone, Debug, Default)]
pub struct Environment {
    pub token: Option<String>,
    pub url: Option<String>,
}

impl Environment {
    pub fn from_process() -> Self {
        Self {
            token: std::env::var("CAMBIUM_TOKEN").ok(),
            url: std::env::var("CAMBIUM_URL").ok(),
        }
    }
}

pub struct Io<'a> {
    pub stdin: &'a mut dyn Read,
    pub stdout: &'a mut dyn Write,
    pub stderr: &'a mut dyn Write,
}

/// Runs one CLI invocation and returns its exit status.
pub async fn run<I, T>(args: I, environment: Environment, io: Io<'_>) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let catalog = Catalog::english();
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            let rendered = error.render().to_string();
            if error.use_stderr() {
                let _ = io.stderr.write_all(rendered.as_bytes());
                return exit::USAGE;
            }
            let _ = io.stdout.write_all(rendered.as_bytes());
            return exit::SUCCESS;
        }
    };
    let url = cli
        .url
        .clone()
        .or(environment.url.clone())
        .unwrap_or_else(|| DEFAULT_URL.to_owned());
    let url = url.trim_end_matches('/').to_owned();
    let token = match token(&cli, &environment, &catalog) {
        Ok(token) => token,
        Err(message) => {
            let _ = writeln!(io.stderr, "{message}");
            return exit::USAGE;
        }
    };
    let client = Client {
        http: reqwest::Client::new(),
        url,
        token,
        json: cli.json,
        catalog,
    };
    match cli.command {
        Command::Call {
            operation,
            input,
            idempotency_key,
        } => {
            client
                .call(&operation, input.as_deref(), idempotency_key.as_deref(), io)
                .await
        }
        Command::Operations => client.operations(io).await,
    }
}

fn token(cli: &Cli, environment: &Environment, catalog: &Catalog) -> Result<String, String> {
    let token = match &cli.token_file {
        Some(path) => std::fs::read_to_string(path).map_err(|_| {
            catalog.format("unreadable-token", &[("path", &path.display().to_string())])
        })?,
        None => environment
            .token
            .clone()
            .ok_or_else(|| catalog.format("missing-token", &[]))?,
    };
    let token = token.trim().to_owned();
    if token.is_empty() {
        return Err(catalog.format("missing-token", &[]));
    }
    Ok(token)
}

struct Client {
    http: reqwest::Client,
    url: String,
    token: String,
    json: bool,
    catalog: Catalog,
}

/// An HTTP answer whose body is JSON.
struct Answer {
    status: u16,
    replayed: bool,
    body: Value,
    raw: String,
}

impl Client {
    async fn call(
        &self,
        operation: &str,
        input: Option<&str>,
        idempotency_key: Option<&str>,
        io: Io<'_>,
    ) -> i32 {
        // The name becomes a URL path segment; the server's names are snake_case.
        if operation.is_empty()
            || !operation
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            let _ = writeln!(
                io.stderr,
                "{}",
                self.catalog.format(
                    "invalid-input",
                    &[("reason", "operation names are lowercase snake_case")]
                )
            );
            return exit::USAGE;
        }
        let input = match read_input(input, io.stdin, &self.catalog) {
            Ok(input) => input,
            Err(message) => {
                let _ = writeln!(io.stderr, "{message}");
                return exit::USAGE;
            }
        };
        let endpoint = format!("{}/v1/operations/{operation}", self.url);
        let mut request = self
            .http
            .post(&endpoint)
            .bearer_auth(&self.token)
            .header("content-type", "application/json")
            .body(input.to_string());
        if let Some(key) = idempotency_key {
            request = request.header("idempotency-key", key);
        }
        match self.send(request, &endpoint).await {
            Ok(answer) => self.report(answer, &endpoint, io, |body| body),
            Err(message) => {
                let _ = writeln!(io.stderr, "{message}");
                exit::TRANSPORT
            }
        }
    }

    async fn operations(&self, io: Io<'_>) -> i32 {
        let endpoint = format!("{}/mcp", self.url);
        let request = self
            .http
            .post(&endpoint)
            .bearer_auth(&self.token)
            .header("content-type", "application/json")
            .body(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string());
        let answer = match self.send(request, &endpoint).await {
            Ok(answer) => answer,
            Err(message) => {
                let _ = writeln!(io.stderr, "{message}");
                return exit::TRANSPORT;
            }
        };
        if answer.status < 400 && !answer.body["result"]["tools"].is_array() {
            return self.unexpected(&endpoint, answer.status, io);
        }
        if answer.status >= 400 {
            return self.report(answer, &endpoint, io, |body| body);
        }
        let tools = answer.body["result"].clone();
        if self.json {
            let _ = writeln!(io.stdout, "{tools}");
            return exit::SUCCESS;
        }
        let _ = writeln!(
            io.stdout,
            "{}",
            self.catalog
                .format("operations-header", &[("url", &self.url)])
        );
        for tool in tools["tools"].as_array().into_iter().flatten() {
            let _ = writeln!(
                io.stdout,
                "  {:<26} {}",
                tool["name"].as_str().unwrap_or_default(),
                tool["description"].as_str().unwrap_or_default()
            );
        }
        exit::SUCCESS
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        endpoint: &str,
    ) -> Result<Answer, String> {
        let unreachable = |error: reqwest::Error| {
            self.catalog.format(
                "unreachable",
                &[
                    ("url", endpoint),
                    ("reason", &error.without_url().to_string()),
                ],
            )
        };
        let response = request.send().await.map_err(unreachable)?;
        let status = response.status().as_u16();
        let replayed = response
            .headers()
            .get("idempotent-replayed")
            .is_some_and(|value| value == "true");
        let raw = response.text().await.map_err(unreachable)?;
        let body = serde_json::from_str(&raw).unwrap_or(Value::Null);
        Ok(Answer {
            status,
            replayed,
            body,
            raw,
        })
    }

    fn report(
        &self,
        answer: Answer,
        endpoint: &str,
        io: Io<'_>,
        select: impl Fn(&Value) -> &Value,
    ) -> i32 {
        let error = &answer.body["error"];
        let is_api_error = answer.status >= 400
            && error["code"].is_string()
            && error["message"].is_string()
            && error["request_id"].is_string();
        if !answer.body.is_object() || (answer.status >= 400 && !is_api_error) {
            return self.unexpected(endpoint, answer.status, io);
        }
        if self.json {
            let _ = writeln!(io.stdout, "{}", answer.raw.trim_end());
        } else if is_api_error {
            let field = |name: &str| error[name].as_str().unwrap_or_default().to_owned();
            let _ = writeln!(
                io.stderr,
                "{}",
                self.catalog.format(
                    "api-error",
                    &[
                        ("code", &field("code")),
                        ("status", &answer.status.to_string()),
                        ("message", &field("message")),
                    ],
                )
            );
            if !error["details"].as_object().is_none_or(|d| d.is_empty()) {
                let _ = writeln!(
                    io.stderr,
                    "{}",
                    serde_json::to_string_pretty(&error["details"]).unwrap_or_default()
                );
            }
            let _ = writeln!(
                io.stderr,
                "{}",
                self.catalog
                    .format("api-error-request", &[("request_id", &field("request_id"))])
            );
            if error["retryable"] == true {
                let _ = writeln!(
                    io.stderr,
                    "{}",
                    self.catalog.format("api-error-retryable", &[])
                );
            }
        } else {
            let _ = writeln!(
                io.stdout,
                "{}",
                serde_json::to_string_pretty(select(&answer.body)).unwrap_or_default()
            );
        }
        if answer.replayed && !self.json {
            let _ = writeln!(io.stderr, "{}", self.catalog.format("replayed", &[]));
        }
        if is_api_error {
            exit::API_ERROR
        } else {
            exit::SUCCESS
        }
    }

    fn unexpected(&self, endpoint: &str, status: u16, io: Io<'_>) -> i32 {
        let _ = writeln!(
            io.stderr,
            "{}",
            self.catalog.format(
                "unexpected-response",
                &[("url", endpoint), ("status", &status.to_string())],
            )
        );
        exit::TRANSPORT
    }
}

fn read_input(
    input: Option<&str>,
    stdin: &mut dyn Read,
    catalog: &Catalog,
) -> Result<Value, String> {
    let text = match input {
        None => "{}".to_owned(),
        Some("-") => {
            let mut text = String::new();
            stdin
                .read_to_string(&mut text)
                .map_err(|_| catalog.format("unreadable-input", &[("source", "standard input")]))?;
            text
        }
        Some(value) => match value.strip_prefix('@') {
            Some(path) => std::fs::read_to_string(path)
                .map_err(|_| catalog.format("unreadable-input", &[("source", path)]))?,
            None => value.to_owned(),
        },
    };
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| catalog.format("invalid-input", &[("reason", &error.to_string())]))?;
    if !value.is_object() {
        return Err(catalog.format("invalid-input", &[("reason", "expected an object")]));
    }
    Ok(value)
}
