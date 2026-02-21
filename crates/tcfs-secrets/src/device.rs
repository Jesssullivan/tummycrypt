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
    /// age public key (age1...)
    pub public_key: String,
    /// Unix timestamp of enrollment
    pub enrolled_at: u64,
    /// Whether this device is revoked
    pub revoked: bool,
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
            public_key: "age1test123".into(),
            enrolled_at: 1000,
            revoked: false,
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
            public_key: "age1old".into(),
            enrolled_at: 1000,
            revoked: false,
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
            public_key: "age1abc".into(),
            enrolled_at: 2000,
            revoked: false,
        });
        reg.save(&path).unwrap();

        let loaded = DeviceRegistry::load(&path).unwrap();
        assert_eq!(loaded.devices.len(), 1);
        assert_eq!(loaded.devices[0].name, "test-device");
    }
}
