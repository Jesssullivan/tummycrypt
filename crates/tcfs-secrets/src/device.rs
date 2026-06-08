//! Device identity management for multi-device E2E encryption.
//!
//! Each device gets its own age X25519 keypair. Device secret keys are stored in
//! the platform keychain or an owner-only age-encrypted file.
//!
//! ## Registry integrity (TIN-1417 B4)
//!
//! The *registry itself* (the list of devices, which doubles as the per-device
//! age **recipient set**) is the high-value attack surface: anyone with direct
//! object-store write access to `meta_prefix/tcfs-meta/devices.json` could inject
//! a hostile recipient, un-revoke a device, or drop a device. To make per-device
//! revocation trustworthy the registry is wrapped in an **Ed25519 signature** over
//! a canonical serialization of the device list.
//!
//! The signing key is *derived deterministically from the master key* via
//! HKDF-SHA256 (domain `tcfs-device-registry-signing-v1`), so any master-key
//! holder can sign and verify without distributing new key material. The
//! signature and the signer's Ed25519 verifying key are stored alongside the
//! device list in the same JSON object (see [`SIGNATURE_FIELD`]). Old (unsigned)
//! registries still deserialize for a bounded back-compat migration window, but
//! the recipient-set builders refuse to wrap to recipients from a registry that
//! fails verification — they fall back to the shared master wrap instead.
//!
//! `signing_key_hash` on a [`DeviceIdentity`] is a BLAKE3 self-fingerprint of the
//! device's *own* public key — it is NOT a signature and conveys no authority.

use anyhow::{Context, Result};
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// HKDF info string domain-separating the registry-signing key from every other
/// master-derived key. Changing this string rotates every registry signature.
const REGISTRY_SIGNING_INFO: &[u8] = b"tcfs-device-registry-signing-v1";

/// Signature algorithm tag stored in the envelope. Lets future versions migrate.
const REGISTRY_SIG_ALG: &str = "ed25519-hkdf-sha256-v1";

/// JSON field that carries the base64 Ed25519 signature in the on-disk envelope.
pub const SIGNATURE_FIELD: &str = "registry_signature";

/// A registered device identity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceIdentity {
    /// Human-readable device name (e.g., "yoga-laptop")
    pub name: String,
    /// UUID v4 device identifier (generated at enrollment)
    #[serde(default)]
    pub device_id: String,
    /// age public key (age1...)
    pub public_key: String,
    /// BLAKE3 hash of the signing key
    #[serde(default)]
    pub signing_key_hash: String,
    /// Human-readable description
    #[serde(default)]
    pub description: Option<String>,
    /// Unix timestamp of enrollment
    pub enrolled_at: u64,
    /// Whether this device is revoked
    pub revoked: bool,
    /// Unix timestamp at which this device was revoked, if any (TIN-1417 B4).
    /// `None` for active devices and for legacy entries revoked before this field
    /// existed; set by [`DeviceRegistry::revoke`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<u64>,
    /// Ed25519 verifying key of the device that enrolled this one, base64
    /// (TIN-1417 B4). Optional/forward-looking: lets a future per-event signed
    /// log attribute enrollment. `None` on legacy entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrolled_by: Option<String>,
    /// This device's own Ed25519 signing public key, base64 (TIN-1417 B4).
    /// Reserved for a future per-device append-only signed-event log; the SEV-1
    /// fix signs the *whole registry* with the master-derived key, so this is
    /// `None` today and carries no authority when present. `#[serde(default)]`
    /// keeps legacy registries parseable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_pubkey: Option<String>,
    /// Last NATS JetStream sequence processed by this device
    #[serde(default)]
    pub last_nats_seq: u64,
}

/// Result of a per-device roll-call probe (TIN-1417).
///
/// See [`DeviceRegistry::roll_call`]. The CONTRACT (`PerDevice`) wrap mode is
/// only safe to enter when `all_capable()` is true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollCall {
    /// Number of active (non-revoked) devices.
    pub active: usize,
    /// Number of active devices that carry a real age recipient.
    pub capable: usize,
    /// Names of active devices that lack a real age recipient (the blockers).
    pub incapable_devices: Vec<String>,
}

impl RollCall {
    /// True when at least one active device exists and EVERY active device is
    /// per-device-capable (has a real age recipient). Only then is it safe to
    /// drop the shared master wrap.
    pub fn all_capable(&self) -> bool {
        self.active > 0 && self.capable == self.active && self.incapable_devices.is_empty()
    }
}

/// Device registry: tracks all enrolled devices for this user.
///
/// The registry is the per-device age **recipient set**, so its integrity is
/// security-critical (TIN-1417 B4). When persisted it is wrapped in an Ed25519
/// signature (see [`DeviceRegistry::sign`] / [`DeviceRegistry::verify_signature`]);
/// the signature and signer pubkey live in `registry_signature` / `signer_pubkey`
/// and are deliberately excluded from the signed payload so they can be (re)written
/// without invalidating the signature over the device list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceRegistry {
    /// List of enrolled devices
    pub devices: Vec<DeviceIdentity>,
    /// Base64 Ed25519 signature over the canonical device list (TIN-1417 B4).
    /// `None` for an unsigned/legacy registry. Not part of the signed payload.
    #[serde(
        default,
        rename = "registry_signature",
        skip_serializing_if = "Option::is_none"
    )]
    pub registry_signature: Option<String>,
    /// Base64 Ed25519 verifying key of whoever signed the registry (the
    /// master-derived signer). Bound into verification: a signature that verifies
    /// against an *unexpected* signer is still rejected. Not part of the signed
    /// payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_pubkey: Option<String>,
    /// Signature algorithm tag, e.g. `ed25519-hkdf-sha256-v1`. Not part of the
    /// signed payload (the algorithm is implied by the verifier).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig_alg: Option<String>,
}

/// Outcome of verifying a loaded registry against the master-derived signer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryTrust {
    /// Envelope carried a valid signature from the expected master-derived signer.
    Signed,
    /// Legacy registry with no signature. Accepted only inside the migration
    /// window; callers must treat its recipient set as UNVERIFIED.
    UnsignedLegacy,
}

/// HKDF-derive the deterministic Ed25519 registry-signing key from the master key.
///
/// Domain-separated by [`REGISTRY_SIGNING_INFO`] so it is independent of the
/// manifest/name keys. Every master-key holder derives the same signer, so no new
/// key distribution is required to sign or verify the registry.
fn registry_signing_key(master_key_bytes: &[u8; 32]) -> SigningKey {
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, master_key_bytes);
    let mut seed = [0u8; 32];
    // HKDF-Expand into 32 bytes never fails for this length.
    hk.expand(REGISTRY_SIGNING_INFO, &mut seed)
        .expect("HKDF expand into 32 bytes is infallible");
    let key = SigningKey::from_bytes(&seed);
    seed.fill(0);
    key
}

/// Base64 (standard, padded) helper used for signatures and keys.
fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// Newly generated local device key material.
///
/// The secret half must be persisted outside `DeviceRegistry`; the registry is
/// intended to be shareable metadata and should only contain public keys.
#[derive(Clone)]
pub struct LocalDeviceKey {
    pub public_key: String,
    pub secret_key: SecretString,
}

impl DeviceRegistry {
    /// Load device registry from a JSON file (no signature verification).
    ///
    /// Retained for non-crypto callers (e.g. `tcfs device list`, status probes)
    /// that have no master key. Security-sensitive callers that build a per-device
    /// **recipient set** MUST use [`DeviceRegistry::load_verified`] instead.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading device registry: {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("parsing device registry: {}", path.display()))
    }

    /// Load a registry from a JSON file AND verify its signature (TIN-1417 B4).
    ///
    /// Returns the registry plus its [`RegistryTrust`]. A present-but-invalid
    /// signature (tampered device list, un-revoked entry, injected recipient, or a
    /// signature from a non-master-derived key) is a HARD error. A registry with
    /// no signature loads as [`RegistryTrust::UnsignedLegacy`] (migration window)
    /// — callers MUST NOT build a per-device recipient set from an unsigned
    /// registry. A missing file is an empty, signed-by-construction registry.
    pub fn load_verified(
        path: &Path,
        master_key_bytes: &[u8; 32],
    ) -> Result<(Self, RegistryTrust)> {
        if !path.exists() {
            return Ok((Self::default(), RegistryTrust::Signed));
        }
        let reg = Self::load(path)?;
        let trust = reg.verify_signature(master_key_bytes).with_context(|| {
            format!(
                "verifying device registry signature: {} (refusing to trust a tampered registry)",
                path.display()
            )
        })?;
        if trust == RegistryTrust::UnsignedLegacy {
            tracing::warn!(
                path = %path.display(),
                "device registry is UNSIGNED (legacy). Accepting inside the TIN-1417 B4 \
                 migration window, but its recipient set is UNVERIFIED and will not be used \
                 for per-device wrapping. Re-save with a master-key-holding command to sign it."
            );
        }
        Ok((reg, trust))
    }

    /// Save device registry to a JSON file.
    ///
    /// NOTE: this writes whatever signature envelope is currently on `self`. To
    /// produce a *freshly signed* registry use [`DeviceRegistry::save_signed`],
    /// which signs with the master-derived key before writing. Raw `save` is
    /// retained for non-crypto callers and round-trips an existing envelope.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating dir: {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self).context("serializing device registry")?;
        std::fs::write(path, json)
            .with_context(|| format!("writing device registry: {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("chmod device registry: {}", path.display()))?;
        }
        Ok(())
    }

    /// Sign with the master-derived key, then persist to a JSON file (TIN-1417 B4).
    ///
    /// This is the preferred writer for any mutating command: it guarantees the
    /// on-disk registry carries a fresh, valid signature so subsequent
    /// verification (load-side and recipient-set builders) succeeds.
    pub fn save_signed(&mut self, path: &Path, master_key_bytes: &[u8; 32]) -> Result<()> {
        self.sign(master_key_bytes)?;
        self.save(path)
    }

    /// Add a new device
    pub fn add(&mut self, device: DeviceIdentity) {
        self.devices.push(device);
    }

    /// List active (non-revoked) devices
    pub fn active_devices(&self) -> impl Iterator<Item = &DeviceIdentity> {
        self.devices.iter().filter(|d| !d.revoked)
    }

    // ── TIN-1417 B4: registry signing / verification ─────────────────────────

    /// Build the canonical, deterministic byte string that is signed.
    ///
    /// Independent of map/field ordering, of the envelope fields (signature,
    /// signer pubkey, alg) and of pretty-printing: it serializes a *sorted* copy
    /// of the device list to canonical JSON. Two registries with the same device
    /// set (regardless of insertion order or signature) produce identical bytes,
    /// so the signature is stable and tamper-evident.
    fn canonical_signing_payload(&self) -> Result<Vec<u8>> {
        let mut devices = self.devices.clone();
        // Deterministic order: by device_id, then name (stable for empty ids).
        devices.sort_by(|a, b| {
            a.device_id
                .cmp(&b.device_id)
                .then_with(|| a.name.cmp(&b.name))
        });
        // serde_json serializes struct fields in declaration order, giving a
        // canonical encoding for a fixed schema. Domain-prefix the algorithm tag
        // so a signature can never be replayed under a different scheme.
        let body = serde_json::to_vec(&devices).context("serializing canonical device list")?;
        let mut msg = Vec::with_capacity(body.len() + REGISTRY_SIG_ALG.len() + 1);
        msg.extend_from_slice(REGISTRY_SIG_ALG.as_bytes());
        msg.push(0u8);
        msg.extend_from_slice(&body);
        Ok(msg)
    }

    /// Sign the registry with the master-derived Ed25519 key (TIN-1417 B4).
    ///
    /// Populates `registry_signature`, `signer_pubkey` and `sig_alg`. Idempotent:
    /// re-signing an unchanged device list reproduces the same signature.
    pub fn sign(&mut self, master_key_bytes: &[u8; 32]) -> Result<()> {
        let signing_key = registry_signing_key(master_key_bytes);
        let payload = self.canonical_signing_payload()?;
        let sig: Signature = signing_key.sign(&payload);
        let verifying: VerifyingKey = signing_key.verifying_key();
        self.registry_signature = Some(b64().encode(sig.to_bytes()));
        self.signer_pubkey = Some(b64().encode(verifying.to_bytes()));
        self.sig_alg = Some(REGISTRY_SIG_ALG.to_string());
        Ok(())
    }

    /// Verify the registry signature against the master-derived signer.
    ///
    /// Returns [`RegistryTrust::Signed`] on a valid signature from the expected
    /// (master-derived) signer. Returns [`RegistryTrust::UnsignedLegacy`] for a
    /// registry that carries NO signature (migration window). Returns a hard
    /// `Err` when a signature is present but does not verify, is malformed, or was
    /// produced by an unexpected (non-master-derived) signer — i.e. tampering.
    pub fn verify_signature(&self, master_key_bytes: &[u8; 32]) -> Result<RegistryTrust> {
        let expected = registry_signing_key(master_key_bytes).verifying_key();

        match (&self.registry_signature, &self.signer_pubkey) {
            (None, None) => Ok(RegistryTrust::UnsignedLegacy),
            (Some(_), None) | (None, Some(_)) => Err(anyhow::anyhow!(
                "device registry signature envelope is incomplete (signature/signer_pubkey \
                 mismatch); refusing to trust"
            )),
            (Some(sig_b64), Some(signer_b64)) => {
                // 1. The signer pubkey in the envelope MUST be the master-derived
                //    one. A signature that verifies against an attacker-chosen key
                //    is worthless, so bind to the expected signer first.
                let signer_bytes = b64()
                    .decode(signer_b64.as_bytes())
                    .context("decoding registry signer pubkey")?;
                let signer_arr: [u8; 32] = signer_bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("registry signer pubkey is not 32 bytes"))?;
                if signer_arr != expected.to_bytes() {
                    return Err(anyhow::anyhow!(
                        "device registry signed by an UNEXPECTED key (not the master-derived \
                         signer); refusing to trust — possible tampering or wrong master key"
                    ));
                }

                // 2. Verify the signature over the canonical payload.
                let sig_bytes = b64()
                    .decode(sig_b64.as_bytes())
                    .context("decoding registry signature")?;
                let sig_arr: [u8; 64] = sig_bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("registry signature is not 64 bytes"))?;
                let signature = Signature::from_bytes(&sig_arr);
                let payload = self.canonical_signing_payload()?;
                expected.verify(&payload, &signature).map_err(|_| {
                    anyhow::anyhow!(
                        "device registry signature FAILED verification; the device list was \
                         tampered with or re-signed by a different key"
                    )
                })?;
                Ok(RegistryTrust::Signed)
            }
        }
    }

    /// True when the registry carries a signature envelope (signed or tampered),
    /// as opposed to an unsigned legacy registry.
    pub fn is_signed(&self) -> bool {
        self.registry_signature.is_some() || self.signer_pubkey.is_some()
    }

    /// Roll-call probe for the per-device wrapping CONTRACT (TIN-1417).
    ///
    /// Inspects every active (non-revoked) device and reports whether each one
    /// carries a *real* age recipient. The daemon must NOT drop the shared
    /// master wrap (i.e. enter [`crate`]'s `PerDevice` contract mode) unless this
    /// returns [`RollCall::all_capable`] true — otherwise a device that lacks a
    /// real age recipient would be locked out of newly written content.
    ///
    /// When the roll call is not satisfied callers fall back to dual-wrapping
    /// (master + per-device) and log the offending devices, never silently
    /// dropping the master fallback.
    pub fn roll_call(&self) -> RollCall {
        let mut active = 0usize;
        let mut capable = 0usize;
        let mut incapable_devices = Vec::new();
        for d in self.active_devices() {
            active += 1;
            if is_real_age_public_key(&d.public_key) {
                capable += 1;
            } else {
                incapable_devices.push(d.name.clone());
            }
        }
        RollCall {
            active,
            capable,
            incapable_devices,
        }
    }

    /// Revoke a device by name.
    ///
    /// Sets `revoked = true` and stamps `revoked_at` (TIN-1417 B4) so the signed
    /// envelope records *when* the device lost trust. Callers must re-`sign` and
    /// persist the registry afterwards; the whole-registry signature plus
    /// `revoked_at` is the SEV-1 fix (a full append-only signed-event log is a
    /// follow-up).
    pub fn revoke(&mut self, name: &str) -> bool {
        if let Some(device) = self.devices.iter_mut().find(|d| d.name == name) {
            device.revoked = true;
            if device.revoked_at.is_none() {
                device.revoked_at = Some(now_unix());
            }
            true
        } else {
            false
        }
    }

    /// Find a device by name
    pub fn find(&self, name: &str) -> Option<&DeviceIdentity> {
        self.devices.iter().find(|d| d.name == name)
    }

    /// Backfill a missing device_id with a new UUID v4.
    /// Returns the new device_id, or None if the device was not found.
    pub fn backfill_device_id(&mut self, name: &str) -> Option<String> {
        if let Some(device) = self.devices.iter_mut().find(|d| d.name == name) {
            let new_id = uuid::Uuid::new_v4().to_string();
            device.device_id = new_id.clone();
            // Also backfill signing_key_hash if missing
            if device.signing_key_hash.is_empty() {
                device.signing_key_hash =
                    blake3::hash(device.public_key.as_bytes()).to_hex().as_str()[..16].to_string();
            }
            Some(new_id)
        } else {
            None
        }
    }

    /// Find a device by UUID
    pub fn find_by_id(&self, device_id: &str) -> Option<&DeviceIdentity> {
        self.devices.iter().find(|d| d.device_id == device_id)
    }

    /// Enroll a new device: generates a UUID, creates identity, adds to registry.
    pub fn enroll(&mut self, name: &str, public_key: &str, description: Option<String>) -> String {
        let device_id = uuid::Uuid::new_v4().to_string();
        let now = now_unix();

        let signing_hash = blake3::hash(public_key.as_bytes()).to_hex().as_str()[..16].to_string();

        self.add(DeviceIdentity {
            name: name.to_string(),
            device_id: device_id.clone(),
            public_key: public_key.to_string(),
            signing_key_hash: signing_hash,
            description,
            enrolled_at: now,
            revoked: false,
            revoked_at: None,
            enrolled_by: None,
            signing_pubkey: None,
            last_nats_seq: 0,
        });

        device_id
    }

    /// Enroll a local device with a real age X25519 keypair.
    ///
    /// Returns `(device_id, key_material)`. Callers must persist
    /// `key_material.secret_key` with `save_device_secret_key` before exposing
    /// the registry as usable.
    pub fn enroll_local(
        &mut self,
        name: &str,
        description: Option<String>,
    ) -> (String, LocalDeviceKey) {
        let key = generate_local_device_key();
        let device_id = self.enroll(name, &key.public_key, description);
        (device_id, key)
    }

    /// Load device registry from S3 remote storage (no signature verification).
    ///
    /// Security-sensitive callers must use [`DeviceRegistry::load_remote_verified`].
    pub async fn load_remote(op: &opendal::Operator, meta_prefix: &str) -> Result<Self> {
        let key = format!(
            "{}/tcfs-meta/devices.json",
            meta_prefix.trim_end_matches('/')
        );

        match op.read(&key).await {
            Ok(data) => {
                let content = String::from_utf8(data.to_bytes().to_vec())
                    .context("remote device registry is not UTF-8")?;
                serde_json::from_str(&content).context("parsing remote device registry")
            }
            Err(e) if e.kind() == opendal::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(anyhow::anyhow!("reading remote device registry: {e}")),
        }
    }

    /// Load the remote registry AND verify its signature (TIN-1417 B4).
    ///
    /// The remote object store is the primary tamper surface: anyone with direct
    /// write access to `meta_prefix/tcfs-meta/devices.json` could inject a hostile
    /// recipient. A present-but-invalid signature is a HARD error; an unsigned
    /// remote registry returns [`RegistryTrust::UnsignedLegacy`] with a warning.
    pub async fn load_remote_verified(
        op: &opendal::Operator,
        meta_prefix: &str,
        master_key_bytes: &[u8; 32],
    ) -> Result<(Self, RegistryTrust)> {
        let reg = Self::load_remote(op, meta_prefix).await?;
        if reg.devices.is_empty() && !reg.is_signed() {
            // Nothing was stored yet (NotFound -> default); treat as trusted-empty.
            return Ok((reg, RegistryTrust::Signed));
        }
        let trust = reg
            .verify_signature(master_key_bytes)
            .context("verifying remote device registry signature")?;
        if trust == RegistryTrust::UnsignedLegacy {
            tracing::warn!(
                "remote device registry is UNSIGNED (legacy). Accepting inside the TIN-1417 B4 \
                 migration window; its recipient set is UNVERIFIED."
            );
        }
        Ok((reg, trust))
    }

    /// Sync (upload) device registry to S3 remote storage (writes current envelope).
    ///
    /// Prefer [`DeviceRegistry::sync_to_remote_signed`] which re-signs first.
    pub async fn sync_to_remote(&self, op: &opendal::Operator, meta_prefix: &str) -> Result<()> {
        let key = format!(
            "{}/tcfs-meta/devices.json",
            meta_prefix.trim_end_matches('/')
        );
        let json = serde_json::to_string_pretty(self).context("serializing device registry")?;
        op.write(&key, json.into_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("writing remote device registry: {e}"))?;
        Ok(())
    }

    /// Sign with the master-derived key, then upload to S3 (TIN-1417 B4).
    pub async fn sync_to_remote_signed(
        &mut self,
        op: &opendal::Operator,
        meta_prefix: &str,
        master_key_bytes: &[u8; 32],
    ) -> Result<()> {
        self.sign(master_key_bytes)?;
        self.sync_to_remote(op, meta_prefix).await
    }

    /// Enroll a device with a real age X25519 keypair and sync to remote S3.
    ///
    /// Returns `(device_id, age_secret_key)`. The caller MUST persist the secret
    /// key securely (e.g., keychain, encrypted file) — it is not recoverable.
    pub async fn enroll_remote(
        &mut self,
        op: &opendal::Operator,
        name: &str,
        meta_prefix: &str,
    ) -> Result<(String, String)> {
        let (device_id, key) = self.enroll_local(name, None);
        self.sync_to_remote(op, meta_prefix).await?;
        Ok((device_id, key.secret_key.expose_secret().to_string()))
    }
}

/// Generate a real local age X25519 device identity.
pub fn generate_local_device_key() -> LocalDeviceKey {
    let identity = age::x25519::Identity::generate();
    LocalDeviceKey {
        public_key: identity.to_public().to_string(),
        secret_key: SecretString::from(identity.to_string().expose_secret().to_string()),
    }
}

/// Return the secret-key path associated with a registry path and device id.
pub fn device_secret_key_path(registry_path: &Path, device_id: &str) -> PathBuf {
    registry_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("device-{device_id}.age"))
}

/// Persist a local device identity secret key with owner-only permissions.
pub fn save_device_secret_key(
    path: &Path,
    secret_key: &SecretString,
    overwrite: bool,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating device key dir: {}", parent.display()))?;
    }

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true);
    if overwrite {
        options.truncate(true);
    } else {
        options.create_new(true);
    }

    let mut file = options
        .open(path)
        .with_context(|| format!("creating device secret key: {}", path.display()))?;
    file.write_all(secret_key.expose_secret().as_bytes())
        .with_context(|| format!("writing device secret key: {}", path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("writing device secret key newline: {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing device secret key: {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod device secret key: {}", path.display()))?;
    }

    Ok(())
}

/// Return true when `public_key` is a parseable age X25519 recipient.
pub fn is_real_age_public_key(public_key: &str) -> bool {
    public_key.parse::<age::x25519::Recipient>().is_ok()
}

/// Current Unix time in seconds (saturating to 0 before the epoch).
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Get the default device registry path
pub fn default_registry_path() -> PathBuf {
    let config_dir = dirs_path();
    config_dir.join("devices.json")
}

/// Get the default tcfs config directory
fn dirs_path() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        })
        .join("tcfs")
}

/// Get the default hostname for device naming
pub fn default_device_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown-device".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn test_registry_add_and_find() {
        let mut reg = DeviceRegistry::default();
        reg.add(DeviceIdentity {
            name: "laptop".into(),
            device_id: "test-uuid".into(),
            public_key: "age1test123".into(),
            signing_key_hash: String::new(),
            description: None,
            enrolled_at: 1000,
            revoked: false,
            revoked_at: None,
            enrolled_by: None,
            signing_pubkey: None,
            last_nats_seq: 0,
        });

        assert_eq!(reg.devices.len(), 1);
        assert!(reg.find("laptop").is_some());
        assert!(reg.find("phone").is_none());
    }

    #[test]
    fn test_registry_revoke() {
        let mut reg = DeviceRegistry::default();
        reg.add(DeviceIdentity {
            name: "old-phone".into(),
            device_id: "test-uuid-2".into(),
            public_key: "age1old".into(),
            signing_key_hash: String::new(),
            description: None,
            enrolled_at: 1000,
            revoked: false,
            revoked_at: None,
            enrolled_by: None,
            signing_pubkey: None,
            last_nats_seq: 0,
        });

        assert!(reg.revoke("old-phone"));
        assert_eq!(reg.active_devices().count(), 0);
        // revoke() stamps revoked_at (TIN-1417 B4).
        assert!(reg.find("old-phone").unwrap().revoked_at.is_some());
        assert!(!reg.revoke("nonexistent"));
    }

    #[test]
    fn test_registry_save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");

        let mut reg = DeviceRegistry::default();
        reg.add(DeviceIdentity {
            name: "test-device".into(),
            device_id: "uuid-abc".into(),
            public_key: "age1abc".into(),
            signing_key_hash: "hash123".into(),
            description: Some("my test device".into()),
            enrolled_at: 2000,
            revoked: false,
            revoked_at: None,
            enrolled_by: None,
            signing_pubkey: None,
            last_nats_seq: 42,
        });
        reg.save(&path).unwrap();

        let loaded = DeviceRegistry::load(&path).unwrap();
        assert_eq!(loaded.devices.len(), 1);
        assert_eq!(loaded.devices[0].name, "test-device");
        assert_eq!(loaded.devices[0].device_id, "uuid-abc");
        assert_eq!(loaded.devices[0].last_nats_seq, 42);
    }

    #[test]
    fn test_enroll_generates_uuid() {
        let mut reg = DeviceRegistry::default();
        let id = reg.enroll("yoga", "age1test", None);
        assert!(!id.is_empty());
        assert!(reg.find("yoga").is_some());
        assert_eq!(reg.find("yoga").unwrap().device_id, id);
    }

    #[test]
    fn test_enroll_local_generates_real_age_keypair() {
        let mut reg = DeviceRegistry::default();
        let (id, key) = reg.enroll_local("neo", None);
        let device = reg.find("neo").unwrap();

        assert_eq!(device.device_id, id);
        assert_eq!(device.public_key, key.public_key);
        assert!(is_real_age_public_key(&device.public_key));
        assert!(!device.public_key.starts_with("age1-device-"));

        let identity: age::x25519::Identity = key
            .secret_key
            .expose_secret()
            .parse()
            .expect("age secret key");
        assert_eq!(identity.to_public().to_string(), device.public_key);
    }

    #[test]
    fn test_save_device_secret_key_uses_owner_only_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-test.age");
        let key = generate_local_device_key();

        save_device_secret_key(&path, &key.secret_key, false).unwrap();
        let persisted = std::fs::read_to_string(&path).unwrap();
        let identity: age::x25519::Identity =
            persisted.trim().parse().expect("persisted age secret key");
        assert_eq!(identity.to_public().to_string(), key.public_key);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        let err = save_device_secret_key(&path, &key.secret_key, false).unwrap_err();
        assert!(
            err.to_string().contains("creating device secret key"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn test_find_by_id() {
        let mut reg = DeviceRegistry::default();
        let id = reg.enroll("xoxd-bates", "age1xoxd", Some("main server".into()));
        assert!(reg.find_by_id(&id).is_some());
        assert!(reg.find_by_id("nonexistent-uuid").is_none());
    }

    // ── TIN-1417: roll-call gate for the per-device CONTRACT ───────────────

    fn real_pubkey() -> String {
        generate_local_device_key().public_key
    }

    #[test]
    fn roll_call_all_capable_when_every_active_device_has_real_recipient() {
        let mut reg = DeviceRegistry::default();
        reg.enroll("a", &real_pubkey(), None);
        reg.enroll("b", &real_pubkey(), None);
        let rc = reg.roll_call();
        assert_eq!(rc.active, 2);
        assert_eq!(rc.capable, 2);
        assert!(rc.incapable_devices.is_empty());
        assert!(
            rc.all_capable(),
            "all active devices are per-device-capable"
        );
    }

    #[test]
    fn roll_call_blocks_when_any_active_device_lacks_real_recipient() {
        let mut reg = DeviceRegistry::default();
        reg.enroll("real", &real_pubkey(), None);
        // Placeholder / non-age public key: not a real recipient.
        reg.enroll("placeholder", "age1xoxd-not-a-real-key", None);
        let rc = reg.roll_call();
        assert_eq!(rc.active, 2);
        assert_eq!(rc.capable, 1);
        assert_eq!(rc.incapable_devices, vec!["placeholder".to_string()]);
        assert!(
            !rc.all_capable(),
            "must block PerDevice while any active device cannot do per-device unwrap"
        );
    }

    #[test]
    fn roll_call_revoked_devices_do_not_block() {
        let mut reg = DeviceRegistry::default();
        reg.enroll("real", &real_pubkey(), None);
        reg.enroll("old-placeholder", "age1xoxd-not-a-real-key", None);
        assert!(reg.revoke("old-placeholder"));
        let rc = reg.roll_call();
        assert_eq!(rc.active, 1);
        assert!(
            rc.all_capable(),
            "a revoked non-capable device must not block the roll call"
        );
    }

    #[test]
    fn roll_call_empty_registry_is_not_capable() {
        let reg = DeviceRegistry::default();
        let rc = reg.roll_call();
        assert_eq!(rc.active, 0);
        assert!(
            !rc.all_capable(),
            "an empty active set must not satisfy the contract gate"
        );
    }

    // ── TIN-1417 B4: signed registry ──────────────────────────────────────────

    const MASTER_A: [u8; 32] = [0x11; 32];
    const MASTER_B: [u8; 32] = [0x22; 32];

    fn signed_registry(master: &[u8; 32]) -> DeviceRegistry {
        let mut reg = DeviceRegistry::default();
        reg.enroll("alpha", &real_pubkey(), None);
        reg.enroll("beta", &real_pubkey(), None);
        reg.sign(master).unwrap();
        reg
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let reg = signed_registry(&MASTER_A);
        assert!(reg.is_signed());
        assert_eq!(
            reg.verify_signature(&MASTER_A).unwrap(),
            RegistryTrust::Signed
        );
    }

    #[test]
    fn signing_is_deterministic_for_same_device_set() {
        let mut a = DeviceRegistry::default();
        let pk1 = real_pubkey();
        let pk2 = real_pubkey();
        a.enroll("one", &pk1, None);
        a.enroll("two", &pk2, None);
        // Same devices (same ids/keys) inserted in the OPPOSITE order.
        let mut b = DeviceRegistry {
            devices: a.devices.iter().cloned().rev().collect(),
            ..Default::default()
        };
        a.sign(&MASTER_A).unwrap();
        b.sign(&MASTER_A).unwrap();
        assert_eq!(
            a.registry_signature, b.registry_signature,
            "canonical signing payload must be order-independent"
        );
    }

    #[test]
    fn tamper_inject_recipient_fails_verification() {
        let mut reg = signed_registry(&MASTER_A);
        // Attacker appends a hostile recipient but cannot re-sign.
        reg.enroll("attacker", &real_pubkey(), None);
        let err = reg.verify_signature(&MASTER_A).unwrap_err();
        assert!(
            err.to_string().contains("FAILED verification"),
            "injecting a recipient must fail signature verification: {err:#}"
        );
    }

    #[test]
    fn tamper_unrevoke_fails_verification() {
        let mut reg = DeviceRegistry::default();
        reg.enroll("keep", &real_pubkey(), None);
        reg.enroll("gone", &real_pubkey(), None);
        reg.revoke("gone");
        reg.sign(&MASTER_A).unwrap();
        assert_eq!(
            reg.verify_signature(&MASTER_A).unwrap(),
            RegistryTrust::Signed
        );
        // Attacker un-revokes the device in place.
        reg.devices
            .iter_mut()
            .find(|d| d.name == "gone")
            .unwrap()
            .revoked = false;
        let err = reg.verify_signature(&MASTER_A).unwrap_err();
        assert!(
            err.to_string().contains("FAILED verification"),
            "un-revoking a device must fail signature verification: {err:#}"
        );
    }

    #[test]
    fn tamper_flip_field_fails_verification() {
        let mut reg = signed_registry(&MASTER_A);
        reg.devices[0].public_key = real_pubkey();
        let err = reg.verify_signature(&MASTER_A).unwrap_err();
        assert!(err.to_string().contains("FAILED verification"), "{err:#}");
    }

    #[test]
    fn signature_from_unrelated_key_is_rejected() {
        // Registry signed by master B is rejected when verified against master A.
        let reg = signed_registry(&MASTER_B);
        let err = reg.verify_signature(&MASTER_A).unwrap_err();
        assert!(
            err.to_string().contains("UNEXPECTED key"),
            "a signature from a non-master-derived signer must be rejected: {err:#}"
        );
    }

    #[test]
    fn forged_signer_pubkey_is_rejected() {
        // Attacker re-signs with their OWN ed25519 key and swaps in their pubkey;
        // verification must still reject because the signer isn't master-derived.
        let mut reg = DeviceRegistry::default();
        reg.enroll("victim", &real_pubkey(), None);
        let attacker = registry_signing_key(&MASTER_B); // any non-master-A key
        let payload = reg.canonical_signing_payload().unwrap();
        let sig: Signature = attacker.sign(&payload);
        reg.registry_signature = Some(b64().encode(sig.to_bytes()));
        reg.signer_pubkey = Some(b64().encode(attacker.verifying_key().to_bytes()));
        reg.sig_alg = Some(REGISTRY_SIG_ALG.to_string());
        let err = reg.verify_signature(&MASTER_A).unwrap_err();
        assert!(err.to_string().contains("UNEXPECTED key"), "{err:#}");
    }

    #[test]
    fn incomplete_envelope_is_rejected() {
        let mut reg = signed_registry(&MASTER_A);
        reg.signer_pubkey = None; // signature present but signer missing
        let err = reg.verify_signature(&MASTER_A).unwrap_err();
        assert!(err.to_string().contains("incomplete"), "{err:#}");
    }

    #[test]
    fn unsigned_legacy_registry_loads_with_migration_trust() {
        let mut reg = DeviceRegistry::default();
        reg.enroll("legacy", &real_pubkey(), None);
        assert!(!reg.is_signed());
        assert_eq!(
            reg.verify_signature(&MASTER_A).unwrap(),
            RegistryTrust::UnsignedLegacy
        );
    }

    #[test]
    fn legacy_json_without_new_fields_still_parses() {
        // A registry as written by the pre-B4 code: no envelope, no new fields.
        let legacy = r#"{
            "devices": [
                {
                    "name": "old",
                    "device_id": "id-1",
                    "public_key": "age1legacy",
                    "enrolled_at": 100,
                    "revoked": false
                }
            ]
        }"#;
        let reg: DeviceRegistry = serde_json::from_str(legacy).unwrap();
        assert_eq!(reg.devices.len(), 1);
        assert_eq!(reg.devices[0].revoked_at, None);
        assert_eq!(reg.devices[0].enrolled_by, None);
        assert!(!reg.is_signed());
    }

    #[test]
    fn save_signed_then_load_verified_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        let mut reg = DeviceRegistry::default();
        reg.enroll("disk", &real_pubkey(), None);
        reg.save_signed(&path, &MASTER_A).unwrap();

        let (loaded, trust) = DeviceRegistry::load_verified(&path, &MASTER_A).unwrap();
        assert_eq!(trust, RegistryTrust::Signed);
        assert_eq!(loaded.devices.len(), 1);

        // A registry signed by a different master fails load_verified.
        let err = DeviceRegistry::load_verified(&path, &MASTER_B).unwrap_err();
        assert!(
            format!("{err:#}").contains("UNEXPECTED key"),
            "load_verified must reject a registry signed by a different master: {err:#}"
        );
    }

    #[test]
    fn load_verified_missing_file_is_trusted_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        let (reg, trust) = DeviceRegistry::load_verified(&path, &MASTER_A).unwrap();
        assert!(reg.devices.is_empty());
        assert_eq!(trust, RegistryTrust::Signed);
    }

    #[test]
    fn tampered_on_disk_json_fails_load_verified() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        let mut reg = DeviceRegistry::default();
        reg.enroll("orig", &real_pubkey(), None);
        reg.save_signed(&path, &MASTER_A).unwrap();

        // Tamper with the JSON on disk: inject a hostile recipient by hand.
        let mut value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let devices = value["devices"].as_array_mut().unwrap();
        devices.push(serde_json::json!({
            "name": "evil",
            "device_id": "evil-id",
            "public_key": real_pubkey(),
            "enrolled_at": 1,
            "revoked": false
        }));
        std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

        let err = DeviceRegistry::load_verified(&path, &MASTER_A).unwrap_err();
        assert!(
            err.to_string()
                .contains("verifying device registry signature")
                || err.to_string().contains("FAILED verification"),
            "a hand-tampered on-disk registry must fail load_verified: {err:#}"
        );
    }

    #[test]
    fn unsigned_registry_serializes_without_new_envelope_or_identity_fields() {
        // Back-compat: an UNSIGNED registry must serialize byte-compatibly with
        // the pre-B4 schema — none of the new optional fields leak into the JSON,
        // so older readers see exactly what they used to.
        let mut reg = DeviceRegistry::default();
        reg.enroll("plain", &real_pubkey(), None);
        let json = serde_json::to_string(&reg).unwrap();
        for forbidden in [
            "registry_signature",
            "signer_pubkey",
            "sig_alg",
            "revoked_at",
            "enrolled_by",
            "signing_pubkey",
        ] {
            assert!(
                !json.contains(forbidden),
                "unsigned registry must not emit `{forbidden}` (back-compat): {json}"
            );
        }
        // A signed registry, by contrast, DOES carry the envelope fields.
        reg.sign(&MASTER_A).unwrap();
        let signed = serde_json::to_string(&reg).unwrap();
        assert!(signed.contains("registry_signature"));
        assert!(signed.contains("signer_pubkey"));
    }
}
