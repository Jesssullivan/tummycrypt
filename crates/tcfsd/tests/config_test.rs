//! Config loading tests for tcfsd
//!
//! Tests the `load_config` path:
//! - Missing file → actionable setup error
//! - Valid TOML → parsed config
//! - Invalid TOML → error

use std::io::Write;
use tcfsd::config_loader::load_config;
use tempfile::NamedTempFile;

#[tokio::test]
async fn missing_config_returns_actionable_setup_error() {
    let path = std::path::Path::new("/tmp/tcfsd-test-nonexistent.toml");
    assert!(!path.exists());

    let err = load_config(path).await.unwrap_err().to_string();
    assert!(err.contains("tcfsd config not found"), "got: {err}");
    assert!(err.contains("tcfs init --config-out"), "got: {err}");
}

#[tokio::test]
async fn valid_toml_parses() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(
        f,
        r#"
[daemon]
socket = "/tmp/test-tcfsd.sock"

[storage]
endpoint = "http://localhost:8333"
bucket = "test-bucket"

[sync]
device_name = "test-device"

[crypto]
enabled = false

[secrets]

[fuse]

[sops]

[auth]
"#
    )
    .unwrap();

    let config = load_config(f.path())
        .await
        .expect("valid TOML should parse");
    assert_eq!(config.storage.bucket, "test-bucket");
    assert_eq!(
        config.daemon.socket,
        std::path::PathBuf::from("/tmp/test-tcfsd.sock")
    );
    assert_eq!(config.sync.device_name, Some("test-device".to_string()));
}

#[tokio::test]
async fn invalid_toml_returns_error() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "this is not [valid toml {{{{").unwrap();

    let result = load_config(f.path()).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("parsing config"), "got: {err}");
}

#[tokio::test]
async fn partial_config_uses_defaults_for_missing_sections() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(
        f,
        r#"
[storage]
bucket = "my-bucket"
"#
    )
    .unwrap();

    let config = load_config(f.path())
        .await
        .expect("partial config should parse");
    assert_eq!(config.storage.bucket, "my-bucket");
    // Other sections should have defaults
    assert!(!config.crypto.enabled);
}

#[tokio::test]
async fn crypto_enabled_config() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(
        f,
        r#"
[crypto]
enabled = true
kdf_salt = "a3f7b82e14d09c56deadbeef12345678"
"#
    )
    .unwrap();

    let config = load_config(f.path())
        .await
        .expect("crypto config should parse");
    assert!(config.crypto.enabled);
}
