//! Token generation and deployment-key encryption of idempotency responses.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use cambium_protocol::{ApiError, ErrorCode, Principal};
use cambium_store::{TokenHash, operations::OperationKey};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

pub fn new_token() -> Result<String, ApiError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| internal())?;
    Ok(format!(
        "cmb_{}",
        data_encoding::BASE32_NOPAD
            .encode(&bytes)
            .to_ascii_lowercase()
    ))
}

pub fn token_hash(secret: &str) -> Result<TokenHash, ApiError> {
    let value = secret.strip_prefix("cmb_").ok_or_else(unauthenticated)?;
    if value.len() != 52
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    {
        return Err(unauthenticated());
    }
    let decoded = data_encoding::BASE32_NOPAD
        .decode(value.to_ascii_uppercase().as_bytes())
        .map_err(|_| unauthenticated())?;
    if decoded.len() != 32
        || data_encoding::BASE32_NOPAD
            .encode(&decoded)
            .to_ascii_lowercase()
            != value
    {
        return Err(unauthenticated());
    }
    Ok(TokenHash(Sha256::digest(secret.as_bytes()).into()))
}

pub struct ResponseCipher(XChaCha20Poly1305);

impl ResponseCipher {
    pub fn new(key: &[u8; 32]) -> Self {
        Self(XChaCha20Poly1305::new(key.into()))
    }

    pub fn encrypt(
        &self,
        principal: &Principal,
        operation: &OperationKey,
        status: u16,
        value: &Value,
    ) -> Result<String, ApiError> {
        let mut nonce = [0; 24];
        getrandom::fill(&mut nonce).map_err(|_| internal())?;
        let bytes = serde_json::to_vec(value).map_err(|_| internal())?;
        let aad = associated_data(principal, operation, status)?;
        let encrypted = self
            .0
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &bytes,
                    aad: &aad,
                },
            )
            .map_err(|_| internal())?;
        let mut envelope = Vec::with_capacity(1 + nonce.len() + encrypted.len());
        envelope.push(1);
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(&encrypted);
        Ok(STANDARD.encode(envelope))
    }

    pub fn decrypt(
        &self,
        principal: &Principal,
        operation: &OperationKey,
        status: u16,
        envelope: &str,
    ) -> Result<Value, ApiError> {
        let envelope = STANDARD.decode(envelope).map_err(|_| internal())?;
        if envelope.len() < 41 || envelope[0] != 1 {
            return Err(internal());
        }
        let aad = associated_data(principal, operation, status)?;
        let bytes = self
            .0
            .decrypt(
                &XNonce::try_from(&envelope[1..25]).map_err(|_| internal())?,
                Payload {
                    msg: &envelope[25..],
                    aad: &aad,
                },
            )
            .map_err(|_| internal())?;
        serde_json::from_slice(&bytes).map_err(|_| internal())
    }
}

fn associated_data(
    principal: &Principal,
    operation: &OperationKey,
    status: u16,
) -> Result<Vec<u8>, ApiError> {
    serde_json::to_vec(&(
        "cambium-idempotency-v1",
        &principal.tenant_id,
        &principal.actor.identity_id,
        &operation.key,
        operation.fingerprint,
        status,
    ))
    .map_err(|_| internal())
}

fn internal() -> ApiError {
    ApiError::new(ErrorCode::Internal)
}
fn unauthenticated() -> ApiError {
    ApiError::new(ErrorCode::Unauthenticated)
}
