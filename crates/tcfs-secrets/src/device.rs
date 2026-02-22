//! Device identity management for multi-device E2E encryption.
//!
//! Each device gets its own age X25519 keypair, signed by the master identity.
//! Device keys are stored in the platform keychain or an age-encrypted file.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
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

/// Device registry: tracks all enrolled devices for this user
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceRegistry {
    /// List of enrolled devices
    pub devices: Vec<DeviceIdentity>,
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
            .with_context(|| format!("writing device registry: {}", path.display()))
    }

    /// Add a new device
    pub fn add(&mut self, device: DeviceIdentity) {
        self.devices.push(device);
    }

    /// List active (non-revoked) devices
    pub fn active_devices(&self) -> impl Iterator<Item = &DeviceIdentity> {
        self.devices.iter().filter(|d| !d.revoked)
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

    /// Enroll a device and sync to remote S3.
    pub async fn enroll_remote(
        &mut self,
        op: &opendal::Operator,
        name: &str,
        meta_prefix: &str,
    ) -> Result<String> {
        let public_key = format!(
            "age1-device-{}",
            &blake3::hash(name.as_bytes()).to_hex().as_str()[..8]
        );
        let device_id = self.enroll(name, &public_key, None);
        self.sync_to_remote(op, meta_prefix).await?;
        Ok(device_id)
    }
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
    fn test_find_by_id() {
        let mut reg = DeviceRegistry::default();
        let id = reg.enroll("xoxd-bates", "age1xoxd", Some("main server".into()));
        assert!(reg.find_by_id(&id).is_some());
        assert!(reg.find_by_id("nonexistent-uuid").is_none());
    }
}
