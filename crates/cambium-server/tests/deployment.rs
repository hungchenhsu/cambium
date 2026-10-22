use std::{path::Path, process::Command};

use cambium_protocol::ErrorCode;
use cambium_server::{
    deployment::{self, Config},
    secrets::token_hash,
    service::{Clock, SystemClock},
};
use cambium_store::{SqliteStore, Store, TokenSettings};
use serde_json::Value;

fn cli(path: &Path, arguments: &[&str], succeeds: bool) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_cambium"))
        .arg("--data-dir")
        .arg(path)
        .arg("--json")
        .args(arguments)
        .env("LANG", "zh_TW.UTF-8")
        .output()
        .unwrap();
    assert_eq!(
        output.status.success(),
        succeeds,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(if succeeds {
        &output.stdout
    } else {
        &output.stderr
    })
    .unwrap()
}

#[tokio::test]
async fn host_commands_preserve_keys_and_revoke_without_network_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("private");
    assert_eq!(cli(&path, &["init"], true)["initialized"], true);
    let key = deployment::response_key(&path).unwrap();
    assert_eq!(
        Config::load(&path)
            .unwrap()
            .verifying_keys(&path)
            .unwrap()
            .len(),
        1
    );
    cli(&path, &["init"], false);
    assert_eq!(deployment::response_key(&path).unwrap(), key);
    let first = cli(
        &path,
        &[
            "admin",
            "bootstrap",
            "--tenant-slug",
            "demo",
            "--human-name",
            "Operator",
        ],
        true,
    );
    assert_eq!(first["scope"], "tenant:admin");
    assert!(first["expires_at"].as_str().unwrap().ends_with('Z'));
    let tenant = first["tenant_id"].as_str().unwrap();
    let identity = first["identity_id"].as_str().unwrap();
    let token_id = first["token_id"].as_str().unwrap();
    let second = cli(
        &path,
        &[
            "admin",
            "issue-human-token",
            "--tenant-id",
            tenant,
            "--identity-id",
            identity,
        ],
        true,
    );
    let store = SqliteStore::open(path.join("metadata.sqlite"), TokenSettings::default())
        .await
        .unwrap();
    let hash = token_hash(first["token"].as_str().unwrap()).unwrap();
    store
        .deployment_authenticate(hash, SystemClock.now_ms())
        .await
        .unwrap();
    cli(
        &path,
        &[
            "admin",
            "revoke-token",
            "--tenant-id",
            tenant,
            "--token-id",
            token_id,
        ],
        true,
    );
    assert_eq!(
        store
            .deployment_authenticate(hash, SystemClock.now_ms())
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Unauthenticated
    );
    let hash = token_hash(second["token"].as_str().unwrap()).unwrap();
    store
        .deployment_authenticate(hash, SystemClock.now_ms())
        .await
        .unwrap();
    cli(
        &path,
        &[
            "admin",
            "disable-identity",
            "--tenant-id",
            tenant,
            "--identity-id",
            identity,
        ],
        true,
    );
    assert_eq!(
        store
            .deployment_authenticate(hash, SystemClock.now_ms())
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Unauthenticated
    );
    let first_lock = deployment::acquire_process_lock(&path).unwrap();
    assert!(deployment::acquire_process_lock(&path).is_err());
    drop(first_lock);
    drop(deployment::acquire_process_lock(&path).unwrap());
    std::fs::remove_file(path.join("responses.key")).unwrap();
    assert!(deployment::response_key(&path).is_err());
    // Startup must not silently replace a lost key or reset active operations.
    cli(&path, &["serve"], false);
    assert!(!path.join("responses.key").exists());
    store.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn deployment_rejects_public_permissions_and_symlinked_secrets() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("private");
    deployment::initialize(&path, &Config::default())
        .await
        .unwrap();
    let key_path = path.join("responses.key");
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(deployment::response_key(&path).is_err());
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let original = path.join("original.key");
    std::fs::rename(&key_path, &original).unwrap();
    symlink(&original, &key_path).unwrap();
    assert!(deployment::response_key(&path).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Config::load(&path).is_err());
}
