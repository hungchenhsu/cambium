//! OpenAPI description of the HTTP operations and the full error registry (ADR-0012).
//!
//! The document is derived from the same request schemas that MCP `tools/list`
//! publishes and from the `cambium-protocol` registry. `docs/api/openapi.json`
//! is checked against this output by a test, so the published contract cannot
//! drift from the code.

use cambium_protocol::{ErrorCode, WarningCode};
use serde_json::{Map, Value, json};

use crate::transport;

/// The published file is pretty-printed for review; the server never emits it.
pub const PUBLISHED_PATH: &str = "docs/api/openapi.json";

pub fn document() -> Value {
    let mut schemas = Map::new();
    let mut paths = Map::new();
    for tool in transport::tools() {
        let name = tool["name"].as_str().expect("tool names are strings");
        let read_only = tool["annotations"]["readOnlyHint"] == true;
        let input = hoist_definitions(tool["inputSchema"].clone(), &mut schemas);
        let schema_name = pascal_case(name) + "Input";
        insert_unique(&mut schemas, schema_name.clone(), input);
        let mut parameters = Vec::new();
        if !read_only {
            parameters.push(json!({"$ref": "#/components/parameters/IdempotencyKey"}));
        }
        let mut operation = json!({
            "operationId": name,
            "summary": tool["description"],
            "tags": [tag(name)],
            "requestBody": {
                "required": true,
                "content": {"application/json": {"schema": {"$ref": format!("#/components/schemas/{schema_name}")}}}
            },
            "responses": responses(),
        });
        if !parameters.is_empty() {
            operation["parameters"] = Value::Array(parameters);
        }
        paths.insert(format!("/v1/operations/{name}"), json!({"post": operation}));
    }
    schemas.insert("ErrorResponse".into(), error_response_schema());
    schemas.insert("Warning".into(), warning_schema());
    schemas.insert(
        "OperationResult".into(),
        json!({
            "type": "object",
            "description": "Operation-specific JSON object; see docs/api/operations.md. Absent values are omitted, never null. Unknown fields must be ignored.",
            "properties": {"warnings": {"type": "array", "items": {"$ref": "#/components/schemas/Warning"}}},
            "additionalProperties": true
        }),
    );
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Cambium API",
            "version": "v1",
            "description": "JSON operations over HTTPS (ADR-0012). Every request is authenticated with a Bearer token that selects the tenant. Unknown request fields fail with invalid_request. Machine output is English and never depends on locale."
        },
        "servers": [{"url": "http://127.0.0.1:8080", "description": "Local development listener"}],
        "security": [{"bearer": []}],
        "paths": paths,
        "components": {
            "securitySchemes": {"bearer": {"type": "http", "scheme": "bearer", "description": "Opaque, short-lived Cambium token (ADR-0011)."}},
            "parameters": {"IdempotencyKey": {
                "name": "Idempotency-Key",
                "in": "header",
                "required": false,
                "description": "1 to 255 printable ASCII characters. Must match idempotency_key when both are sent. Replays return Idempotent-Replayed: true.",
                "schema": {"type": "string", "minLength": 1, "maxLength": 255, "pattern": "^[ -~]+$"}
            }},
            "headers": {
                "RequestId": {"description": "Identifies the request in server logs.", "schema": {"type": "string", "pattern": "^req_[0-9a-z]{26}$"}},
                "IdempotentReplayed": {"description": "Present with the value true when the stored response of an earlier request is replayed.", "schema": {"type": "string", "enum": ["true"]}},
                "RetryAfter": {"description": "Seconds to wait before retrying, when the server knows a useful delay.", "schema": {"type": "integer", "minimum": 0}}
            },
            "responses": error_responses(),
            "schemas": schemas,
        },
        "x-cambium-error-codes": registry(),
        "x-cambium-warning-codes": warning_registry(),
    })
}

fn responses() -> Value {
    let mut responses = Map::new();
    responses.insert(
        "200".into(),
        json!({
            "description": "The operation succeeded.",
            "headers": {
                "Request-Id": {"$ref": "#/components/headers/RequestId"},
                "Idempotent-Replayed": {"$ref": "#/components/headers/IdempotentReplayed"}
            },
            "content": {"application/json": {"schema": {"$ref": "#/components/schemas/OperationResult"}}}
        }),
    );
    for status in statuses() {
        responses.insert(
            status.to_string(),
            json!({"$ref": format!("#/components/responses/Error{status}")}),
        );
    }
    Value::Object(responses)
}

fn statuses() -> Vec<u16> {
    let mut statuses: Vec<_> = ErrorCode::ALL.iter().map(|code| code.status()).collect();
    statuses.sort_unstable();
    statuses.dedup();
    statuses
}

fn error_responses() -> Value {
    let mut responses = Map::new();
    for status in statuses() {
        let codes: Vec<_> = ErrorCode::ALL
            .into_iter()
            .filter(|code| code.status() == status)
            .map(code_name)
            .collect();
        let mut headers = json!({"Request-Id": {"$ref": "#/components/headers/RequestId"}});
        if ErrorCode::ALL
            .into_iter()
            .any(|code| code.status() == status && code.retryable())
        {
            headers["Retry-After"] = json!({"$ref": "#/components/headers/RetryAfter"});
        }
        if status == 401 {
            headers["WWW-Authenticate"] = json!({"description": "Always Bearer.", "schema": {"type": "string", "enum": ["Bearer"]}});
        }
        responses.insert(
            format!("Error{status}"),
            json!({
                "description": format!("Error codes: {}.", codes.join(", ")),
                "headers": headers,
                "content": {"application/json": {"schema": {"$ref": "#/components/schemas/ErrorResponse"}}}
            }),
        );
    }
    Value::Object(responses)
}

fn error_response_schema() -> Value {
    let variants: Vec<_> = ErrorCode::ALL
        .into_iter()
        .map(|code| {
            json!({
                "type": "object",
                "properties": {
                    "code": {"const": code_name(code)},
                    "message": {"type": "string", "description": "English text for people and logs. Never translated; may change."},
                    "retryable": {"const": code.retryable()},
                    "details": code.details_schema(),
                    "request_id": {"type": "string", "pattern": "^req_[0-9a-z]{26}$"}
                },
                "required": ["code", "message", "retryable", "details", "request_id"],
                "additionalProperties": false
            })
        })
        .collect();
    json!({
        "type": "object",
        "description": "Clients branch on error.code. Treat an unknown code by its HTTP status and retryable flag.",
        "properties": {"error": {"oneOf": variants}},
        "required": ["error"],
        "additionalProperties": false
    })
}

fn warning_schema() -> Value {
    let variants: Vec<_> = WarningCode::ALL
        .into_iter()
        .map(|code| {
            json!({
                "type": "object",
                "properties": {
                    "code": {"const": warning_name(code)},
                    "message": {"type": "string"},
                    "details": code.details_schema()
                },
                "required": ["code", "message", "details"],
                "additionalProperties": false
            })
        })
        .collect();
    json!({"description": "Clients ignore warning codes they do not know.", "oneOf": variants})
}

fn registry() -> Value {
    Value::Array(
        ErrorCode::ALL
            .into_iter()
            .map(|code| {
                json!({
                    "code": code_name(code),
                    "status": code.status(),
                    "retryable": code.retryable(),
                    "message": code.message(),
                    "details": code.details(),
                })
            })
            .collect(),
    )
}

fn warning_registry() -> Value {
    Value::Array(
        WarningCode::ALL
            .into_iter()
            .map(|code| {
                json!({"code": warning_name(code), "message": code.message(), "details": code.details()})
            })
            .collect(),
    )
}

fn code_name(code: ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .expect("error codes serialize as strings")
}

fn warning_name(code: WarningCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .expect("warning codes serialize as strings")
}

fn tag(name: &str) -> &'static str {
    match name {
        "create_identity" | "get_identity" | "mint_token" | "revoke_token" | "disable_identity" => {
            "identities"
        }
        "create_ephemeral_branch"
        | "get_branch"
        | "list_branches"
        | "extend_branch"
        | "promote_branch" => "branches",
        "write_changeset" | "read_file" | "list_tree" | "diff" | "get_commit_provenance" => {
            "content"
        }
        _ => "repositories",
    }
}

fn pascal_case(name: &str) -> String {
    name.split('_')
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect()
}

/// Moves `$defs` into `components/schemas` and rewrites local references.
fn hoist_definitions(mut schema: Value, schemas: &mut Map<String, Value>) -> Value {
    if let Some(Value::Object(definitions)) = schema.as_object_mut().and_then(|s| s.remove("$defs"))
    {
        for (name, definition) in definitions {
            insert_unique(schemas, name, rewrite_refs(definition));
        }
    }
    if let Some(object) = schema.as_object_mut() {
        object.remove("$schema");
        object.remove("title");
    }
    rewrite_refs(schema)
}

fn rewrite_refs(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| {
                    let value =
                        match (key.as_str(), value) {
                            ("$ref", Value::String(reference)) => Value::String(
                                reference.replacen("#/$defs/", "#/components/schemas/", 1),
                            ),
                            (_, value) => rewrite_refs(value),
                        };
                    (key, value)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(rewrite_refs).collect()),
        value => value,
    }
}

/// Two request types that share a definition name must share its shape.
fn insert_unique(schemas: &mut Map<String, Value>, name: String, schema: Value) {
    if let Some(existing) = schemas.get(&name) {
        assert_eq!(
            existing, &schema,
            "conflicting schema definitions for {name}"
        );
    } else {
        schemas.insert(name, schema);
    }
}

/// Stable, reviewable serialization: serde_json maps keep keys sorted.
pub fn render() -> String {
    serde_json::to_string_pretty(&document()).expect("the document serializes") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk_refs<'a>(value: &'a Value, found: &mut Vec<&'a str>) {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    if key == "$ref" {
                        found.push(value.as_str().unwrap());
                    } else {
                        walk_refs(value, found);
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| walk_refs(item, found)),
            _ => {}
        }
    }

    #[test]
    fn every_reference_resolves_inside_the_document() {
        let document = document();
        let mut references = Vec::new();
        walk_refs(&document, &mut references);
        assert!(!references.is_empty());
        for reference in references {
            let pointer = reference
                .strip_prefix('#')
                .unwrap_or_else(|| panic!("external reference {reference}"));
            assert!(document.pointer(pointer).is_some(), "{reference}");
        }
    }

    #[test]
    fn every_tool_is_an_operation_and_every_code_is_published() {
        let document = document();
        let paths = document["paths"].as_object().unwrap();
        assert_eq!(paths.len(), transport::tools().len());
        let variants =
            document["components"]["schemas"]["ErrorResponse"]["properties"]["error"]["oneOf"]
                .as_array()
                .unwrap();
        assert_eq!(variants.len(), ErrorCode::ALL.len());
        for tool in transport::tools() {
            let path = format!("/v1/operations/{}", tool["name"].as_str().unwrap());
            let operation = &paths[&path]["post"];
            let mutating = tool["annotations"]["readOnlyHint"] == false;
            assert_eq!(operation["parameters"].is_array(), mutating, "{path}");
            for status in statuses() {
                assert!(operation["responses"][status.to_string()].is_object());
            }
        }
    }

    #[test]
    fn published_document_matches_the_code() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/api/openapi.json");
        let rendered = render();
        if std::env::var_os("CAMBIUM_UPDATE_OPENAPI").is_some() {
            std::fs::write(path, &rendered).unwrap();
        }
        let published = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            published == rendered,
            "{PUBLISHED_PATH} is stale; run CAMBIUM_UPDATE_OPENAPI=1 cargo test -p cambium-server --lib openapi"
        );
    }
}
