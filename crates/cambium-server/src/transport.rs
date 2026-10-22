//! JSON API and stateless MCP Streamable HTTP on the public listener.

use std::sync::Arc;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Path, Request, State, rejection::PathRejection},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
};
use cambium_backend_enroute::hooks::{HookAdapter, HookError, MAX_HOOK_BYTES};
use cambium_protocol::{ApiError, ErrorCode, RequestId};
use schemars::JsonSchema;
use serde_json::{Value, json};

use crate::{
    git_policy::bearer,
    requests,
    service::{App, Reply},
};

pub const MAX_API_BYTES: usize = 16 * 1024 * 1024;
const MCP_VERSION: &str = "2025-11-25";

#[derive(Clone)]
pub struct PublicState {
    pub app: Arc<App>,
    /// Exact origins, such as https://cambium.example. Empty permits no browser origin.
    pub allowed_origins: Vec<String>,
}

pub fn public_router(state: PublicState) -> Router {
    Router::new()
        .route("/v1/operations/{operation}", any(operation))
        .route("/mcp", any(mcp))
        .fallback(fallback)
        .layer(middleware::from_fn_with_state(state.clone(), cors))
        .with_state(state)
}

pub fn hook_router(adapter: Arc<HookAdapter>) -> Router {
    Router::new()
        .route("/api/enroute/hooks", any(hook))
        .with_state(adapter)
}

async fn authenticated(state: &PublicState, headers: &HeaderMap) -> Result<String, ApiError> {
    let values: Vec<_> = headers.get_all(header::AUTHORIZATION).iter().collect();
    if values.len() != 1 {
        return Err(ApiError::new(ErrorCode::Unauthenticated));
    }
    let token = bearer(values[0].as_bytes())?;
    state.app.authenticate(&token).await?;
    Ok(token)
}

fn check_origin(state: &PublicState, headers: &HeaderMap) -> Result<(), ApiError> {
    let origins: Vec<_> = headers.get_all(header::ORIGIN).iter().collect();
    if origins.is_empty() {
        return Ok(());
    }
    if origins.len() != 1
        || origins[0]
            .to_str()
            .ok()
            .is_none_or(|v| !state.allowed_origins.iter().any(|allowed| allowed == v))
    {
        return Err(ApiError::new(ErrorCode::PermissionDenied));
    }
    Ok(())
}

const CORS_HEADERS: &str =
    "authorization, content-type, idempotency-key, mcp-protocol-version, accept";

async fn cors(State(state): State<PublicState>, request: Request, next: Next) -> Response {
    if let Err(error) = check_origin(&state, request.headers()) {
        return reply_response(Reply::error(error, RequestId::new()));
    }
    let origin = request.headers().get(header::ORIGIN).cloned();
    let preflight = request.method() == Method::OPTIONS
        && origin.is_some()
        && request
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);
    let mut response = if preflight {
        let methods: Vec<_> = request
            .headers()
            .get_all(header::ACCESS_CONTROL_REQUEST_METHOD)
            .iter()
            .collect();
        let valid_method = methods.len() == 1 && methods[0] == "POST";
        let valid_headers = request
            .headers()
            .get_all(header::ACCESS_CONTROL_REQUEST_HEADERS)
            .iter()
            .all(|value| {
                value.to_str().is_ok_and(|value| {
                    value.split(',').all(|name| {
                        CORS_HEADERS
                            .split(", ")
                            .any(|allowed| allowed.eq_ignore_ascii_case(name.trim()))
                    })
                })
            });
        if !valid_method || !valid_headers {
            return reply_response(Reply::error(
                ApiError::new(ErrorCode::PermissionDenied),
                RequestId::new(),
            ));
        }
        let mut response = StatusCode::NO_CONTENT.into_response();
        response.headers_mut().insert(
            "request-id",
            HeaderValue::from_str(RequestId::new().as_str())
                .expect("generated request IDs are ASCII"),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("POST"),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(CORS_HEADERS),
        );
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    } else {
        next.run(request).await
    };
    response.headers_mut().append(
        header::VARY,
        HeaderValue::from_static(
            "Origin, Access-Control-Request-Method, Access-Control-Request-Headers",
        ),
    );
    if let Some(origin) = origin {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        response.headers_mut().insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static(
                "Request-Id, Idempotent-Replayed, Retry-After, MCP-Protocol-Version",
            ),
        );
    }
    response
}

fn idempotency_header(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let values: Vec<_> = headers.get_all("idempotency-key").iter().collect();
    match values.as_slice() {
        [] => Ok(None),
        [value] => value
            .to_str()
            .map(|v| Some(v.to_owned()))
            .map_err(|_| ApiError::invalid("idempotency_key", "Expected printable ASCII")),
        _ => Err(ApiError::invalid(
            "idempotency_key",
            "Send one Idempotency-Key header",
        )),
    }
}

async fn json_body(request: Request) -> Result<Value, ApiError> {
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| {
            !v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
    {
        return Err(ApiError::invalid(
            "content_type",
            "Expected application/json",
        ));
    }
    let bytes = to_bytes(request.into_body(), MAX_API_BYTES)
        .await
        .map_err(|_| {
            let mut error = ApiError::new(ErrorCode::PayloadTooLarge);
            error
                .details
                .insert("limit_bytes".into(), json!(MAX_API_BYTES));
            error
        })?;
    serde_json::from_slice(&bytes).map_err(|_| ApiError::invalid("body", "Expected a JSON object"))
}

async fn operation(
    State(state): State<PublicState>,
    path: Result<Path<String>, PathRejection>,
    request: Request,
) -> Response {
    let result = async {
        let token = authenticated(&state, request.headers()).await?;
        let Path(name) =
            path.map_err(|_| ApiError::invalid("operation", "Expected a UTF-8 operation name"))?;
        if request.method() != Method::POST {
            return Err(ApiError::invalid(
                "method",
                "Use POST for operation requests",
            ));
        }
        let key = idempotency_header(request.headers())?;
        let input = json_body(request).await?;
        Ok::<_, ApiError>(state.app.call(token, name, input, key).await)
    }
    .await;
    reply_response(result.unwrap_or_else(|error| Reply::error(error, RequestId::new())))
}

async fn fallback(State(state): State<PublicState>, request: Request) -> Response {
    let error = match authenticated(&state, request.headers()).await {
        Err(error) => error,
        Ok(_) => ApiError::new(ErrorCode::NotFound),
    };
    reply_response(Reply::error(error, RequestId::new()))
}

fn reply_response(reply: Reply) -> Response {
    let mut response = json_response(reply.status, reply.body, &reply.request_id);
    if reply.replayed {
        response
            .headers_mut()
            .insert("idempotent-replayed", HeaderValue::from_static("true"));
    }
    response
}

fn json_response(status: u16, value: Value, request_id: &RequestId) -> Response {
    let mut response = (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        [(header::CONTENT_TYPE, "application/json")],
        value.to_string(),
    )
        .into_response();
    response.headers_mut().insert(
        "request-id",
        HeaderValue::from_str(request_id.as_str()).expect("generated request IDs are ASCII"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if status == 401 {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    if matches!(status, 503) {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
    response
}

async fn mcp(State(state): State<PublicState>, request: Request) -> Response {
    let request_id = RequestId::new();
    let token = match authenticated(&state, request.headers()).await {
        Ok(token) => token,
        Err(error) => return reply_response(Reply::error(error, request_id)),
    };
    if request.method() != Method::POST {
        let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST"));
        response.headers_mut().insert(
            "request-id",
            HeaderValue::from_str(request_id.as_str()).expect("generated request IDs are ASCII"),
        );
        return response;
    }
    if let Some(version) = request.headers().get("mcp-protocol-version")
        && version != MCP_VERSION
        && version != "2025-06-18"
        && version != "2025-03-26"
    {
        return rpc_error(
            400,
            None,
            -32600,
            "Unsupported protocol version",
            &request_id,
        );
    }
    let message = match json_body(request).await {
        Ok(value) => value,
        Err(error)
            if error.code == ErrorCode::InvalidRequest
                && error.details.get("field").and_then(Value::as_str) == Some("body") =>
        {
            return rpc_error(400, None, -32700, "Parse error", &request_id);
        }
        Err(error) => return reply_response(Reply::error(error, request_id)),
    };
    let Some(object) = message.as_object() else {
        return rpc_error(
            400,
            None,
            -32600,
            "Expected one JSON-RPC message",
            &request_id,
        );
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return rpc_error(400, None, -32600, "Expected JSON-RPC 2.0", &request_id);
    }
    let id = object.get("id").cloned();
    if id
        .as_ref()
        .is_some_and(|id| !id.is_string() && !id.is_i64() && !id.is_u64())
    {
        return rpc_error(400, None, -32600, "Invalid request ID", &request_id);
    }
    let method = object.get("method").and_then(Value::as_str);
    if id.is_none()
        || method.is_none() && (object.contains_key("result") || object.contains_key("error"))
    {
        if method.is_none() && !object.contains_key("result") && !object.contains_key("error") {
            return rpc_error(400, None, -32600, "Invalid message", &request_id);
        }
        let mut response = StatusCode::ACCEPTED.into_response();
        response.headers_mut().insert(
            "request-id",
            HeaderValue::from_str(request_id.as_str()).expect("generated request IDs are ASCII"),
        );
        return response;
    }
    let id = id.expect("request ID checked above");
    let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return rpc_error(
            200,
            Some(id),
            -32602,
            "Expected object parameters",
            &request_id,
        );
    }
    let result = match method {
        Some("initialize") => {
            let Some(requested) = params.get("protocolVersion").and_then(Value::as_str) else {
                return rpc_error(
                    200,
                    Some(id),
                    -32602,
                    "protocolVersion is required",
                    &request_id,
                );
            };
            if !params.get("capabilities").is_some_and(Value::is_object)
                || !params.get("clientInfo").is_some_and(|info| {
                    info.get("name").is_some_and(Value::is_string)
                        && info.get("version").is_some_and(Value::is_string)
                })
            {
                return rpc_error(
                    200,
                    Some(id),
                    -32602,
                    "capabilities and clientInfo are required",
                    &request_id,
                );
            }
            let version = if [MCP_VERSION, "2025-06-18", "2025-03-26"].contains(&requested) {
                requested
            } else {
                MCP_VERSION
            };
            json!({"protocolVersion": version, "capabilities": {"tools": {"listChanged": false}}, "serverInfo": {"name": "cambium", "version": env!("CARGO_PKG_VERSION")}, "instructions": "Use the same idempotency_key when retrying a mutation. Tokens are scoped to one tenant; tenant IDs are never request parameters."})
        }
        Some("ping") => json!({}),
        Some("tools/list") => {
            if params.get("cursor").is_some() {
                return rpc_error(
                    200,
                    Some(id),
                    -32602,
                    "The tool list has no next page",
                    &request_id,
                );
            }
            json!({"tools": tools()})
        }
        Some("tools/call") => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return rpc_error(200, Some(id), -32602, "Tool name is required", &request_id);
            };
            if !tools().iter().any(|tool| tool["name"] == name) {
                return rpc_error(200, Some(id), -32602, "Unknown tool", &request_id);
            }
            let input = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let reply = state.app.call(token, name.to_owned(), input, None).await;
            let result = json!({"content": [{"type": "text", "text": reply.body.to_string()}], "structuredContent": reply.body, "isError": reply.status >= 400});
            let mut response = json_response(
                200,
                json!({"jsonrpc": "2.0", "id": id, "result": result}),
                &reply.request_id,
            );
            if reply.replayed {
                response
                    .headers_mut()
                    .insert("idempotent-replayed", HeaderValue::from_static("true"));
            }
            return response;
        }
        _ => return rpc_error(200, Some(id), -32601, "Method not found", &request_id),
    };
    json_response(
        200,
        json!({"jsonrpc": "2.0", "id": id, "result": result}),
        &request_id,
    )
}

fn rpc_error(
    status: u16,
    id: Option<Value>,
    code: i32,
    message: &str,
    request_id: &RequestId,
) -> Response {
    // JSON-RPC requires null when the request ID could not be determined.
    let value = json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}});
    json_response(status, value, request_id)
}

fn tool<T: JsonSchema>(name: &str, description: &str, read_only: bool, destructive: bool) -> Value {
    let mut schema =
        serde_json::to_value(schemars::schema_for!(T)).expect("request schemas serialize");
    schema["properties"]["idempotency_key"] = json!({"type": "string", "minLength": 1, "maxLength": 255, "pattern": "^[ -~]+$", "description": "Reuse this key to retry the same mutation safely."});
    json!({"name": name, "description": description, "inputSchema": schema, "annotations": {"readOnlyHint": read_only, "destructiveHint": destructive, "openWorldHint": false}})
}

pub fn tools() -> Vec<Value> {
    vec![
        tool::<requests::CreateRepository>(
            "create_repository",
            "Create a repository in the authenticated tenant.",
            false,
            false,
        ),
        tool::<requests::Repository>(
            "get_repository",
            "Read repository metadata and its stock-git URL.",
            true,
            false,
        ),
        tool::<requests::ListRepositories>(
            "list_repositories",
            "List visible repositories with pagination.",
            true,
            false,
        ),
        tool::<requests::Repository>(
            "delete_repository",
            "Delete a repository and all its storage generations.",
            false,
            true,
        ),
        tool::<requests::Repository>(
            "request_compaction",
            "Request immediate storage reclamation for a repository.",
            false,
            true,
        ),
        tool::<requests::GetReclamation>(
            "get_reclamation",
            "Read reclamation progress, overdue status, and conflict rescue refs.",
            true,
            false,
        ),
        tool::<requests::CreateEphemeralBranch>(
            "create_ephemeral_branch",
            "Create a recoverable branch with a bounded TTL.",
            false,
            false,
        ),
        tool::<requests::Branch>("get_branch", "Read a branch lifecycle record.", true, false),
        tool::<requests::ListBranches>(
            "list_branches",
            "List branch lifecycle records with pagination.",
            true,
            false,
        ),
        tool::<requests::ExtendBranch>(
            "extend_branch",
            "Extend a live ephemeral branch from the request time without shortening its TTL.",
            false,
            false,
        ),
        tool::<requests::Branch>(
            "promote_branch",
            "Make a live ephemeral branch permanent.",
            false,
            false,
        ),
        tool::<requests::WriteChangeset>(
            "write_changeset",
            "Write file changes as one CAS-protected commit without a checkout.",
            false,
            true,
        ),
        tool::<requests::ReadFile>(
            "read_file",
            "Read exact file bytes at a revision without following symlinks.",
            true,
            false,
        ),
        tool::<requests::ListTree>(
            "list_tree",
            "List byte-preserving tree entries with pagination.",
            true,
            false,
        ),
        tool::<requests::Diff>(
            "diff",
            "Compare paths, modes, and object IDs between two revisions.",
            true,
            false,
        ),
        tool::<requests::CreateIdentity>(
            "create_identity",
            "Create a human or agent identity; agents require a human owner.",
            false,
            false,
        ),
        tool::<requests::Identity>(
            "get_identity",
            "Read an identity in the authenticated tenant.",
            true,
            false,
        ),
        tool::<requests::MintToken>(
            "mint_token",
            "Mint a short-lived token with no more privileges than the caller.",
            false,
            false,
        ),
        tool::<requests::Token>(
            "revoke_token",
            "Revoke a token and every token descended from it.",
            false,
            true,
        ),
        tool::<requests::Identity>(
            "disable_identity",
            "Disable an identity and invalidate its tokens.",
            false,
            true,
        ),
    ]
}

async fn hook(State(adapter): State<Arc<HookAdapter>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let authority = parts
        .uri
        .authority()
        .map(|v| v.as_str())
        .or_else(|| {
            parts
                .headers
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
        })
        .unwrap_or("");
    if parts.uri.query().is_some() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let bytes = match to_bytes(body, MAX_HOOK_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let now = cambium_backend_enroute::signature::now_unix_secs();
    match adapter
        .handle(
            parts.method.as_str(),
            authority,
            parts.uri.path(),
            &parts.headers,
            &bytes,
            now,
        )
        .await
    {
        Ok(bytes) => (
            [(header::CONTENT_TYPE, "application/protobuf")],
            Body::from(bytes),
        )
            .into_response(),
        Err(HookError::Signature) => StatusCode::UNAUTHORIZED.into_response(),
        Err(HookError::InvalidRequest) => StatusCode::BAD_REQUEST.into_response(),
        Err(HookError::TooLarge) => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Err(HookError::Unavailable) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
