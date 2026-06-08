//! Device identity management for multi-device E2E encryption.
//!
//! Each device gets its own age X25519 keypair, signed by the master identity.
//! Device keys are stored in the platform keychain or an age-encrypted file.

use anyhow::{Context, Result};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

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

/// Device registry: tracks all enrolled devices for this user
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceRegistry {
    /// List of enrolled devices
    pub devices: Vec<DeviceIdentity>,
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
    /// Load device registry from a JSON file
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading device registry: {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("parsing device registry: {}", path.display()))
    }

    /// Save device registry to a JSON file
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

    /// Add a new device
    pub fn add(&mut self, device: DeviceIdentity) {
        self.devices.push(device);
    }

    /// List active (non-revoked) devices
    pub fn active_devices(&self) -> impl Iterator<Item = &DeviceIdentity> {
        self.devices.iter().filter(|d| !d.revoked)
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

    /// Revoke a device by name
    pub fn revoke(&mut self, name: &str) -> bool {
        if let Some(device) = self.devices.iter_mut().find(|d| d.name == name) {
            device.revoked = true;
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
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let signing_hash = blake3::hash(public_key.as_bytes()).to_hex().as_str()[..16].to_string();

        self.add(DeviceIdentity {
            name: name.to_string(),
            device_id: device_id.clone(),
            public_key: public_key.to_string(),
            signing_key_hash: signing_hash,
            description,
            enrolled_at: now,
            revoked: false,
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

    /// Load device registry from S3 remote storage.
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

    /// Sync (upload) device registry to S3 remote storage.
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
            last_nats_seq: 0,
        });

        assert!(reg.revoke("old-phone"));
        assert_eq!(reg.active_devices().count(), 0);
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
}
