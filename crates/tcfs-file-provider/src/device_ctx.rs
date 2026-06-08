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
//! - When `wrap_mode` is absent or `master` in the config (the default),
//!   `build_encryption_context` returns exactly `EncryptionContext::new(mk)` —
//!   byte-identical to the prior master-only behavior. Master-only manifests
//!   read unchanged. (The legacy `per_device_wrapping` boolean is still accepted
//!   as an alias: `true` -> `dual`, `false`/absent -> `master`.)
//! - When a per-device manifest is encountered without an available device
//!   identity, the engine read switch fails CLOSED (clear error); we never
//!   silently master-fall-back, and we never copy raw ciphertext to disk.
//! - The `PerDevice` (CONTRACT) mode is gated behind the same roll-call probe as
//!   the daemon: it is downgraded to `Dual` unless every active device carries a
//!   real age recipient.

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

/// Resolve the requested `wrap_mode` from an FP init-config JSON blob (TIN-1417).
///
/// Canonical key is `wrap_mode` (`"master" | "dual" | "per_device"`). The legacy
/// boolean `per_device_wrapping` is still accepted as an alias (`true` -> Dual,
/// `false`/absent -> Master); a present `wrap_mode` wins. Mirrors the
/// `CryptoConfig` deserialize back-compat rule.
#[cfg(feature = "grpc")]
fn wrap_mode_from_config(config: &serde_json::Value) -> tcfs_core::config::WrapMode {
    use tcfs_core::config::WrapMode;
    if let Some(raw) = config.get("wrap_mode").and_then(serde_json::Value::as_str) {
        return match raw {
            "master" => WrapMode::Master,
            "dual" => WrapMode::Dual,
            "per_device" => WrapMode::PerDevice,
            other => {
                tracing::warn!("unknown wrap_mode {other:?} in FileProvider config; using master");
                WrapMode::Master
            }
        };
    }
    match config
        .get("per_device_wrapping")
        .and_then(serde_json::Value::as_bool)
    {
        Some(true) => WrapMode::Dual,
        Some(false) | None => WrapMode::Master,
    }
}

/// Resolve the active per-device recipient set, PREFERRING values inlined into
/// the config over an on-disk registry read (TIN-1417 Keychain inlining).
///
/// Under the macOS FileProvider sandbox the `.appex` cannot `fs`-read
/// `~/.config/tcfs` (where the device registry lives); it receives its config
/// from the shared Keychain instead. The Swift host therefore inlines the
/// active recipients into the Keychain config copy under `device_recipients`,
/// mirroring how it inlines `master_key_base64`. When that key is present we
/// prefer it over the filesystem; otherwise we fall back to loading
/// `devices.json` (the non-sandboxed path used by the daemon/CLI and by tests).
///
/// TRUST SCOPE (read this carefully — it is a security boundary):
/// The inlined recipients are RE-VALIDATED here for age-key WELL-FORMEDNESS
/// only — each `recipient` must parse as a real `age::x25519::Recipient` (via
/// `is_real_age_public_key`) or it is dropped from the wrap set. We do NOT, and
/// cannot, verify the AUTHENTICITY of the underlying registry: the registry the
/// Swift host reads is not signature-verified, so a forged/tampered registry
/// could still inline a well-formed-but-attacker-controlled recipient. Closing
/// that gap is the job of the Ed25519-signed device registry (B4, separate and
/// currently unmerged); it is explicitly OUT OF SCOPE for this inlining change,
/// which only mirrors the existing (forgeable) trust posture into the sandbox.
///
/// Returns `(recipients, all_capable)`. `all_capable` reflects whether every
/// active device carries a real age recipient. We RE-DERIVE it HERE on the Rust
/// side from our own re-validated recipient set rather than trusting the host's
/// `device_recipients_all_capable` boolean (the Swift `isRealAgePublicKey` is a
/// prefix/length heuristic, so a host over-count must never be allowed to drop
/// the master wrap and lock this device out — see the inlined branch below). For
/// the fs path it is likewise derived from the loaded registry's `roll_call()`.
/// It gates the PerDevice contract mode exactly as the daemon's roll-call gate
/// does.
#[cfg(feature = "grpc")]
fn resolve_recipients(
    config: &serde_json::Value,
    requested: tcfs_core::config::WrapMode,
) -> Option<(Vec<tcfs_crypto::AgeFileKeyRecipient>, bool)> {
    // Inlined-first: the Keychain-provided config copy carries the active
    // recipients so the sandboxed FileProvider never reads devices.json. These
    // are re-validated for age-key WELL-FORMEDNESS below; their registry
    // AUTHENTICITY is NOT verified here (out of scope — pending B4 signed
    // registry).
    if let Some(arr) = config
        .get("device_recipients")
        .and_then(serde_json::Value::as_array)
    {
        // Count every inlined entry the host *claims* is active so we can detect
        // a host over-count: any entry that fails our own age-recipient parse
        // means "not all active devices are per-device-capable" -> all_capable
        // MUST be false regardless of what the host asserted.
        let inlined_total = arr.len();
        let recipients: Vec<tcfs_crypto::AgeFileKeyRecipient> = arr
            .iter()
            .filter_map(|entry| {
                let device_id = entry.get("device_id")?.as_str()?.to_string();
                let recipient = entry.get("recipient")?.as_str()?.to_string();
                // Re-validate WELL-FORMEDNESS only (real age::x25519::Recipient
                // parse). A Swift false-positive cannot inject a malformed
                // recipient into the wrap set.
                if !tcfs_secrets::device::is_real_age_public_key(&recipient) {
                    return None;
                }
                Some(tcfs_crypto::AgeFileKeyRecipient {
                    device_id,
                    recipient,
                })
            })
            .collect();
        if recipients.is_empty() {
            tracing::warn!(
                "wrap_mode={requested:?}: inlined device_recipients present but empty/invalid; \
                 using master wrap"
            );
            return None;
        }
        // HARDENING (availability): RE-DERIVE all_capable in Rust from our own
        // re-validated recipient set — do NOT trust the host-provided
        // `device_recipients_all_capable` boolean. The Swift `isRealAgePublicKey`
        // is only a prefix/length heuristic; if it over-counts (asserts a
        // malformed key is real) and we trusted its boolean, Rust could enter
        // PerDevice, drop the master wrap, and lock out a device that actually
        // lacks a usable recipient. all_capable is true iff EVERY inlined entry
        // the host listed as active also parsed as a real age recipient here.
        let all_capable = recipients.len() == inlined_total;
        if !all_capable {
            tracing::warn!(
                "wrap_mode={requested:?}: {} of {} inlined recipient(s) failed Rust age-key \
                 re-validation; re-deriving all_capable=false (keeping master wrap) regardless \
                 of host device_recipients_all_capable",
                inlined_total - recipients.len(),
                inlined_total
            );
        }
        return Some((recipients, all_capable));
    }

    // FS fallback (non-sandboxed daemon/CLI/test path): load the registry.
    let registry_path = config
        .get("device_registry_path")
        .and_then(serde_json::Value::as_str)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(tcfs_secrets::device::default_registry_path);

    let registry = match tcfs_secrets::device::DeviceRegistry::load(&registry_path) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                "wrap_mode={requested:?}: registry load failed ({e}); using master wrap"
            );
            return None;
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
            "wrap_mode={requested:?} enabled but no active age recipients; using master wrap"
        );
        return None;
    }

    let all_capable = registry.roll_call().all_capable();
    Some((recipients, all_capable))
}

/// Resolve THIS device's age unwrap identity, PREFERRING a secret inlined into
/// the config over an on-disk `device-<id>.age` read (TIN-1417 Keychain
/// inlining).
///
/// The macOS FileProvider `.appex` cannot `fs`-read `device-<id>.age`, so the
/// Swift host inlines this device's armored age secret into the Keychain config
/// copy under `device_secret`, mirroring `master_key_base64`. When present we use
/// it verbatim; otherwise we fall back to reading `device-<id>.age` from disk
/// (the daemon/CLI/test path).
#[cfg(feature = "grpc")]
fn resolve_device_identity(
    config: &serde_json::Value,
    device_id: &str,
    requested: tcfs_core::config::WrapMode,
) -> Option<tcfs_sync::engine::DeviceUnwrapIdentity> {
    use tcfs_sync::engine::DeviceUnwrapIdentity;

    // Inlined-first: prefer the Keychain-provided secret over the fs read.
    if let Some(secret) = config
        .get("device_secret")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        return Some(DeviceUnwrapIdentity {
            device_id: device_id.to_string(),
            secret: secret.trim().to_string(),
        });
    }

    // FS fallback: read device-<id>.age relative to the registry path.
    let registry_path = config
        .get("device_registry_path")
        .and_then(serde_json::Value::as_str)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(tcfs_secrets::device::default_registry_path);
    let secret_path = tcfs_secrets::device::device_secret_key_path(&registry_path, device_id);
    match std::fs::read_to_string(&secret_path) {
        Ok(s) => Some(DeviceUnwrapIdentity {
            device_id: device_id.to_string(),
            secret: s.trim().to_string(),
        }),
        Err(e) => {
            tracing::warn!(
                "wrap_mode={requested:?}: local device secret unreadable ({e}); using master wrap"
            );
            None
        }
    }
}

/// Build a DEVICE-AWARE `EncryptionContext` for the FileProvider read path,
/// mirroring `tcfsd`'s `build_encryption_context`.
///
/// Default-off invariant: returns `EncryptionContext::new(master_key)` verbatim
/// unless the config selects a non-`Master` `wrap_mode` (or the legacy
/// `per_device_wrapping: true` alias). In that case it resolves the active
/// recipient set plus this device's age secret and attaches them, like the
/// daemon, and applies the same roll-call gate (PerDevice downgrades to Dual
/// unless every active device has a real age recipient).
///
/// SANDBOX (TIN-1417): both the recipient set and this device's secret are
/// resolved INLINED-FIRST — preferring values the Swift host wrote into the
/// shared-Keychain config copy (`device_recipients` / `device_secret`) over an
/// on-disk read of `devices.json` / `device-<id>.age`. The macOS FileProvider
/// `.appex` cannot `fs`-read `~/.config/tcfs`, so the inlined path is the only
/// one that works in-sandbox; the fs path remains the fallback for the
/// non-sandboxed daemon/CLI and for tests.
///
/// Like the daemon, this falls back to the master-only context (and logs why)
/// only when per-device wrapping is requested but the registry/recipients/secret
/// are unavailable — it never produces a context that this device cannot read
/// back. The engine's read switch independently fails CLOSED if it then meets a
/// per-device manifest with no identity attached.
#[cfg(feature = "grpc")]
pub(crate) fn build_encryption_context(
    config: &serde_json::Value,
    device_id: &str,
    master_key: &tcfs_crypto::MasterKey,
) -> tcfs_sync::engine::EncryptionContext {
    use tcfs_core::config::WrapMode;
    use tcfs_sync::engine::EncryptionContext;

    let base = EncryptionContext::new(master_key.clone());

    // Default-off: byte-identical master-only behavior unless explicitly enabled.
    let requested = wrap_mode_from_config(config);
    if requested == WrapMode::Master {
        return base;
    }

    let (recipients, all_capable) = match resolve_recipients(config, requested) {
        Some(v) => v,
        None => return base,
    };

    let identity = match resolve_device_identity(config, device_id, requested) {
        Some(id) => id,
        None => return base,
    };

    // Roll-call gate: refuse PerDevice (drop master wrap) unless every active
    // device is per-device-capable; otherwise degrade to Dual + warn.
    let effective = if requested == WrapMode::PerDevice {
        if all_capable {
            WrapMode::PerDevice
        } else {
            tracing::warn!(
                "wrap_mode=PerDevice REFUSED by roll-call gate (not all active devices \
                 carry a real age recipient); falling back to Dual (keeping the master wrap)"
            );
            WrapMode::Dual
        }
    } else {
        requested
    };

    base.with_wrap_mode(effective, recipients, Some(identity))
}

#[cfg(all(test, feature = "grpc"))]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
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

        let secret_path = tcfs_secrets::device::device_secret_key_path(&registry_path, device_id);
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

    /// Explicit wrap_mode=master also stays master-only.
    #[test]
    fn explicit_disabled_is_master_only() {
        let config = serde_json::json!({ "wrap_mode": "master" });
        let ctx = build_encryption_context(&config, "device-a", &master());
        assert!(ctx.device_recipients.is_empty());
        assert!(ctx.device_identity.is_none());
        // Legacy alias: per_device_wrapping=false also stays master-only.
        let config = serde_json::json!({ "per_device_wrapping": false });
        let ctx = build_encryption_context(&config, "device-a", &master());
        assert!(ctx.device_recipients.is_empty());
        assert!(ctx.device_identity.is_none());
    }

    /// Legacy `per_device_wrapping = true` maps to Dual (keeps the master
    /// fallback) and still attaches recipients + this device's identity.
    #[test]
    fn legacy_per_device_wrapping_true_attaches_dual_context() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _pub = provision_device(tmp.path(), "device-a");
        let config = serde_json::json!({
            "per_device_wrapping": true,
            "device_registry_path": tmp.path().join("devices.json").to_str().unwrap(),
        });
        let ctx = build_encryption_context(&config, "device-a", &master());
        assert_eq!(ctx.wrap_mode, tcfs_sync::engine::WrapMode::Dual);
        assert_eq!(ctx.device_recipients.len(), 1);
        assert!(ctx.device_identity.is_some());
    }

    /// When wrap_mode=per_device with a real registry + local secret, the context
    /// carries this device's unwrap identity and the active-device recipient set,
    /// and (roll-call satisfied) stays in PerDevice.
    #[test]
    fn enabled_attaches_device_identity_and_recipients() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _pub = provision_device(tmp.path(), "device-a");

        let config = serde_json::json!({
            "wrap_mode": "per_device",
            "device_registry_path": tmp.path().join("devices.json").to_str().unwrap(),
        });
        let ctx = build_encryption_context(&config, "device-a", &master());
        assert_eq!(ctx.wrap_mode, tcfs_sync::engine::WrapMode::PerDevice);

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
            "wrap_mode": "per_device",
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
        let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

        // Build the write context from the same registry the FP read context uses.
        let enabled_config = serde_json::json!({
            "wrap_mode": "per_device",
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
            &op,
            &up.remote_path,
            &dst_ok,
            prefix,
            None,
            "device-a",
            None,
            Some(&read_ctx),
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
            &op,
            &up.remote_path,
            &dst_closed,
            prefix,
            None,
            "device-a",
            None,
            Some(&master_only),
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
        let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

        // Legacy master-only write (no per-device recipients).
        let write_ctx = tcfs_sync::engine::EncryptionContext::new(master());
        let content = b"legacy master-only payload";
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
            &op,
            &up.remote_path,
            &dst,
            prefix,
            None,
            "device-a",
            None,
            Some(&read_ctx),
        )
        .await
        .expect("default FP context must read master-only manifest unchanged");
        assert_eq!(std::fs::read(&dst).unwrap(), content);
    }

    /// SANDBOX (TIN-1417): the INLINED Keychain-config path is preferred over any
    /// fs read. With `device_recipients` + `device_secret` inlined and NO
    /// registry/secret on disk (and a deliberately bogus `device_registry_path`),
    /// the context still attaches recipients + this device's identity — proving
    /// the FileProvider `.appex` can build a device-aware context without ever
    /// touching `~/.config/tcfs`.
    #[test]
    fn inlined_recipients_and_secret_are_preferred_without_fs() {
        let key = tcfs_secrets::device::generate_local_device_key();
        let secret = key.secret_key.expose_secret().to_string();
        let config = serde_json::json!({
            "wrap_mode": "per_device",
            // Point at a path that does NOT exist on disk to prove no fs read
            // happens when the inlined values are present.
            "device_registry_path": "/nonexistent/tcfs/devices.json",
            "device_recipients": [
                { "device_id": "device-a", "recipient": key.public_key }
            ],
            "device_recipients_all_capable": true,
            "device_secret": secret,
        });
        let ctx = build_encryption_context(&config, "device-a", &master());
        assert_eq!(
            ctx.wrap_mode,
            tcfs_sync::engine::WrapMode::PerDevice,
            "Rust-re-derived all_capable (1 of 1 well-formed) should keep PerDevice"
        );
        assert_eq!(
            ctx.device_recipients.len(),
            1,
            "inlined recipient set must be used"
        );
        assert_eq!(ctx.device_recipients[0].device_id, "device-a");
        let identity = ctx
            .device_identity
            .as_ref()
            .expect("inlined secret must attach a device identity");
        assert_eq!(identity.device_id, "device-a");
        assert_eq!(
            identity.secret, secret,
            "inlined secret must be used verbatim (no fs read)"
        );
    }

    /// all_capable is RE-DERIVED in Rust, NOT trusted from the host boolean.
    /// Here a single inlined recipient is malformed (would fail the real
    /// `age::x25519::Recipient` parse) while a second is well-formed, and the
    /// host asserts `device_recipients_all_capable=true` (a Swift over-count via
    /// its prefix/length heuristic). Rust must drop the malformed recipient,
    /// re-derive all_capable=FALSE (1 of 2 entries failed re-validation), and
    /// therefore downgrade PerDevice -> Dual (keeping the master wrap) instead of
    /// locking the device out. This is the core hardening guard.
    #[test]
    fn host_all_capable_true_with_malformed_recipient_redrives_false_in_rust() {
        let good = tcfs_secrets::device::generate_local_device_key();
        let secret = good.secret_key.expose_secret().to_string();
        // Precondition: confirm the keys are what we expect — one real, one not.
        assert!(tcfs_secrets::device::is_real_age_public_key(
            &good.public_key
        ));
        let bogus = "age1-this-is-not-a-real-bech32-age-recipient-key-000000";
        assert!(
            !tcfs_secrets::device::is_real_age_public_key(bogus),
            "bogus recipient must fail the real age parse"
        );

        let config = serde_json::json!({
            "wrap_mode": "per_device",
            "device_recipients": [
                { "device_id": "device-a", "recipient": good.public_key },
                { "device_id": "device-b", "recipient": bogus }
            ],
            // Host LIES (over-counts via its heuristic): claims all capable.
            "device_recipients_all_capable": true,
            "device_secret": secret,
        });
        let ctx = build_encryption_context(&config, "device-a", &master());
        // The malformed recipient is dropped from the wrap set...
        assert_eq!(
            ctx.device_recipients.len(),
            1,
            "malformed inlined recipient must be dropped by Rust re-validation"
        );
        assert_eq!(ctx.device_recipients[0].device_id, "device-a");
        // ...and all_capable is re-derived to false (1 of 2 failed), so despite
        // the host's all_capable=true we must NOT drop the master wrap.
        assert_eq!(
            ctx.wrap_mode,
            tcfs_sync::engine::WrapMode::Dual,
            "host all_capable=true must be IGNORED; Rust re-derivation -> Dual, \
             keeping the master wrap (no lockout)"
        );
        assert!(ctx.device_identity.is_some());
    }

    /// The inlined `device_recipients_all_capable=false` signal must NOT be able
    /// to FORCE all_capable when Rust re-derivation says every inlined recipient
    /// is well-formed. Re-derivation is authoritative: with a single valid
    /// recipient (1 of 1 parsed) all_capable re-derives TRUE and PerDevice is
    /// kept, even though the host asserted `false`. (The host boolean is not
    /// trusted in EITHER direction.)
    #[test]
    fn host_all_capable_false_is_overridden_by_rust_when_recipients_well_formed() {
        let key = tcfs_secrets::device::generate_local_device_key();
        let secret = key.secret_key.expose_secret().to_string();
        let config = serde_json::json!({
            "wrap_mode": "per_device",
            "device_recipients": [
                { "device_id": "device-a", "recipient": key.public_key }
            ],
            // Host asserts NOT all capable; Rust re-derivation disagrees.
            "device_recipients_all_capable": false,
            "device_secret": secret,
        });
        let ctx = build_encryption_context(&config, "device-a", &master());
        assert_eq!(
            ctx.wrap_mode,
            tcfs_sync::engine::WrapMode::PerDevice,
            "Rust re-derivation (1 of 1 well-formed) -> all_capable=true; host \
             false is not trusted"
        );
        assert_eq!(ctx.device_recipients.len(), 1);
        assert!(ctx.device_identity.is_some());
    }

    /// The fs path is the FALLBACK: when no inlined values are present the context
    /// is built from `devices.json` + `device-<id>.age` exactly as before. This is
    /// the daemon/CLI/non-sandboxed path. (Regression guard that inlining did not
    /// break the on-disk path.)
    #[test]
    fn fs_path_is_used_as_fallback_when_not_inlined() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _pub = provision_device(tmp.path(), "device-a");
        let config = serde_json::json!({
            "wrap_mode": "per_device",
            "device_registry_path": tmp.path().join("devices.json").to_str().unwrap(),
        });
        let ctx = build_encryption_context(&config, "device-a", &master());
        assert_eq!(ctx.wrap_mode, tcfs_sync::engine::WrapMode::PerDevice);
        assert_eq!(ctx.device_recipients.len(), 1);
        let identity = ctx
            .device_identity
            .as_ref()
            .expect("fs fallback must attach a device identity");
        assert!(identity.secret.starts_with("AGE-SECRET-KEY-"));
    }

    /// Mixed precedence: inlined recipients but NO inlined secret -> the secret is
    /// resolved via the fs fallback. Proves the two resolvers are independent and
    /// each is inlined-first / fs-fallback on its own.
    #[test]
    fn inlined_recipients_with_fs_secret_fallback() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pub_key = provision_device(tmp.path(), "device-a");
        let config = serde_json::json!({
            "wrap_mode": "per_device",
            "device_registry_path": tmp.path().join("devices.json").to_str().unwrap(),
            "device_recipients": [
                { "device_id": "device-a", "recipient": pub_key }
            ],
            "device_recipients_all_capable": true,
            // No device_secret -> falls back to reading device-a.age from disk.
        });
        let ctx = build_encryption_context(&config, "device-a", &master());
        assert_eq!(ctx.wrap_mode, tcfs_sync::engine::WrapMode::PerDevice);
        assert_eq!(ctx.device_recipients.len(), 1);
        let identity = ctx
            .device_identity
            .as_ref()
            .expect("secret should resolve via fs fallback");
        assert!(identity.secret.starts_with("AGE-SECRET-KEY-"));
    }

    /// Inlined `device_secret` present but recipients absent and no on-disk
    /// registry -> recipient resolution fails, so the context fails back to
    /// master-only (never a half-wired identity-only context). Fail-safe.
    #[test]
    fn inlined_secret_without_recipients_falls_back_to_master() {
        let key = tcfs_secrets::device::generate_local_device_key();
        let secret = key.secret_key.expose_secret().to_string();
        let config = serde_json::json!({
            "wrap_mode": "per_device",
            "device_registry_path": "/nonexistent/tcfs/devices.json",
            "device_secret": secret,
        });
        let ctx = build_encryption_context(&config, "device-a", &master());
        assert!(
            ctx.device_recipients.is_empty(),
            "no recipients resolvable -> master-only"
        );
        assert!(
            ctx.device_identity.is_none(),
            "must not attach a half-wired identity-only context"
        );
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
