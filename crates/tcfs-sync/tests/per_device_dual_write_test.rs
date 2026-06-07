//! Integration tests for Phase-1 DUAL-WRITE per-device file-key wrapping
//! (TIN-1417, B3). Exercises the real engine write/read paths to prove:
//!
//! - Default mode (`per_device_wrap_strict = false`, i.e.
//!   `EncryptionContext::strict_device_wrap == false`) with non-empty
//!   `device_recipients` emits BOTH the master-wrapped `encrypted_file_key`
//!   (rollback fallback) AND per-device `wrapped_file_keys`.
//! - A dual-written manifest is readable by a per-device reader (via the
//!   `wrapped_file_keys` switch) AND by a master-key-only reader (via the
//!   `encrypted_file_key` fallback — the old-binary / master-key path that
//!   never consults `wrapped_file_keys`).
//! - STRICT mode (`strict_device_wrap == true`) keeps the original clean-cut:
//!   only `wrapped_file_keys`, no master rollback.
//! - The default-off path (no `device_recipients`) is byte-identical to legacy:
//!   only `encrypted_file_key`, no `wrapped_file_keys`.

#![cfg(feature = "crypto")]

use opendal::Operator;
use secrecy::ExposeSecret;
use tempfile::TempDir;

use tcfs_crypto::AgeFileKeyRecipient;
use tcfs_sync::engine::{DeviceUnwrapIdentity, EncryptionContext};
use tcfs_sync::manifest::SyncManifest;

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

/// Per-device read context: master key present (for the fallback path) plus this
/// device's age identity.
fn read_ctx(id: &DeviceUnwrapIdentity) -> EncryptionContext {
    EncryptionContext::new(master()).with_device_wrapping(Vec::new(), Some(id.clone()))
}

/// Master-only read context: no device identity at all. This is the legacy /
/// old-binary / master-key-direct reader. It can only succeed on a manifest
/// whose `wrapped_file_keys` is empty (it has no age identity to unwrap them).
fn master_only_read_ctx() -> EncryptionContext {
    EncryptionContext::new(master())
}

async fn read_manifest(op: &Operator, remote_path: &str) -> SyncManifest {
    let bytes = op.read(remote_path).await.unwrap();
    SyncManifest::from_bytes(&bytes.to_bytes()).unwrap()
}

#[tokio::test]
async fn dual_write_emits_both_fields_and_is_readable_by_both_readers() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/dual-write";
    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    let (rec_a, id_a) = device("device-a");
    let (rec_b, id_b) = device("device-b");

    // Default: per-device wrapping ON (recipients present), strict OFF => DUAL-WRITE.
    let write_ctx = EncryptionContext::new(master())
        .with_device_wrapping(vec![rec_a, rec_b], Some(id_a.clone()));
    assert!(
        !write_ctx.strict_device_wrap,
        "default EncryptionContext must be non-strict (dual-write)"
    );

    let content = b"dual-written payload: readable by master AND per-device readers";
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

    // Manifest carries BOTH the master wrap (rollback fallback) and one
    // per-device wrap per recipient.
    let manifest = read_manifest(&op, &up.remote_path).await;
    assert_eq!(
        manifest.version, 2,
        "dual-write stays at manifest version 2"
    );
    assert_eq!(
        manifest.wrapped_file_keys.len(),
        2,
        "one per-device wrap per recipient"
    );
    assert!(
        manifest.encrypted_file_key.is_some(),
        "dual-write MUST keep the master-wrapped key as a rollback fallback"
    );
    // The master wrap must actually unwrap to a valid file key under the master.
    let wrapped_master = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        manifest.encrypted_file_key.as_ref().unwrap(),
    )
    .expect("master wrap is valid base64");
    tcfs_crypto::unwrap_key(&master(), &wrapped_master)
        .expect("dual-write master wrap must unwrap under the master key");

    // (1) Per-device readers (in the recipient set) hydrate exact bytes via the
    // wrapped_file_keys switch.
    for id in [&id_a, &id_b] {
        let dst = tmp.path().join(format!("pd-{}.txt", id.device_id));
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
        .expect("per-device reader download should succeed");
        assert_eq!(std::fs::read(&dst).unwrap(), content);
    }

    // (2) A master-key-only reader (old binary / master-key-direct path) hydrates
    // exact bytes via the `encrypted_file_key` fallback. That path never consults
    // `wrapped_file_keys`, so we present the manifest as such a reader sees it:
    // wrapped_file_keys stripped, encrypted_file_key intact. This proves the
    // dual-write rollback field is a complete, master-readable copy.
    let mut master_view = manifest.clone();
    master_view.wrapped_file_keys.clear();
    let master_remote = format!("{}/master-view-manifest.bin", prefix);
    op.write(&master_remote, master_view.to_bytes().unwrap())
        .await
        .unwrap();

    let dst_master = tmp.path().join("master-only.txt");
    tcfs_sync::engine::download_file_with_device(
        &op,
        &master_remote,
        &dst_master,
        prefix,
        None,
        "master-only-reader",
        None,
        Some(&master_only_read_ctx()),
    )
    .await
    .expect("master-only reader must hydrate the dual-write rollback fallback");
    assert_eq!(std::fs::read(&dst_master).unwrap(), content);
}

#[tokio::test]
async fn strict_mode_omits_master_wrap() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/strict";
    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    let (rec_a, id_a) = device("device-a");

    // CONTRACT: strict ON => clean-cut, only wrapped_file_keys.
    let write_ctx = EncryptionContext::new(master())
        .with_device_wrapping(vec![rec_a], Some(id_a.clone()))
        .with_strict_device_wrap(true);

    let content = b"strict clean-cut payload";
    let src = tmp.path().join("strict.txt");
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

    let manifest = read_manifest(&op, &up.remote_path).await;
    assert_eq!(manifest.wrapped_file_keys.len(), 1, "one per-device wrap");
    assert!(
        manifest.encrypted_file_key.is_none(),
        "strict mode must OMIT the master-wrapped key (clean-cut)"
    );

    // The recipient can still hydrate exact bytes.
    let dst = tmp.path().join("out.txt");
    tcfs_sync::engine::download_file_with_device(
        &op,
        &up.remote_path,
        &dst,
        prefix,
        None,
        &id_a.device_id,
        None,
        Some(&read_ctx(&id_a)),
    )
    .await
    .expect("recipient download should succeed");
    assert_eq!(std::fs::read(&dst).unwrap(), content);
}

#[tokio::test]
async fn default_off_path_is_legacy_master_only() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/default-off";
    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    // per_device_wrapping OFF == no device_recipients. Legacy shared-master.
    let write_ctx = EncryptionContext::new(master());
    assert!(
        write_ctx.device_recipients.is_empty(),
        "default-off path has no recipients"
    );

    let content = b"legacy shared-master payload (default-off, unchanged)";
    let src = tmp.path().join("legacy.txt");
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

    let manifest = read_manifest(&op, &up.remote_path).await;
    assert!(
        manifest.encrypted_file_key.is_some(),
        "default-off keeps the legacy master wrap"
    );
    assert!(
        manifest.wrapped_file_keys.is_empty(),
        "default-off must NOT emit any per-device wraps (byte-identical legacy)"
    );

    // Master-only reader hydrates exact bytes — legacy path unchanged.
    let dst = tmp.path().join("out.txt");
    tcfs_sync::engine::download_file_with_device(
        &op,
        &up.remote_path,
        &dst,
        prefix,
        None,
        "device-a",
        None,
        Some(&master_only_read_ctx()),
    )
    .await
    .expect("master-only reader download should succeed");
    assert_eq!(std::fs::read(&dst).unwrap(), content);
}
