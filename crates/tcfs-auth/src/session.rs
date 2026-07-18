//! Session management — typed sessions with device identity and permissions.
//!
//! Replaces the raw `Option<MasterKey>` in the daemon with a proper session
//! that tracks who authenticated, what they can do, and when it expires.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

fn safe_permission_prefix(value: &str) -> Option<&str> {
    let value = value.trim_matches('/');
    if value.is_empty()
        || value.contains('\\')
        || value.chars().any(char::is_control)
        || value
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        None
    } else {
        Some(value)
    }
}

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
        let Some(requested) = safe_permission_prefix(prefix) else {
            return false;
        };
        self.allowed_prefixes.iter().any(|allowed| {
            let Some(allowed) = safe_permission_prefix(allowed) else {
                return false;
            };
            requested == allowed
                || requested
                    .strip_prefix(allowed)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        })
    }
}

/// Persisted authorization assigned when an enrollment invite is redeemed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceAuthorization {
    pub device_name: String,
    pub permissions: DevicePermissions,
}

/// Device identity to enrollment-authority mapping used when minting sessions.
/// Unknown devices intentionally have no implicit default permissions.
#[derive(Clone, Default)]
pub struct DeviceAuthorizationStore {
    authorizations: Arc<RwLock<HashMap<String, DeviceAuthorization>>>,
}

fn validate_device_authorizations(
    authorizations: &HashMap<String, DeviceAuthorization>,
) -> anyhow::Result<()> {
    for (device_id, authorization) in authorizations {
        anyhow::ensure!(
            !device_id.is_empty()
                && !device_id.chars().any(char::is_control)
                && !authorization.device_name.is_empty()
                && !authorization.device_name.chars().any(char::is_control),
            "device authorization contains an invalid device identity"
        );
        anyhow::ensure!(
            authorization
                .permissions
                .allowed_prefixes
                .iter()
                .all(|prefix| safe_permission_prefix(prefix).is_some()),
            "device authorization contains an invalid allowed prefix"
        );
    }
    Ok(())
}

impl DeviceAuthorizationStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn authorize(
        &self,
        device_id: impl Into<String>,
        device_name: impl Into<String>,
        permissions: DevicePermissions,
    ) {
        self.authorizations.write().await.insert(
            device_id.into(),
            DeviceAuthorization {
                device_name: device_name.into(),
                permissions,
            },
        );
    }

    pub async fn get(&self, device_id: &str) -> Option<DeviceAuthorization> {
        self.authorizations.read().await.get(device_id).cloned()
    }

    pub async fn revoke(&self, device_id: &str) {
        self.authorizations.write().await.remove(device_id);
    }

    pub async fn count(&self) -> usize {
        self.authorizations.read().await.len()
    }

    /// Atomically persist the authorization map with owner-only permissions.
    pub async fn save_to_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;

        let data = {
            let authorizations = self.authorizations.read().await;
            validate_device_authorizations(&authorizations)?;
            serde_json::to_vec_pretty(&*authorizations)?
        };
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("authorization path has no parent"))?;
        tokio::fs::create_dir_all(parent).await?;
        let tmp = parent.join(format!(
            ".{}.tmp-{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("device-authorizations"),
            uuid::Uuid::new_v4()
        ));
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let mut file = options.open(&tmp).await?;
        file.write_all(&data).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&tmp, path).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }

    pub async fn load_from_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let metadata = tokio::fs::symlink_metadata(path).await?;
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "device authorization store must be a regular, non-symlink file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            // SAFETY: geteuid has no preconditions and only reads process identity.
            let effective_uid = unsafe { libc::geteuid() };
            anyhow::ensure!(
                metadata.uid() == effective_uid,
                "device authorization store must be owned by daemon uid {effective_uid}"
            );
            anyhow::ensure!(
                metadata.nlink() == 1,
                "device authorization store must have exactly one hard link"
            );
            anyhow::ensure!(
                metadata.permissions().mode() & 0o077 == 0,
                "device authorization store must be mode 0600 or stricter"
            );
        }
        let data = tokio::fs::read(path).await?;
        let loaded: HashMap<String, DeviceAuthorization> = serde_json::from_slice(&data)?;
        validate_device_authorizations(&loaded)?;
        *self.authorizations.write().await = loaded;
        Ok(())
    }
}

/// Rate limiting configuration.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum failed attempts before lockout.
    pub max_attempts: u32,
    /// Base lockout duration after exceeding max_attempts.
    pub lockout_duration: Duration,
    /// Backoff multiplier (lockout doubles each successive breach).
    pub backoff_multiplier: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            lockout_duration: Duration::minutes(5),
            backoff_multiplier: 2,
        }
    }
}

/// Per-device attempt tracking for brute-force protection.
#[derive(Debug, Clone)]
struct DeviceAttempts {
    failures: u32,
    last_failure: DateTime<Utc>,
    lockout_until: Option<DateTime<Utc>>,
    consecutive_lockouts: u32,
}

/// Thread-safe rate limiter for auth attempts.
#[derive(Clone)]
pub struct RateLimiter {
    config: RateLimitConfig,
    attempts: Arc<RwLock<HashMap<String, DeviceAttempts>>>,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            attempts: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Check if a device is currently rate-limited. Returns remaining lockout seconds if limited.
    pub async fn check(&self, device_id: &str) -> Option<i64> {
        let attempts = self.attempts.read().await;
        if let Some(da) = attempts.get(device_id) {
            if let Some(until) = da.lockout_until {
                let remaining = (until - Utc::now()).num_seconds();
                if remaining > 0 {
                    return Some(remaining);
                }
            }
        }
        None
    }

    /// Record a failed auth attempt. Returns lockout seconds if the device is now locked out.
    pub async fn record_failure(&self, device_id: &str) -> Option<i64> {
        let mut attempts = self.attempts.write().await;
        let da = attempts
            .entry(device_id.to_string())
            .or_insert(DeviceAttempts {
                failures: 0,
                last_failure: Utc::now(),
                lockout_until: None,
                consecutive_lockouts: 0,
            });
        da.failures += 1;
        da.last_failure = Utc::now();

        if da.failures >= self.config.max_attempts {
            let multiplier = self.config.backoff_multiplier.pow(da.consecutive_lockouts);
            let lockout = self.config.lockout_duration * multiplier as i32;
            da.lockout_until = Some(Utc::now() + lockout);
            da.consecutive_lockouts += 1;
            tracing::warn!(
                device_id,
                failures = da.failures,
                lockout_secs = lockout.num_seconds(),
                "device locked out due to failed auth attempts"
            );
            Some(lockout.num_seconds())
        } else {
            None
        }
    }

    /// Clear attempt tracking for a device (call on successful auth).
    pub async fn clear(&self, device_id: &str) {
        self.attempts.write().await.remove(device_id);
    }

    /// Number of devices currently tracked.
    pub async fn tracked_count(&self) -> usize {
        self.attempts.read().await.len()
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

    /// Save active (non-expired) sessions to a JSON file.
    pub async fn save_to_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;

        // Only persist valid sessions
        let sessions = self.sessions.read().await;
        let valid: HashMap<String, Session> = sessions
            .iter()
            .filter(|(_, s)| s.is_valid())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        drop(sessions);
        let data = serde_json::to_vec_pretty(&valid)?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("session path has no parent"))?;
        tokio::fs::create_dir_all(parent).await?;
        let tmp = parent.join(format!(
            ".{}.tmp-{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("sessions"),
            uuid::Uuid::new_v4()
        ));
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let mut file = options.open(&tmp).await?;
        file.write_all(&data).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&tmp, path).await?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        tracing::debug!(path = %path.display(), count = valid.len(), "saved sessions (mode 0600)");
        Ok(())
    }

    /// Load sessions from a JSON file, discarding any that have expired.
    pub async fn load_from_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let metadata = tokio::fs::symlink_metadata(path).await?;
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "session store must be a regular, non-symlink file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            // SAFETY: geteuid has no preconditions and only reads process identity.
            let effective_uid = unsafe { libc::geteuid() };
            anyhow::ensure!(
                metadata.uid() == effective_uid,
                "session store must be owned by daemon uid {effective_uid}"
            );
            anyhow::ensure!(
                metadata.nlink() == 1,
                "session store must have one hard link"
            );
            anyhow::ensure!(
                metadata.permissions().mode() & 0o077 == 0,
                "session store must be mode 0600 or stricter"
            );
        }
        let data = tokio::fs::read_to_string(path).await?;
        let loaded: HashMap<String, Session> = serde_json::from_str(&data)?;
        let mut sessions = self.sessions.write().await;
        let mut device_sessions = self.device_sessions.write().await;
        let mut count = 0;
        for (token, session) in loaded {
            if session.is_valid() {
                device_sessions.insert(session.device_id.clone(), token.clone());
                sessions.insert(token, session);
                count += 1;
            }
        }
        tracing::info!(path = %path.display(), count, "loaded sessions");
        Ok(())
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
        let mut session = Session::new("device-1", "laptop", "totp");
        // Set expiry to the past to guarantee expired state
        session.expires_at = Some(chrono::Utc::now() - chrono::Duration::seconds(1));
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
        assert!(restricted.can_access_prefix("git"));
        assert!(!restricted.can_access_prefix("git-malicious"));
        assert!(!restricted.can_access_prefix("git/../secrets"));
        assert!(!restricted.can_access_prefix("secrets/keys"));
    }

    #[tokio::test]
    async fn device_authorization_store_roundtrips_scoped_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-authorizations.json");
        let store = DeviceAuthorizationStore::new();
        let permissions = DevicePermissions {
            can_mount: true,
            can_push: false,
            can_pull: true,
            can_admin: false,
            allowed_prefixes: vec!["git/team-a".into()],
        };
        store
            .authorize("device-a", "read-only laptop", permissions.clone())
            .await;
        store.save_to_file(&path).await.unwrap();

        let restored = DeviceAuthorizationStore::new();
        restored.load_from_file(&path).await.unwrap();
        let authorization = restored.get("device-a").await.unwrap();
        assert_eq!(authorization.device_name, "read-only laptop");
        assert!(!authorization.permissions.can_push);
        assert_eq!(
            authorization.permissions.allowed_prefixes,
            permissions.allowed_prefixes
        );
        assert!(restored.get("unknown-device").await.is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn device_authorization_store_rejects_unsafe_file_and_scope() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-authorizations.json");
        std::fs::write(
            &path,
            r#"{"device-a":{"device_name":"laptop","permissions":{"can_mount":true,"can_push":true,"can_pull":true,"can_admin":false,"allowed_prefixes":["../escape"]}}}"#,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let store = DeviceAuthorizationStore::new();
        assert!(store.load_from_file(&path).await.is_err());

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.load_from_file(&path).await.is_err());

        let symlink_path = dir.path().join("authorizations-link.json");
        std::os::unix::fs::symlink(&path, &symlink_path).unwrap();
        assert!(store.load_from_file(&symlink_path).await.is_err());
    }

    #[tokio::test]
    async fn test_rate_limiter_allows_under_threshold() {
        let limiter = RateLimiter::new(RateLimitConfig::default());
        // 4 failures (under default max of 5) should not lock out
        for _ in 0..4 {
            assert!(limiter.record_failure("device-1").await.is_none());
        }
        assert!(limiter.check("device-1").await.is_none());
    }

    #[tokio::test]
    async fn test_rate_limiter_locks_at_threshold() {
        let limiter = RateLimiter::new(RateLimitConfig {
            max_attempts: 3,
            lockout_duration: Duration::minutes(1),
            backoff_multiplier: 2,
        });
        // 3 failures should trigger lockout
        assert!(limiter.record_failure("device-1").await.is_none());
        assert!(limiter.record_failure("device-1").await.is_none());
        let lockout = limiter.record_failure("device-1").await;
        assert!(lockout.is_some());
        assert!(lockout.unwrap() > 0);

        // Should be locked out
        assert!(limiter.check("device-1").await.is_some());
    }

    #[tokio::test]
    async fn test_rate_limiter_clear_resets() {
        let limiter = RateLimiter::new(RateLimitConfig {
            max_attempts: 2,
            lockout_duration: Duration::minutes(1),
            backoff_multiplier: 2,
        });
        limiter.record_failure("device-1").await;
        limiter.record_failure("device-1").await;
        assert!(limiter.check("device-1").await.is_some());

        // Clear on successful auth
        limiter.clear("device-1").await;
        assert!(limiter.check("device-1").await.is_none());
        assert_eq!(limiter.tracked_count().await, 0);
    }

    #[tokio::test]
    async fn test_rate_limiter_independent_devices() {
        let limiter = RateLimiter::new(RateLimitConfig {
            max_attempts: 2,
            lockout_duration: Duration::minutes(1),
            backoff_multiplier: 2,
        });
        // Lock out device-1
        limiter.record_failure("device-1").await;
        limiter.record_failure("device-1").await;
        assert!(limiter.check("device-1").await.is_some());

        // device-2 should be unaffected
        assert!(limiter.check("device-2").await.is_none());
    }
}
