//! Integration tests for per-device file-key wrapping (TIN-1417).
//!
//! Exercises the real engine write/read paths to prove:
//! - per-device recipients produce `wrapped_file_keys` and OMIT the
//!   master-wrapped `encrypted_file_key` (clean-cut), and every recipient can
//!   hydrate exact bytes;
//! - a revoked device (absent from the recipient set on a new write) cannot
//!   decrypt content written after revocation, while an active device can.

#![cfg(feature = "crypto")]

use opendal::Operator;
use secrecy::ExposeSecret;
use tempfile::TempDir;

use tcfs_crypto::AgeFileKeyRecipient;
use tcfs_sync::engine::{DeviceUnwrapIdentity, EncryptionContext, WrapMode};

fn memory_operator() -> Operator {
    Operator::new(opendal::services::Memory::default())
        .expect("memory operator")
        .finish()
}

fn master() -> tcfs_crypto::MasterKey {
    tcfs_crypto::MasterKey::from_bytes([7u8; 32])
}

/// Freshly generate an age device, returning its wrap recipient and unwrap identity.
fn device(id: &str) -> (AgeFileKeyRecipient, DeviceUnwrapIdentity) {
    let key = age::x25519::Identity::generate();
    let recipient = AgeFileKeyRecipient {
        device_id: id.to_string(),
        recipient: key.to_public().to_string(),
    };
    let identity = DeviceUnwrapIdentity {
        device_id: id.to_string(),
        secret: key.to_string().expose_secret().to_string(),
    };
    (recipient, identity)
}

fn read_ctx(id: &DeviceUnwrapIdentity) -> EncryptionContext {
    EncryptionContext::new(master()).with_device_wrapping(Vec::new(), Some(id.clone()))
}

#[tokio::test]
async fn per_device_wrap_roundtrip_and_manifest_shape() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/per-device";
    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    let (rec_a, id_a) = device("device-a");
    let (rec_b, id_b) = device("device-b");

    let write_ctx = EncryptionContext::new(master())
        .with_device_wrapping(vec![rec_a, rec_b], Some(id_a.clone()));

    let content = b"per-device wrapped payload that should round-trip exactly";
    let src = tmp.path().join("doc.txt");
    std::fs::write(&src, content).unwrap();

    let up = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        None,
        Some(&write_ctx),
    )
    .await
    .expect("upload should succeed");

    // Manifest carries one wrap per recipient and OMITS the master-wrapped key.
    let manifest_bytes = op.read(&up.remote_path).await.unwrap();
    let manifest =
        tcfs_sync::manifest::SyncManifest::from_bytes(&manifest_bytes.to_bytes()).unwrap();
    assert_eq!(
        manifest.wrapped_file_keys.len(),
        2,
        "one wrap per recipient"
    );
    assert!(
        manifest.encrypted_file_key.is_none(),
        "per-device manifest must not carry the master-wrapped key (clean-cut)"
    );

    // Both recipients hydrate exact bytes.
    for id in [&id_a, &id_b] {
        let dst = tmp.path().join(format!("out-{}.txt", id.device_id));
        tcfs_sync::engine::download_file_with_device(
            &op,
            &up.remote_path,
            &dst,
            prefix,
            None,
            &id.device_id,
            None,
            Some(&read_ctx(id)),
        )
        .await
        .expect("recipient download should succeed");
        assert_eq!(std::fs::read(&dst).unwrap(), content);
    }
}

#[tokio::test]
async fn revoked_device_cannot_decrypt_new_content() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/revocation";
    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    let (rec_a, id_a) = device("device-a");
    let (_rec_b, id_b) = device("device-b");

    // New content is written for the active set [A] only — B has been revoked.
    let write_ctx =
        EncryptionContext::new(master()).with_device_wrapping(vec![rec_a], Some(id_a.clone()));

    let content = b"content written after device-b was revoked";
    let src = tmp.path().join("after-revoke.txt");
    std::fs::write(&src, content).unwrap();

    let up = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        None,
        Some(&write_ctx),
    )
    .await
    .expect("upload should succeed");

    // The revoked device B cannot decrypt the new content.
    let dst_b = tmp.path().join("b.txt");
    let revoked = tcfs_sync::engine::download_file_with_device(
        &op,
        &up.remote_path,
        &dst_b,
        prefix,
        None,
        "device-b",
        None,
        Some(&read_ctx(&id_b)),
    )
    .await;
    assert!(
        revoked.is_err(),
        "revoked device must not decrypt content written after revocation"
    );

    // The still-active device A can.
    let dst_a = tmp.path().join("a.txt");
    tcfs_sync::engine::download_file_with_device(
        &op,
        &up.remote_path,
        &dst_a,
        prefix,
        None,
        "device-a",
        None,
        Some(&read_ctx(&id_a)),
    )
    .await
    .expect("active device download should succeed");
    assert_eq!(std::fs::read(&dst_a).unwrap(), content);
}

// ── TIN-1417: tri-state wrap_mode write-path shapes + v3 fail-closed ────────

async fn upload_with_ctx(
    op: &Operator,
    tmp: &TempDir,
    prefix: &str,
    ctx: &EncryptionContext,
    content: &[u8],
) -> tcfs_sync::manifest::SyncManifest {
    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();
    let src = tmp.path().join("doc.txt");
    std::fs::write(&src, content).unwrap();
    let up = tcfs_sync::engine::upload_file_with_device(
        op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        None,
        Some(ctx),
    )
    .await
    .expect("upload should succeed");
    let bytes = op.read(&up.remote_path).await.unwrap().to_bytes();
    tcfs_sync::manifest::SyncManifest::from_bytes(&bytes).unwrap()
}

/// Master mode (DEFAULT): master-wrapped key only, no per-device wraps, v2.
#[tokio::test]
async fn wrap_mode_master_emits_encrypted_file_key_only_v2() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let (rec_a, id_a) = device("device-a");
    // Even with recipients/identity attached, Master must emit master-only.
    let ctx =
        EncryptionContext::new(master()).with_wrap_mode(WrapMode::Master, vec![rec_a], Some(id_a));
    let manifest = upload_with_ctx(&op, &tmp, "test/master", &ctx, b"master payload").await;
    assert_eq!(manifest.version, 2, "Master stays manifest v2");
    assert!(
        manifest.encrypted_file_key.is_some(),
        "Master must emit the master-wrapped key"
    );
    assert!(
        manifest.wrapped_file_keys.is_empty(),
        "Master must NOT emit per-device wraps"
    );
}

/// Dual mode (EXPAND): BOTH master wrap and per-device wraps, stays v2.
#[tokio::test]
async fn wrap_mode_dual_emits_both_fields_v2() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let (rec_a, id_a) = device("device-a");
    let (rec_b, _id_b) = device("device-b");
    let ctx = EncryptionContext::new(master()).with_wrap_mode(
        WrapMode::Dual,
        vec![rec_a, rec_b],
        Some(id_a),
    );
    let manifest = upload_with_ctx(&op, &tmp, "test/dual", &ctx, b"dual payload").await;
    assert_eq!(manifest.version, 2, "Dual is back-compatible: stays v2");
    assert!(
        manifest.encrypted_file_key.is_some(),
        "Dual must emit the master wrap (rollback + master readers)"
    );
    assert_eq!(
        manifest.wrapped_file_keys.len(),
        2,
        "Dual must also emit one per-device wrap per recipient"
    );
}

/// PerDevice mode (CONTRACT): per-device wraps only, master wrap dropped, v3.
#[tokio::test]
async fn wrap_mode_per_device_emits_wrapped_only_v3() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let (rec_a, id_a) = device("device-a");
    let ctx = EncryptionContext::new(master()).with_wrap_mode(
        WrapMode::PerDevice,
        vec![rec_a],
        Some(id_a.clone()),
    );
    let manifest = upload_with_ctx(&op, &tmp, "test/perdev", &ctx, b"per-device payload").await;
    assert_eq!(manifest.version, 3, "PerDevice bumps the manifest to v3");
    assert!(
        manifest.encrypted_file_key.is_none(),
        "PerDevice must DROP the master wrap (true revocation)"
    );
    assert!(
        !manifest.wrapped_file_keys.is_empty(),
        "PerDevice must emit per-device wraps"
    );
}

/// Dual/PerDevice with no recipients must FAIL CLOSED rather than silently
/// degrading to master-only / writing an unreadable manifest.
#[tokio::test]
async fn dual_and_per_device_without_recipients_fail_closed() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();
    let src = tmp.path().join("doc.txt");
    std::fs::write(&src, b"x").unwrap();

    for mode in [WrapMode::Dual, WrapMode::PerDevice] {
        let ctx = EncryptionContext::new(master()).with_wrap_mode(mode, Vec::new(), None);
        let res = tcfs_sync::engine::upload_file_with_device(
            &op,
            &src,
            "test/failclosed",
            &mut state,
            None,
            "device-a",
            None,
            Some(&ctx),
        )
        .await;
        assert!(
            res.is_err(),
            "wrap_mode={mode:?} with no recipients must fail closed"
        );
    }
}

/// A v3 (per-device) manifest must be REJECTED fail-closed by a master-only read
/// context (a binary/context without a per-device identity).
#[tokio::test]
async fn v3_manifest_rejected_fail_closed_by_master_only_read() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/v3-failclosed";
    let (rec_a, id_a) = device("device-a");
    let write_ctx = EncryptionContext::new(master()).with_wrap_mode(
        WrapMode::PerDevice,
        vec![rec_a],
        Some(id_a),
    );
    let manifest = upload_with_ctx(&op, &tmp, prefix, &write_ctx, b"v3 payload").await;
    assert_eq!(manifest.version, 3);

    // Reconstruct the remote manifest path the same way upload did.
    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state2.db")).unwrap();
    let src = tmp.path().join("doc.txt");
    let up = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        None,
        Some(&write_ctx),
    )
    .await
    .unwrap();

    // Master-only context: no device identity attached -> must fail closed.
    let master_only = EncryptionContext::new(master());
    let dst = tmp.path().join("out.txt");
    let res = tcfs_sync::engine::download_file_with_device(
        &op,
        &up.remote_path,
        &dst,
        prefix,
        None,
        "device-a",
        None,
        Some(&master_only),
    )
    .await;
    assert!(
        res.is_err(),
        "a master-only context must reject a v3 per-device manifest (fail-closed)"
    );
    assert!(
        !dst.exists(),
        "fail-closed read must not materialize a file"
    );
}
