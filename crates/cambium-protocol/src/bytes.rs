use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

use crate::ApiError;

/// Decode one member of a plain/base64 field pair without normalization.
pub fn decode_pair(
    plain: Option<&str>,
    encoded: Option<&str>,
    field: &str,
) -> Result<Vec<u8>, ApiError> {
    match (plain, encoded) {
        (Some(value), None) => Ok(value.as_bytes().to_vec()),
        (None, Some(value)) => {
            let decoded = STANDARD
                .decode(value)
                .map_err(|_| ApiError::invalid(field, "Expected standard padded base64"))?;
            if STANDARD.encode(&decoded) != value {
                return Err(ApiError::invalid(field, "Expected canonical padded base64"));
            }
            Ok(decoded)
        }
        _ => Err(ApiError::invalid(
            field,
            "Send exactly one plain or _b64 field",
        )),
    }
}

pub fn encode_pair(bytes: &[u8], always_base64: bool) -> (Option<String>, Option<String>) {
    if !always_base64 && let Ok(text) = std::str::from_utf8(bytes) {
        return (Some(text.to_owned()), None);
    }
    (None, Some(STANDARD.encode(bytes)))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathValue {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_b64: Option<String>,
}

impl PathValue {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let (path, path_b64) = encode_pair(bytes, false);
        Self { path, path_b64 }
    }

    pub fn decode(&self) -> Result<Vec<u8>, ApiError> {
        decode_pair(self.path.as_deref(), self.path_b64.as_deref(), "path")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_pairs_preserve_non_utf8_and_normalization() {
        for original in [
            b"caf\xe9".as_slice(),
            "café".as_bytes(),
            "cafe\u{301}".as_bytes(),
            b"",
        ] {
            let encoded = PathValue::from_bytes(original);
            let json = serde_json::to_string(&encoded).unwrap();
            assert!(!json.contains("null"));
            let parsed: PathValue = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.decode().unwrap(), original);
        }
        assert!(decode_pair(Some("a"), Some("YQ=="), "path").is_err());
        assert!(decode_pair(None, None, "path").is_err());
        assert!(decode_pair(None, Some("YQ"), "path").is_err());
        assert!(serde_json::from_str::<PathValue>(r#"{"path":"x","typo":1}"#).is_err());
    }
}
