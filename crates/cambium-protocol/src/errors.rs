use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::RequestId;

/// Stable public registry. New variants are additive within /v1.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthenticated,
    PermissionDenied,
    NotFound,
    InvalidRequest,
    AlreadyExists,
    RefConflict,
    BranchExpired,
    PayloadTooLarge,
    UnsupportedPath,
    IdempotencyKeyReused,
    IdempotencyKeyInUse,
    Contention,
    RepositoryBusy,
    BackendUnavailable,
    Internal,
}

impl ErrorCode {
    pub const ALL: [Self; 15] = [
        Self::Unauthenticated,
        Self::PermissionDenied,
        Self::NotFound,
        Self::InvalidRequest,
        Self::AlreadyExists,
        Self::RefConflict,
        Self::BranchExpired,
        Self::PayloadTooLarge,
        Self::UnsupportedPath,
        Self::IdempotencyKeyReused,
        Self::IdempotencyKeyInUse,
        Self::Contention,
        Self::RepositoryBusy,
        Self::BackendUnavailable,
        Self::Internal,
    ];

    pub const fn status(self) -> u16 {
        match self {
            Self::Unauthenticated => 401,
            Self::PermissionDenied => 403,
            Self::NotFound => 404,
            Self::InvalidRequest => 400,
            Self::AlreadyExists
            | Self::RefConflict
            | Self::IdempotencyKeyInUse
            | Self::Contention => 409,
            Self::BranchExpired => 410,
            Self::PayloadTooLarge => 413,
            Self::UnsupportedPath | Self::IdempotencyKeyReused => 422,
            Self::RepositoryBusy | Self::BackendUnavailable => 503,
            Self::Internal => 500,
        }
    }

    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::IdempotencyKeyInUse
                | Self::Contention
                | Self::RepositoryBusy
                | Self::BackendUnavailable
                | Self::Internal
        )
    }

    pub const fn cacheable(self) -> bool {
        matches!(
            self,
            Self::InvalidRequest
                | Self::AlreadyExists
                | Self::RefConflict
                | Self::BranchExpired
                | Self::PayloadTooLarge
                | Self::UnsupportedPath
        )
    }

    /// Object IDs and other absent detail values are omitted, never null.
    pub const fn details(self) -> &'static str {
        match self {
            Self::InvalidRequest => "field, reason",
            Self::RefConflict => "ref, expected?, actual?",
            Self::PayloadTooLarge => "limit_bytes",
            Self::UnsupportedPath => "path or path_b64",
            _ => "empty object",
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::Unauthenticated => "Authentication is required",
            Self::PermissionDenied => "The token lacks the required scope",
            Self::NotFound => "Resource not found",
            Self::InvalidRequest => "The request is invalid",
            Self::AlreadyExists => "The name is already in use",
            Self::RefConflict => "The ref does not match the expected value",
            Self::BranchExpired => "The ephemeral branch has expired",
            Self::PayloadTooLarge => "The request exceeds the size limit",
            Self::UnsupportedPath => "The backend cannot store this path",
            Self::IdempotencyKeyReused => "The idempotency key was used for a different request",
            Self::IdempotencyKeyInUse => "A request with this idempotency key is still running",
            Self::Contention => "Concurrent writers prevented the update",
            Self::RepositoryBusy => "The repository is temporarily fenced",
            Self::BackendUnavailable => "The storage backend did not answer",
            Self::Internal => "An internal error occurred; retry with the same idempotency key",
        }
    }
}

/// Operation errors contain only public details, never backend diagnostics.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{}", code.message())]
pub struct ApiError {
    pub code: ErrorCode,
    pub details: serde_json::Map<String, Value>,
}

impl ApiError {
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            details: serde_json::Map::new(),
        }
    }

    pub fn invalid(field: &str, reason: &str) -> Self {
        let mut error = Self::new(ErrorCode::InvalidRequest);
        error.details.insert("field".into(), json!(field));
        error.details.insert("reason".into(), json!(reason));
        error
    }

    pub fn response(&self, request_id: RequestId) -> ErrorResponse {
        ErrorResponse {
            error: ErrorBody {
                code: self.code,
                message: self.code.message().into(),
                retryable: self.code.retryable(),
                details: self.details.clone(),
                request_id,
            },
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ErrorResponse {
    pub error: ErrorBody,
}

#[derive(Clone, Debug, Serialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub details: serde_json::Map<String, Value>,
    pub request_id: RequestId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    PathCollision,
}

#[derive(Clone, Debug, Serialize)]
pub struct Warning {
    pub code: WarningCode,
    pub message: String,
    pub details: serde_json::Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_registry_matches_codes_status_retryability_and_details() {
        let document = include_str!("../../../docs/api/errors.md");
        let rows: Vec<_> = document
            .lines()
            .filter(|line| line.starts_with("| `"))
            .collect();
        assert_eq!(rows.len(), ErrorCode::ALL.len());
        for code in ErrorCode::ALL {
            let name = serde_json::to_value(code)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned();
            let expected = format!(
                "| `{name}` | {} | {} | {} |",
                code.status(),
                if code.retryable() { "Yes" } else { "No" },
                code.details()
            );
            assert!(rows.contains(&expected.as_str()), "{expected}");
        }
    }
}
