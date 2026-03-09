//! Session management — typed sessions with device identity and permissions.
//!
//! Replaces the raw `Option<MasterKey>` in the daemon with a proper session
//! that tracks who authenticated, what they can do, and when it expires.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Per-device permission set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicePermissions {
    /// Can this device mount filesystems?
    pub can_mount: bool,
    /// Can this device push (upload) files?
    pub can_push: bool,
    /// Can this device pull (download) files?
    pub can_pull: bool,
    /// Can this device manage other devices (enroll/revoke)?
    pub can_admin: bool,
    /// Allowed mount prefixes (empty = all).
    pub allowed_prefixes: Vec<String>,
}

impl Default for DevicePermissions {
    fn default() -> Self {
        Self {
            can_mount: true,
            can_push: true,
            can_pull: true,
            can_admin: false,
            allowed_prefixes: Vec::new(),
        }
    }
}

impl DevicePermissions {
    /// Full admin permissions.
    pub fn admin() -> Self {
        Self {
            can_admin: true,
            ..Default::default()
        }
    }

    /// Read-only permissions (pull + mount, no push/admin).
    pub fn read_only() -> Self {
        Self {
            can_mount: true,
            can_push: false,
            can_pull: true,
            can_admin: false,
            allowed_prefixes: Vec::new(),
        }
    }

    /// Check if the device is allowed to access a given prefix.
    pub fn can_access_prefix(&self, prefix: &str) -> bool {
        if self.allowed_prefixes.is_empty() {
            return true; // No restrictions
        }
        self.allowed_prefixes.iter().any(|p| prefix.starts_with(p))
    }
}

/// An authenticated session bound to a device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session token (BLAKE3 hash of random bytes).
    pub token: String,
    /// Device ID that created this session.
    pub device_id: String,
    /// Device name (human-readable).
    pub device_name: String,
    /// Authentication method used (e.g., "totp", "webauthn", "master_key").
    pub auth_method: String,
    /// Session creation time.
    pub created_at: DateTime<Utc>,
    /// Session expiry time (None = no expiry, relies on explicit lock).
    pub expires_at: Option<DateTime<Utc>>,
    /// Device permissions for this session.
    pub permissions: DevicePermissions,
}

impl Session {
    /// Create a new session with default permissions.
    pub fn new(device_id: &str, device_name: &str, auth_method: &str) -> Self {
        let token = Self::generate_token();
        Self {
            token,
            device_id: device_id.to_string(),
            device_name: device_name.to_string(),
            auth_method: auth_method.to_string(),
            created_at: Utc::now(),
            expires_at: None,
            permissions: DevicePermissions::default(),
        }
    }

    /// Create a session with a specific expiry duration.
    pub fn with_expiry(mut self, hours: u64) -> Self {
        self.expires_at = Some(Utc::now() + chrono::Duration::hours(hours as i64));
        self
    }

    /// Create a session with specific permissions.
    pub fn with_permissions(mut self, permissions: DevicePermissions) -> Self {
        self.permissions = permissions;
        self
    }

    /// Check if this session has expired.
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(expiry) => Utc::now() > expiry,
            None => false,
        }
    }

    /// Check if this session is valid (not expired).
    pub fn is_valid(&self) -> bool {
        !self.is_expired()
    }

    /// Generate a cryptographically random session token.
    fn generate_token() -> String {
        let mut bytes = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
        blake3::hash(&bytes).to_hex().to_string()
    }
}

/// Thread-safe session store supporting multiple concurrent device sessions.
#[derive(Clone)]
pub struct SessionStore {
    /// Active sessions keyed by session token.
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    /// Device → active session token mapping (one session per device).
    device_sessions: Arc<RwLock<HashMap<String, String>>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            device_sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Store a new session, replacing any existing session for the device.
    pub async fn insert(&self, session: Session) {
        let token = session.token.clone();
        let device_id = session.device_id.clone();

        // Revoke previous session for this device
        if let Some(old_token) = self.device_sessions.read().await.get(&device_id) {
            self.sessions.write().await.remove(old_token);
        }

        self.sessions.write().await.insert(token.clone(), session);
        self.device_sessions.write().await.insert(device_id, token);
    }

    /// Validate a session token — returns the session if valid.
    pub async fn validate(&self, token: &str) -> Option<Session> {
        let sessions = self.sessions.read().await;
        let session = sessions.get(token)?;
        if session.is_valid() {
            Some(session.clone())
        } else {
            None
        }
    }

    /// Get the active session for a device.
    pub async fn get_device_session(&self, device_id: &str) -> Option<Session> {
        let token = self.device_sessions.read().await.get(device_id)?.clone();
        self.validate(&token).await
    }

    /// Revoke a specific session by token.
    pub async fn revoke(&self, token: &str) {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.remove(token) {
            self.device_sessions
                .write()
                .await
                .remove(&session.device_id);
        }
    }

    /// Revoke all sessions for a device.
    pub async fn revoke_device(&self, device_id: &str) {
        if let Some(token) = self.device_sessions.write().await.remove(device_id) {
            self.sessions.write().await.remove(&token);
        }
    }

    /// Clean up expired sessions.
    pub async fn cleanup_expired(&self) {
        let mut sessions = self.sessions.write().await;
        let mut device_sessions = self.device_sessions.write().await;
        let expired: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| s.is_expired())
            .map(|(k, _)| k.clone())
            .collect();
        for token in expired {
            if let Some(session) = sessions.remove(&token) {
                device_sessions.remove(&session.device_id);
            }
        }
    }

    /// Number of active (non-expired) sessions.
    pub async fn active_count(&self) -> usize {
        self.sessions
            .read()
            .await
            .values()
            .filter(|s| s.is_valid())
            .count()
    }

    /// Check if any session exists (daemon is "unlocked").
    pub async fn has_active_session(&self) -> bool {
        self.active_count().await > 0
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_lifecycle() {
        let store = SessionStore::new();

        // Create and insert session
        let session = Session::new("device-1", "laptop", "totp");
        let token = session.token.clone();
        store.insert(session).await;

        // Validate
        assert!(store.validate(&token).await.is_some());
        assert!(store.has_active_session().await);
        assert_eq!(store.active_count().await, 1);

        // Revoke
        store.revoke(&token).await;
        assert!(store.validate(&token).await.is_none());
        assert!(!store.has_active_session().await);
    }

    #[tokio::test]
    async fn test_session_expiry() {
        let session = Session::new("device-1", "laptop", "totp").with_expiry(0);
        // Expires immediately (0 hours)
        assert!(session.is_expired());
    }

    #[tokio::test]
    async fn test_device_session_replacement() {
        let store = SessionStore::new();

        let s1 = Session::new("device-1", "laptop", "totp");
        let t1 = s1.token.clone();
        store.insert(s1).await;

        let s2 = Session::new("device-1", "laptop", "webauthn");
        let t2 = s2.token.clone();
        store.insert(s2).await;

        // Old session should be revoked
        assert!(store.validate(&t1).await.is_none());
        // New session should be valid
        assert!(store.validate(&t2).await.is_some());
        assert_eq!(store.active_count().await, 1);
    }

    #[test]
    fn test_permissions() {
        let perms = DevicePermissions::default();
        assert!(perms.can_mount);
        assert!(perms.can_push);
        assert!(perms.can_access_prefix("any/prefix"));

        let restricted = DevicePermissions {
            allowed_prefixes: vec!["git/".to_string()],
            ..Default::default()
        };
        assert!(restricted.can_access_prefix("git/crush-dots"));
        assert!(!restricted.can_access_prefix("secrets/keys"));
    }
}
