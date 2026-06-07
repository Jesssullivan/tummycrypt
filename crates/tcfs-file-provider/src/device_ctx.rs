//! Device-aware encryption-context wiring for the FileProvider backends
//! (TIN-1417, Track B / B1).
//!
//! This is an **FP-local replica** of `tcfsd`'s
//! `build_encryption_context` (`crates/tcfsd/src/grpc.rs`). The daemon and CLI
//! both wire per-device unwrap identity onto the `EncryptionContext`; only the
//! FileProvider direct-read path historically built a master-only context
//! (`EncryptionContext::new`) and could therefore never read a per-device
//! (`wrapped_file_keys`-only) manifest. This module closes that gap.
//!
//! FOLLOW-UP (dedupe): the daemon, CLI, and this module now carry three copies
//! of the same registry/secret-loading logic. The clean fix is to lift a single
//! `build_encryption_context` into a shared crate (e.g. `tcfs-sync`). That was
//! deliberately deferred here because `tcfs-sync` does not currently depend on
//! `tcfs-secrets`, and adding that dependency edge to a core crate consumed by
//! most of the workspace is a broader change than this security fix should
//! carry. See PR body for details.
//!
//! BEHAVIOR CONTRACT (must stay true):
//! - When `per_device_wrapping` is absent/false in the config (the default),
//!   `build_encryption_context` returns exactly `EncryptionContext::new(mk)` —
//!   byte-identical to the prior master-only behavior. Master-only manifests
//!   read unchanged.
//! - When a per-device manifest is encountered without an available device
//!   identity, the engine read switch fails CLOSED (clear error); we never
//!   silently master-fall-back, and we never copy raw ciphertext to disk.

/// Fail CLOSED when a manifest that this backend cannot decrypt would otherwise
/// be copied to disk as raw ciphertext (silent corruption).
///
/// The `direct`/`uniffi` backends only implement master-key unwrapping. A
/// per-device manifest carries `wrapped_file_keys` and OMITS the master-wrapped
/// `encrypted_file_key`, so those backends would fall through to "no file key"
/// and write encrypted chunk bytes verbatim. This guard turns that into a loud,
/// explicit error instead.
#[cfg(any(feature = "direct", feature = "grpc"))]
pub(crate) fn ensure_master_decryptable(
    manifest: &tcfs_sync::manifest::SyncManifest,
) -> anyhow::Result<()> {
    if !manifest.wrapped_file_keys.is_empty() {
        anyhow::bail!(
            "manifest is per-device encrypted (wrapped_file_keys present) but this \
             FileProvider backend only supports master-key unwrapping; refusing to \
             materialize raw ciphertext. Use the gRPC backend with a device identity \
             configured to read per-device content."
        );
    }
    Ok(())
}

/// Build a DEVICE-AWARE `EncryptionContext` for the FileProvider read path,
/// mirroring `tcfsd`'s `build_encryption_context`.
///
/// Default-off invariant: returns `EncryptionContext::new(master_key)` verbatim
/// unless the config explicitly sets `per_device_wrapping: true`. In that case
/// it loads the device registry + this device's age secret and attaches them
/// via `.with_device_wrapping`, exactly like the daemon.
///
/// Like the daemon, this falls back to the master-only context (and logs why)
/// only when per-device wrapping is enabled but the registry/recipients/secret
/// are unavailable — it never produces a context that this device cannot read
/// back. The engine's read switch independently fails CLOSED if it then meets a
/// per-device manifest with no identity attached.
#[cfg(feature = "grpc")]
pub(crate) fn build_encryption_context(
    config: &serde_json::Value,
    device_id: &str,
    master_key: &tcfs_crypto::MasterKey,
) -> tcfs_sync::engine::EncryptionContext {
    use tcfs_sync::engine::{DeviceUnwrapIdentity, EncryptionContext};

    let base = EncryptionContext::new(master_key.clone());

    // Default-off: byte-identical master-only behavior unless explicitly enabled.
    if !config
        .get("per_device_wrapping")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return base;
    }

    let registry_path = config
        .get("device_registry_path")
        .and_then(serde_json::Value::as_str)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(tcfs_secrets::device::default_registry_path);

    let registry = match tcfs_secrets::device::DeviceRegistry::load(&registry_path) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                "per-device wrapping: registry load failed ({e}); using master wrap"
            );
            return base;
        }
    };

    let recipients: Vec<tcfs_crypto::AgeFileKeyRecipient> = registry
        .active_devices()
        .filter(|d| tcfs_secrets::device::is_real_age_public_key(&d.public_key))
        .map(|d| tcfs_crypto::AgeFileKeyRecipient {
            device_id: d.device_id.clone(),
            recipient: d.public_key.clone(),
        })
        .collect();
    if recipients.is_empty() {
        tracing::warn!(
            "per-device wrapping enabled but no active age recipients; using master wrap"
        );
        return base;
    }

    let secret_path = tcfs_secrets::device::device_secret_key_path(&registry_path, device_id);
    let identity = match std::fs::read_to_string(&secret_path) {
        Ok(s) => DeviceUnwrapIdentity {
            device_id: device_id.to_string(),
            secret: s.trim().to_string(),
        },
        Err(e) => {
            tracing::warn!(
                "per-device wrapping: local device secret unreadable ({e}); using master wrap"
            );
            return base;
        }
    };

    base.with_device_wrapping(recipients, Some(identity))
}

#[cfg(all(test, feature = "grpc"))]
mod tests {
    use super::*;
    use tcfs_secrets::device::{DeviceIdentity, DeviceRegistry};

    fn master() -> tcfs_crypto::MasterKey {
        tcfs_crypto::MasterKey::from_bytes([7u8; 32])
    }

    /// Materialize a device registry + this device's age secret in `dir`,
    /// returning (device_id, public_key). Mirrors the on-disk layout the daemon
    /// and `build_encryption_context` expect: `<dir>/devices.json` plus
    /// `<dir>/device-<id>.age`.
    fn provision_device(dir: &std::path::Path, device_id: &str) -> String {
        let registry_path = dir.join("devices.json");
        let key = tcfs_secrets::device::generate_local_device_key();

        let mut registry = DeviceRegistry::default();
        registry.add(DeviceIdentity {
            name: device_id.to_string(),
            device_id: device_id.to_string(),
            public_key: key.public_key.clone(),
            signing_key_hash: String::new(),
            description: None,
            enrolled_at: 0,
            revoked: false,
            last_nats_seq: 0,
        });
        registry.save(&registry_path).expect("save registry");

        let secret_path =
            tcfs_secrets::device::device_secret_key_path(&registry_path, device_id);
        tcfs_secrets::device::save_device_secret_key(&secret_path, &key.secret_key, true)
            .expect("save device secret");

        key.public_key
    }

    /// Default config (per_device_wrapping absent) yields a master-only context:
    /// no recipients, no identity — byte-identical to `EncryptionContext::new`.
    #[test]
    fn default_config_is_master_only_byte_identical() {
        let config = serde_json::json!({});
        let ctx = build_encryption_context(&config, "device-a", &master());
        assert!(
            ctx.device_recipients.is_empty(),
            "default config must not attach per-device recipients"
        );
        assert!(
            ctx.device_identity.is_none(),
            "default config must not attach a device identity"
        );
    }

    /// Explicit per_device_wrapping=false also stays master-only.
    #[test]
    fn explicit_disabled_is_master_only() {
        let config = serde_json::json!({ "per_device_wrapping": false });
        let ctx = build_encryption_context(&config, "device-a", &master());
        assert!(ctx.device_recipients.is_empty());
        assert!(ctx.device_identity.is_none());
    }

    /// When enabled with a real registry + local secret, the context carries this
    /// device's unwrap identity and the active-device recipient set.
    #[test]
    fn enabled_attaches_device_identity_and_recipients() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _pub = provision_device(tmp.path(), "device-a");

        let config = serde_json::json!({
            "per_device_wrapping": true,
            "device_registry_path": tmp.path().join("devices.json").to_str().unwrap(),
        });
        let ctx = build_encryption_context(&config, "device-a", &master());

        assert_eq!(
            ctx.device_recipients.len(),
            1,
            "should pick up the single active device recipient"
        );
        let identity = ctx
            .device_identity
            .as_ref()
            .expect("enabled config with secret must attach a device identity");
        assert_eq!(identity.device_id, "device-a");
        assert!(identity.secret.starts_with("AGE-SECRET-KEY-"));
    }

    /// Enabled but no local secret on disk -> falls back to master-only (never
    /// produces a context that cannot read back); the engine read switch then
    /// fails CLOSED on any per-device manifest it meets.
    #[test]
    fn enabled_without_local_secret_falls_back_to_master_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Registry exists with a recipient, but this device's secret is absent.
        let registry_path = tmp.path().join("devices.json");
        let key = tcfs_secrets::device::generate_local_device_key();
        let mut registry = DeviceRegistry::default();
        registry.add(DeviceIdentity {
            name: "device-a".to_string(),
            device_id: "device-a".to_string(),
            public_key: key.public_key,
            signing_key_hash: String::new(),
            description: None,
            enrolled_at: 0,
            revoked: false,
            last_nats_seq: 0,
        });
        registry.save(&registry_path).unwrap();

        let config = serde_json::json!({
            "per_device_wrapping": true,
            "device_registry_path": registry_path.to_str().unwrap(),
        });
        let ctx = build_encryption_context(&config, "device-a", &master());
        assert!(
            ctx.device_identity.is_none(),
            "missing local secret must not yield a half-wired identity"
        );
    }

    /// End-to-end through the real engine: a per-device (`wrapped_file_keys`)
    /// manifest is READABLE via the FP-built context with the device identity,
    /// fails CLOSED via a master-only context (no identity), and a master-only
    /// manifest still reads unchanged via the FP context.
    #[tokio::test]
    async fn per_device_manifest_roundtrips_via_fp_context_and_fails_closed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _pub = provision_device(tmp.path(), "device-a");
        let op = opendal::Operator::new(opendal::services::Memory::default())
            .unwrap()
            .finish();
        let prefix = "test/fp-per-device";
        let mut state =
            tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

        // Build the write context from the same registry the FP read context uses.
        let enabled_config = serde_json::json!({
            "per_device_wrapping": true,
            "device_registry_path": tmp.path().join("devices.json").to_str().unwrap(),
        });
        let write_ctx = build_encryption_context(&enabled_config, "device-a", &master());
        assert!(
            !write_ctx.device_recipients.is_empty(),
            "precondition: write context must have recipients"
        );

        let content = b"per-device payload read through the FileProvider context";
        let src = tmp.path().join("doc.txt");
        std::fs::write(&src, content).unwrap();

        let up = tcfs_sync::engine::upload_file_with_device(
            &op, &src, prefix, &mut state, None, "device-a", None, Some(&write_ctx),
        )
        .await
        .expect("per-device upload should succeed");

        // The manifest is genuinely per-device (no master-wrapped key).
        let manifest = tcfs_sync::manifest::SyncManifest::from_bytes(
            &op.read(&up.remote_path).await.unwrap().to_bytes(),
        )
        .unwrap();
        assert!(!manifest.wrapped_file_keys.is_empty());
        assert!(manifest.encrypted_file_key.is_none());

        // (1) Readable via the device-aware FP context.
        let read_ctx = build_encryption_context(&enabled_config, "device-a", &master());
        let dst_ok = tmp.path().join("ok.txt");
        tcfs_sync::engine::download_file_with_device(
            &op, &up.remote_path, &dst_ok, prefix, None, "device-a", None, Some(&read_ctx),
        )
        .await
        .expect("device-aware FP context should read per-device manifest");
        assert_eq!(std::fs::read(&dst_ok).unwrap(), content);

        // (2) Fails CLOSED with a master-only context (no identity attached) —
        //     this is the pre-fix FileProvider behavior; it must error, never
        //     silently fall back or corrupt.
        let master_only = build_encryption_context(&serde_json::json!({}), "device-a", &master());
        assert!(master_only.device_identity.is_none());
        let dst_closed = tmp.path().join("closed.txt");
        let res = tcfs_sync::engine::download_file_with_device(
            &op, &up.remote_path, &dst_closed, prefix, None, "device-a", None, Some(&master_only),
        )
        .await;
        assert!(
            res.is_err(),
            "master-only context must FAIL CLOSED on a per-device manifest"
        );
        assert!(
            !dst_closed.exists(),
            "fail-closed read must not materialize a corrupt file"
        );
    }

    /// A master-only manifest reads unchanged through the FP context built from a
    /// default (per_device_wrapping=false) config — byte-identical regression
    /// guard for the default-off path.
    #[tokio::test]
    async fn master_only_manifest_reads_unchanged_via_fp_context() {
        let tmp = tempfile::TempDir::new().unwrap();
        let op = opendal::Operator::new(opendal::services::Memory::default())
            .unwrap()
            .finish();
        let prefix = "test/fp-master-only";
        let mut state =
            tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

        // Legacy master-only write (no per-device recipients).
        let write_ctx = tcfs_sync::engine::EncryptionContext::new(master());
        let content = b"legacy master-only payload";
        let src = tmp.path().join("legacy.txt");
        std::fs::write(&src, content).unwrap();

        let up = tcfs_sync::engine::upload_file_with_device(
            &op, &src, prefix, &mut state, None, "device-a", None, Some(&write_ctx),
        )
        .await
        .expect("master-only upload should succeed");

        let manifest = tcfs_sync::manifest::SyncManifest::from_bytes(
            &op.read(&up.remote_path).await.unwrap().to_bytes(),
        )
        .unwrap();
        assert!(manifest.wrapped_file_keys.is_empty());
        assert!(manifest.encrypted_file_key.is_some());

        // Default config -> master-only FP context -> reads unchanged.
        let read_ctx = build_encryption_context(&serde_json::json!({}), "device-a", &master());
        let dst = tmp.path().join("out.txt");
        tcfs_sync::engine::download_file_with_device(
            &op, &up.remote_path, &dst, prefix, None, "device-a", None, Some(&read_ctx),
        )
        .await
        .expect("default FP context must read master-only manifest unchanged");
        assert_eq!(std::fs::read(&dst).unwrap(), content);
    }

    /// The fail-loud guard for the master-only backends rejects per-device
    /// manifests instead of copying raw ciphertext.
    #[test]
    fn ensure_master_decryptable_rejects_per_device_manifest() {
        let base = || tcfs_sync::manifest::SyncManifest {
            version: 2,
            file_hash: String::new(),
            file_size: 0,
            chunks: Vec::new(),
            vclock: tcfs_sync::conflict::VectorClock::default(),
            written_by: "device-a".to_string(),
            written_at: 0,
            rel_path: None,
            mode: None,
            encrypted_file_key: None,
            wrapped_file_keys: Vec::new(),
        };

        let mut per_device = base();
        per_device
            .wrapped_file_keys
            .push(tcfs_sync::manifest::WrappedFileKey {
                recipient_device_id: "device-a".to_string(),
                recipient: "age1example".to_string(),
                algorithm: "age-x25519-v1".to_string(),
                wrapped_key: "deadbeef".to_string(),
            });
        assert!(ensure_master_decryptable(&per_device).is_err());

        assert!(ensure_master_decryptable(&base()).is_ok());
    }
}
