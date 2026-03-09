//! Integration tests for the full auth flow:
//!   invite → enroll → challenge → verify → session → permission check

use tcfs_auth::enrollment::EnrollmentInvite;
use tcfs_auth::provider::{AuthProvider, AuthResponse, VerifyResult};
use tcfs_auth::session::{DevicePermissions, Session, SessionStore};
use tcfs_auth::totp::{TotpConfig, TotpProvider};

use totp_rs::{Algorithm, Secret, TOTP};

/// Full TOTP auth lifecycle: enroll → challenge → verify → session
#[tokio::test]
async fn test_totp_full_lifecycle() {
    let provider = TotpProvider::new(TotpConfig::default());
    let store = SessionStore::new();
    let device_id = "integration-device-1";

    // 1. Register (enroll) the device
    let reg = provider.register(device_id).await.unwrap();
    assert!(!reg.data.is_empty());

    let reg_json: serde_json::Value = serde_json::from_slice(&reg.data).unwrap();
    let secret = reg_json["secret"].as_str().unwrap();

    // 2. Request a challenge
    let challenge = provider.challenge(device_id).await.unwrap();
    assert!(!challenge.challenge_id.is_empty());
    assert!(challenge.prompt.contains("6-digit"));

    // 3. Generate valid TOTP code (simulating authenticator app)
    let totp = TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        Secret::Encoded(secret.to_string()).to_bytes().unwrap(),
        Some("TummyCrypt".to_string()),
        device_id.to_string(),
    )
    .unwrap();
    let code = totp.generate_current().unwrap();

    // 4. Verify the response
    let response = AuthResponse {
        challenge_id: challenge.challenge_id.clone(),
        data: code.as_bytes().to_vec(),
        device_id: device_id.to_string(),
    };
    let result = provider.verify(&response).await.unwrap();

    let session_token = match result {
        VerifyResult::Success {
            session_token,
            device_id: did,
        } => {
            assert_eq!(did, device_id);
            session_token
        }
        VerifyResult::Failure { reason } => panic!("expected success, got failure: {reason}"),
        VerifyResult::Expired => panic!("expected success, got expired"),
    };

    // 5. Create and store a session
    let session = Session::new(device_id, "integration-laptop", "totp")
        .with_expiry(24)
        .with_permissions(DevicePermissions::default());
    let token = session.token.clone();
    store.insert(session).await;

    // 6. Validate session exists and is usable
    let validated = store.validate(&token).await.unwrap();
    assert_eq!(validated.device_id, device_id);
    assert_eq!(validated.auth_method, "totp");
    assert!(validated.permissions.can_push);
    assert!(validated.permissions.can_pull);
    assert!(validated.permissions.can_mount);
    assert!(!validated.permissions.can_admin);

    // Session token from verify step should be different (UUID-based)
    assert!(!session_token.is_empty());
}

/// Enrollment invite → decode → validate → enroll device
#[tokio::test]
async fn test_invite_then_enroll() {
    let master_key = [99u8; 32];
    let provider = TotpProvider::new(TotpConfig::default());

    // 1. Admin creates invite
    let invite = EnrollmentInvite::new(
        "admin-device",
        &master_key,
        24,
        DevicePermissions::default(),
    );
    let encoded = invite.encode().unwrap();
    let deep_link = invite.to_deep_link().unwrap();
    assert!(deep_link.starts_with("tcfs://enroll?data="));

    // 2. New device decodes invite (simulating QR scan)
    let decoded = EnrollmentInvite::decode(&encoded).unwrap();
    assert!(decoded.is_valid(&master_key));
    assert!(!decoded.is_expired());

    // 3. New device enrolls with TOTP
    let new_device = "new-phone-1";
    let reg = provider.register(new_device).await.unwrap();

    let reg_json: serde_json::Value = serde_json::from_slice(&reg.data).unwrap();
    let secret = reg_json["secret"].as_str().unwrap();

    // 4. New device verifies enrollment by entering a code
    let totp = TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        Secret::Encoded(secret.to_string()).to_bytes().unwrap(),
        Some("TummyCrypt".to_string()),
        new_device.to_string(),
    )
    .unwrap();
    let code = totp.generate_current().unwrap();

    let response = AuthResponse {
        challenge_id: String::new(),
        data: code.as_bytes().to_vec(),
        device_id: new_device.to_string(),
    };
    let result = provider.verify(&response).await.unwrap();
    assert!(matches!(result, VerifyResult::Success { .. }));
}

/// Session-gated permission checks
#[tokio::test]
async fn test_session_gated_operations() {
    let store = SessionStore::new();

    // No session → not unlocked
    assert!(!store.has_active_session().await);

    // Create session with restricted permissions
    let session = Session::new("restricted-device", "tablet", "totp")
        .with_expiry(1)
        .with_permissions(DevicePermissions::read_only());
    let token = session.token.clone();
    store.insert(session).await;

    // Validate and check permissions
    let s = store.validate(&token).await.unwrap();
    assert!(s.permissions.can_pull, "read-only should allow pull");
    assert!(s.permissions.can_mount, "read-only should allow mount");
    assert!(!s.permissions.can_push, "read-only should deny push");
    assert!(!s.permissions.can_admin, "read-only should deny admin");
    assert!(store.has_active_session().await);

    // Revoke → no access
    store.revoke(&token).await;
    assert!(store.validate(&token).await.is_none());
    assert!(!store.has_active_session().await);
}

/// Prefix-based access control
#[tokio::test]
async fn test_prefix_access_control() {
    let perms = DevicePermissions {
        can_mount: true,
        can_push: true,
        can_pull: true,
        can_admin: false,
        allowed_prefixes: vec!["git/".to_string(), "docs/".to_string()],
    };

    assert!(perms.can_access_prefix("git/crush-dots"));
    assert!(perms.can_access_prefix("git/tummycrypt"));
    assert!(perms.can_access_prefix("docs/readme.md"));
    assert!(!perms.can_access_prefix("secrets/master.key"));
    assert!(!perms.can_access_prefix("config/tcfs.toml"));

    // Admin with no restrictions
    let admin = DevicePermissions::admin();
    assert!(admin.can_access_prefix("secrets/master.key"));
}

/// Multiple devices with independent sessions
#[tokio::test]
async fn test_multi_device_sessions() {
    let store = SessionStore::new();
    let provider = TotpProvider::new(TotpConfig::default());

    // Enroll two devices
    let devices = ["laptop-1", "phone-1"];
    let mut tokens = Vec::new();

    for device in &devices {
        let reg = provider.register(device).await.unwrap();
        let reg_json: serde_json::Value = serde_json::from_slice(&reg.data).unwrap();
        let secret = reg_json["secret"].as_str().unwrap();

        let totp = TOTP::new(
            Algorithm::SHA1,
            6,
            1,
            30,
            Secret::Encoded(secret.to_string()).to_bytes().unwrap(),
            Some("TummyCrypt".to_string()),
            device.to_string(),
        )
        .unwrap();
        let code = totp.generate_current().unwrap();

        let response = AuthResponse {
            challenge_id: String::new(),
            data: code.as_bytes().to_vec(),
            device_id: device.to_string(),
        };
        let result = provider.verify(&response).await.unwrap();
        assert!(matches!(result, VerifyResult::Success { .. }));

        // Create session
        let session = Session::new(device, device, "totp").with_expiry(24);
        let token = session.token.clone();
        store.insert(session).await;
        tokens.push(token);
    }

    // Both sessions active
    assert_eq!(store.active_count().await, 2);

    // Revoke one device
    store.revoke_device("laptop-1").await;
    assert_eq!(store.active_count().await, 1);
    assert!(store.validate(&tokens[0]).await.is_none());
    assert!(store.validate(&tokens[1]).await.is_some());
}

/// Session replacement — re-auth replaces old session
#[tokio::test]
async fn test_reauth_replaces_session() {
    let store = SessionStore::new();

    let s1 = Session::new("device-1", "laptop", "totp").with_expiry(24);
    let t1 = s1.token.clone();
    store.insert(s1).await;

    let s2 = Session::new("device-1", "laptop", "webauthn").with_expiry(24);
    let t2 = s2.token.clone();
    store.insert(s2).await;

    // Old session revoked, new one active
    assert!(store.validate(&t1).await.is_none());
    let active = store.validate(&t2).await.unwrap();
    assert_eq!(active.auth_method, "webauthn");
    assert_eq!(store.active_count().await, 1);
}

/// Expired invite should be rejected
#[test]
fn test_expired_invite_rejected() {
    let master_key = [42u8; 32];
    // Create invite with 0 hours TTL (immediately expired)
    let invite =
        EnrollmentInvite::new("admin-device", &master_key, 0, DevicePermissions::default());

    assert!(invite.is_expired());
    assert!(!invite.is_valid(&master_key));
    assert!(invite.verify_signature(&master_key)); // Signature still valid, just expired
}

/// Wrong signing key should fail verification
#[test]
fn test_invite_wrong_key_rejected() {
    let real_key = [42u8; 32];
    let wrong_key = [0u8; 32];

    let invite = EnrollmentInvite::new("admin-device", &real_key, 24, DevicePermissions::default());

    assert!(invite.verify_signature(&real_key));
    assert!(!invite.verify_signature(&wrong_key));
    assert!(!invite.is_valid(&wrong_key));
}

/// TOTP credential persistence (save/load roundtrip)
#[tokio::test]
async fn test_totp_credential_persistence() {
    let provider = TotpProvider::new(TotpConfig::default());

    // Register a device
    provider.register("persist-device").await.unwrap();
    assert_eq!(provider.enrolled_count().await, 1);

    // Save to temp file
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("totp-creds.json");
    provider.save_to_file(&path).await.unwrap();

    // Load into a new provider
    let provider2 = TotpProvider::new(TotpConfig::default());
    assert_eq!(provider2.enrolled_count().await, 0);
    provider2.load_from_file(&path).await.unwrap();
    assert_eq!(provider2.enrolled_count().await, 1);

    // Should be able to challenge the loaded device
    let challenge = provider2.challenge("persist-device").await;
    assert!(challenge.is_ok());
}
