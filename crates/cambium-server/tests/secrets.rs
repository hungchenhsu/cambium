use cambium_protocol::{Actor, IdentityId, IdentityKind, Principal, Scope, TenantId, TokenId};
use cambium_server::secrets::{ResponseCipher, new_token, token_hash};
use cambium_store::operations::OperationKey;
use serde_json::json;

#[test]
fn response_encryption_authenticates_the_identity_key_fingerprint_and_status() {
    let cipher = ResponseCipher::new(&[42; 32]);
    let principal = Principal {
        tenant_id: TenantId::new(),
        token_id: TokenId::new(),
        actor: Actor {
            identity_id: IdentityId::new(),
            kind: IdentityKind::Human,
        },
        scope: Scope::TenantAdmin,
        repositories: None,
        expires_at_ms: i64::MAX,
    };
    let key = OperationKey {
        key: "mint".into(),
        fingerprint: [8; 32],
    };
    let token = new_token().unwrap();
    assert_eq!(token.len(), 56);
    assert!(token_hash(&token).is_ok());
    assert!(!format!("{:?}", token_hash(&token).unwrap()).contains(&token));
    let body = json!({"token": token});
    let envelope = cipher.encrypt(&principal, &key, 200, &body).unwrap();
    assert!(!envelope.contains(&token));
    assert_ne!(
        envelope,
        cipher.encrypt(&principal, &key, 200, &body).unwrap()
    );
    assert_eq!(
        cipher.decrypt(&principal, &key, 200, &envelope).unwrap(),
        body
    );
    let mut other = principal.clone();
    other.tenant_id = TenantId::new();
    assert!(cipher.decrypt(&other, &key, 200, &envelope).is_err());
    other = principal.clone();
    other.actor.identity_id = IdentityId::new();
    assert!(cipher.decrypt(&other, &key, 200, &envelope).is_err());
    for changed in [
        OperationKey {
            key: "other".into(),
            ..key.clone()
        },
        OperationKey {
            fingerprint: [9; 32],
            ..key.clone()
        },
    ] {
        assert!(
            cipher
                .decrypt(&principal, &changed, 200, &envelope)
                .is_err()
        );
    }
    assert!(cipher.decrypt(&principal, &key, 201, &envelope).is_err());
    assert!(
        ResponseCipher::new(&[43; 32])
            .decrypt(&principal, &key, 200, &envelope)
            .is_err()
    );
    for invalid in [
        "",
        "cmb_short",
        &token.to_ascii_uppercase(),
        &format!("{token}="),
    ] {
        assert!(token_hash(invalid).is_err());
    }
}
