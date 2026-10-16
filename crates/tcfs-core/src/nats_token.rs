//! NATS token-file resolution (OI-1003-Q177).
//!
//! The NATS server uses token auth. The token must never be rendered inline
//! into `config.toml`: Home Manager / NixOS render that file into the
//! world-readable Nix store. tcfs therefore reads the token from a file at
//! connect time (`sync.nats_token_file` or `TCFS_NATS_TOKEN_FILE`) and refuses
//! the legacy inline `sync.nats_token` value outright.
//!
//! The token value is held only in a [`NatsToken`], whose `Debug` output is
//! redacted and which implements neither `Display` nor `Serialize`. Error
//! values never carry token bytes.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use thiserror::Error;
use zeroize::Zeroizing;

use crate::config::SyncConfig;

/// Environment variable that overrides `sync.nats_token_file`.
pub const NATS_TOKEN_FILE_ENV: &str = "TCFS_NATS_TOKEN_FILE";

/// Upper bound on the token file size. NATS tokens are short; anything larger
/// is almost certainly the wrong file and is refused without being read.
pub const MAX_NATS_TOKEN_FILE_BYTES: u64 = 4096;

/// A NATS auth token read from a token file. Never printed.
pub struct NatsToken(Zeroizing<String>);

impl NatsToken {
    /// The token value, for handing to the NATS client. Do not log it.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for NatsToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NatsToken(<redacted>)")
    }
}

/// Where the token file path came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsTokenSource {
    /// `TCFS_NATS_TOKEN_FILE` environment variable.
    Env(PathBuf),
    /// `sync.nats_token_file` in config.
    Config(PathBuf),
}

impl NatsTokenSource {
    pub fn path(&self) -> &Path {
        match self {
            NatsTokenSource::Env(path) | NatsTokenSource::Config(path) => path,
        }
    }

    pub fn origin(&self) -> &'static str {
        match self {
            NatsTokenSource::Env(_) => NATS_TOKEN_FILE_ENV,
            NatsTokenSource::Config(_) => "sync.nats_token_file",
        }
    }
}

/// Why a NATS token could not be resolved. No variant carries token bytes.
#[derive(Debug, Error)]
pub enum NatsTokenError {
    #[error(
        "inline sync.nats_token is refused (it would be rendered into the world-readable \
         Nix store / config file); move the token into a 0600 file and set \
         sync.nats_token_file or {NATS_TOKEN_FILE_ENV}"
    )]
    InlineTokenRefused,

    #[error("NATS token file path must be absolute (or start with ~/): {path}")]
    RelativePath { path: PathBuf },

    #[error("NATS token file does not exist (or is a dangling symlink): {path}")]
    Missing { path: PathBuf },

    #[error("NATS token file could not be read: {path}: {source}")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("NATS token file is not a regular file: {path} (resolves to {resolved})")]
    NotRegularFile { path: PathBuf, resolved: PathBuf },

    #[error(
        "NATS token file resolves into the world-readable Nix store: {path} (resolves to {resolved})"
    )]
    InNixStore { path: PathBuf, resolved: PathBuf },

    #[error(
        "NATS token file is group/world accessible (mode {mode:04o}); run chmod 600 on {resolved}"
    )]
    InsecurePermissions {
        path: PathBuf,
        resolved: PathBuf,
        mode: u32,
    },

    #[error("NATS token file is larger than {max} bytes: {path}")]
    TooLarge { path: PathBuf, max: u64 },

    #[error("NATS token file is empty: {path}")]
    Empty { path: PathBuf },

    #[error(
        "NATS token file is malformed (not UTF-8, or contains whitespace/control characters \
         other than one trailing newline): {path}"
    )]
    Malformed { path: PathBuf },

    #[error("NATS token file changed while it was being opened: {path}")]
    Changed { path: PathBuf },
}

/// Decide which token file (if any) applies, without touching the filesystem.
///
/// Precedence:
/// 1. inline `sync.nats_token` set -> [`NatsTokenError::InlineTokenRefused`]
///    (always, even when a token file is also configured);
/// 2. `TCFS_NATS_TOKEN_FILE` (non-empty) -> [`NatsTokenSource::Env`];
/// 3. `sync.nats_token_file` -> [`NatsTokenSource::Config`];
/// 4. otherwise no token (unauthenticated connect).
pub fn nats_token_file_source(
    sync: &SyncConfig,
    env_value: Option<OsString>,
) -> Result<Option<NatsTokenSource>, NatsTokenError> {
    if sync.nats_token.is_some() {
        return Err(NatsTokenError::InlineTokenRefused);
    }
    if let Some(env) = env_value.filter(|value| !value.is_empty()) {
        return Ok(Some(NatsTokenSource::Env(PathBuf::from(env))));
    }
    Ok(sync
        .nats_token_file
        .as_ref()
        .map(|path| NatsTokenSource::Config(path.clone())))
}

/// Resolve the NATS token for `sync`, reading `TCFS_NATS_TOKEN_FILE` from the
/// process environment. Call at connect time; never cache the result in config.
pub fn resolve_nats_token(sync: &SyncConfig) -> Result<Option<NatsToken>, NatsTokenError> {
    let credentials_dir = std::env::var_os("CREDENTIALS_DIRECTORY")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    resolve_with_env(
        sync,
        std::env::var_os(NATS_TOKEN_FILE_ENV),
        credentials_dir.as_deref(),
    )
}

fn resolve_with_env(
    sync: &SyncConfig,
    env_value: Option<OsString>,
    credentials_dir: Option<&Path>,
) -> Result<Option<NatsToken>, NatsTokenError> {
    let Some(source) = nats_token_file_source(sync, env_value)? else {
        return Ok(None);
    };
    tracing::info!(
        origin = source.origin(),
        path = %source.path().display(),
        "NATS: reading auth token file"
    );
    read_nats_token_file_with(source.path(), credentials_dir).map(Some)
}

/// Read and validate a NATS token file.
///
/// Policy (OI-1003-Q177):
/// - the path must be absolute (a leading `~/` is expanded);
/// - symlinks are followed (sops-nix, agenix and Home Manager secrets are
///   symlinks into a generation directory), but every check applies to the
///   resolved target, a dangling link is [`NatsTokenError::Missing`], and a
///   target under `/nix/store` is refused;
/// - the target must be a regular file no larger than
///   [`MAX_NATS_TOKEN_FILE_BYTES`];
/// - on Unix, `mode & 0o077` must be 0. Exception: a direct child of
///   systemd's `$CREDENTIALS_DIRECTORY` may show group bits, because systemd
///   grants a non-root unit access through a POSIX ACL whose mask surfaces as
///   group bits; `mode & 0o007` must still be 0 there;
/// - exactly one trailing `\n` (or `\r\n`) is trimmed; the remainder must be
///   non-empty UTF-8 with no whitespace or control characters.
pub fn read_nats_token_file(path: &Path) -> Result<NatsToken, NatsTokenError> {
    let credentials_dir = std::env::var_os("CREDENTIALS_DIRECTORY")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    read_nats_token_file_with(path, credentials_dir.as_deref())
}

fn read_nats_token_file_with(
    path: &Path,
    credentials_dir: Option<&Path>,
) -> Result<NatsToken, NatsTokenError> {
    use std::io::Read;

    let display_path = path.to_path_buf();
    let expanded = crate::config::expand_tilde(path);
    if !expanded.is_absolute() {
        return Err(NatsTokenError::RelativePath { path: display_path });
    }
    if is_nix_store_path(&expanded) {
        return Err(NatsTokenError::InNixStore {
            path: display_path,
            resolved: expanded,
        });
    }

    let io_err = |source: std::io::Error| {
        if source.kind() == std::io::ErrorKind::NotFound {
            NatsTokenError::Missing {
                path: display_path.clone(),
            }
        } else {
            NatsTokenError::Unreadable {
                path: display_path.clone(),
                source,
            }
        }
    };

    let resolved = std::fs::canonicalize(&expanded).map_err(io_err)?;
    if is_nix_store_path(&resolved) {
        return Err(NatsTokenError::InNixStore {
            path: display_path,
            resolved,
        });
    }

    // stat before open so a FIFO or device is never opened (a FIFO open blocks).
    let before = std::fs::metadata(&resolved).map_err(io_err)?;
    if !before.is_file() {
        return Err(NatsTokenError::NotRegularFile {
            path: display_path,
            resolved,
        });
    }

    let file = std::fs::File::open(&resolved).map_err(io_err)?;
    let meta = file.metadata().map_err(io_err)?;
    if !meta.is_file() || !same_file(&before, &meta) {
        return Err(NatsTokenError::Changed { path: display_path });
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = meta.mode() & 0o7777;
        let in_credentials_dir = credentials_dir
            .and_then(|dir| std::fs::canonicalize(dir).ok())
            .is_some_and(|dir| resolved.parent() == Some(dir.as_path()));
        let forbidden = if in_credentials_dir { 0o007 } else { 0o077 };
        if mode & forbidden != 0 {
            return Err(NatsTokenError::InsecurePermissions {
                path: display_path,
                resolved,
                mode,
            });
        }
    }
    #[cfg(not(unix))]
    let _ = credentials_dir;

    if meta.len() > MAX_NATS_TOKEN_FILE_BYTES {
        return Err(NatsTokenError::TooLarge {
            path: display_path,
            max: MAX_NATS_TOKEN_FILE_BYTES,
        });
    }

    let mut buf = Zeroizing::new(Vec::with_capacity(meta.len() as usize));
    file.take(MAX_NATS_TOKEN_FILE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(io_err)?;
    if buf.len() as u64 > MAX_NATS_TOKEN_FILE_BYTES {
        return Err(NatsTokenError::TooLarge {
            path: display_path,
            max: MAX_NATS_TOKEN_FILE_BYTES,
        });
    }

    let mut len = buf.len();
    if buf[..len].ends_with(b"\n") {
        len -= 1;
        if buf[..len].ends_with(b"\r") {
            len -= 1;
        }
    }
    let body = &buf[..len];
    if body.is_empty() {
        return Err(NatsTokenError::Empty { path: display_path });
    }
    let Ok(text) = std::str::from_utf8(body) else {
        return Err(NatsTokenError::Malformed { path: display_path });
    };
    if text.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(NatsTokenError::Malformed { path: display_path });
    }
    Ok(NatsToken(Zeroizing::new(text.to_owned())))
}

#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    a.len() == b.len()
}

/// True when `path` lies under `/nix/store` (component-wise, so
/// `/nix/storefront` does not match).
pub fn is_nix_store_path(path: &Path) -> bool {
    path.starts_with("/nix/store")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const SENTINEL: &str = "OI1003-token-sentinel-do-not-log";

    #[cfg(unix)]
    fn write_token(dir: &Path, name: &str, contents: &[u8], mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(contents).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    fn sync_with(inline: Option<&str>, file: Option<&str>) -> SyncConfig {
        SyncConfig {
            nats_token: inline.map(str::to_string),
            nats_token_file: file.map(PathBuf::from),
            ..SyncConfig::default()
        }
    }

    #[test]
    fn source_none_when_nothing_configured() {
        let sync = sync_with(None, None);
        assert_eq!(nats_token_file_source(&sync, None).unwrap(), None);
    }

    #[test]
    fn source_inline_token_is_refused_even_with_file() {
        let sync = sync_with(Some(SENTINEL), Some("/run/secrets/nats-token"));
        let err = nats_token_file_source(&sync, Some("/env/token".into())).unwrap_err();
        assert!(matches!(err, NatsTokenError::InlineTokenRefused));
        assert!(!err.to_string().contains(SENTINEL));
        assert!(!format!("{err:?}").contains(SENTINEL));
    }

    #[test]
    fn source_env_overrides_config() {
        let sync = sync_with(None, Some("/config/token"));
        let got = nats_token_file_source(&sync, Some("/env/token".into())).unwrap();
        assert_eq!(got, Some(NatsTokenSource::Env(PathBuf::from("/env/token"))));
    }

    #[test]
    fn source_empty_env_is_unset() {
        let sync = sync_with(None, Some("/config/token"));
        let got = nats_token_file_source(&sync, Some(OsString::new())).unwrap();
        assert_eq!(
            got,
            Some(NatsTokenSource::Config(PathBuf::from("/config/token")))
        );
    }

    #[test]
    fn nix_store_detection() {
        assert!(is_nix_store_path(Path::new("/nix/store/abc-token")));
        assert!(is_nix_store_path(Path::new("/nix/store")));
        assert!(!is_nix_store_path(Path::new("/nix/storefront/token")));
        assert!(!is_nix_store_path(Path::new("/run/secrets/nats-token")));
    }

    #[test]
    fn token_debug_is_redacted() {
        let token = NatsToken(Zeroizing::new(SENTINEL.to_string()));
        assert_eq!(format!("{token:?}"), "NatsToken(<redacted>)");
        assert_eq!(token.expose_secret(), SENTINEL);
    }

    #[test]
    fn relative_path_is_refused() {
        let err = read_nats_token_file_with(Path::new("token"), None).unwrap_err();
        assert!(
            matches!(err, NatsTokenError::RelativePath { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn missing_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_nats_token_file_with(&dir.path().join("absent"), None).unwrap_err();
        assert!(matches!(err, NatsTokenError::Missing { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn reads_token_and_trims_exactly_one_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_token(dir.path(), "t", format!("{SENTINEL}\n").as_bytes(), 0o600);
        let token = read_nats_token_file_with(&path, None).unwrap();
        assert_eq!(token.expose_secret(), SENTINEL);

        let crlf = write_token(
            dir.path(),
            "crlf",
            format!("{SENTINEL}\r\n").as_bytes(),
            0o600,
        );
        assert_eq!(
            read_nats_token_file_with(&crlf, None)
                .unwrap()
                .expose_secret(),
            SENTINEL
        );

        let bare = write_token(dir.path(), "bare", SENTINEL.as_bytes(), 0o400);
        assert_eq!(
            read_nats_token_file_with(&bare, None)
                .unwrap()
                .expose_secret(),
            SENTINEL
        );
    }

    #[cfg(unix)]
    #[test]
    fn two_trailing_newlines_are_malformed_not_silently_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_token(dir.path(), "t", format!("{SENTINEL}\n\n").as_bytes(), 0o600);
        let err = read_nats_token_file_with(&path, None).unwrap_err();
        assert!(matches!(err, NatsTokenError::Malformed { .. }), "{err:?}");
        assert!(!err.to_string().contains(SENTINEL));
    }

    #[cfg(unix)]
    #[test]
    fn embedded_whitespace_is_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_token(dir.path(), "t", b"left right\n", 0o600);
        let err = read_nats_token_file_with(&path, None).unwrap_err();
        assert!(matches!(err, NatsTokenError::Malformed { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_is_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_token(dir.path(), "t", &[0xff, 0xfe, b'a'], 0o600);
        let err = read_nats_token_file_with(&path, None).unwrap_err();
        assert!(matches!(err, NatsTokenError::Malformed { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn empty_and_newline_only_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let empty = write_token(dir.path(), "empty", b"", 0o600);
        assert!(matches!(
            read_nats_token_file_with(&empty, None).unwrap_err(),
            NatsTokenError::Empty { .. }
        ));
        let nl = write_token(dir.path(), "nl", b"\n", 0o600);
        assert!(matches!(
            read_nats_token_file_with(&nl, None).unwrap_err(),
            NatsTokenError::Empty { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn group_or_world_accessible_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        for (name, mode) in [("g", 0o640), ("o", 0o604), ("gw", 0o620), ("x", 0o601)] {
            let path = write_token(dir.path(), name, SENTINEL.as_bytes(), mode);
            let err = read_nats_token_file_with(&path, None).unwrap_err();
            match err {
                NatsTokenError::InsecurePermissions { mode: got, .. } => {
                    assert_eq!(got & 0o777, mode, "{name}")
                }
                other => panic!("{name}: expected InsecurePermissions, got {other:?}"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn oversized_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let big = vec![b'a'; (MAX_NATS_TOKEN_FILE_BYTES + 1) as usize];
        let path = write_token(dir.path(), "big", &big, 0o600);
        let err = read_nats_token_file_with(&path, None).unwrap_err();
        assert!(matches!(err, NatsTokenError::TooLarge { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn directory_is_not_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_nats_token_file_with(dir.path(), None).unwrap_err();
        assert!(
            matches!(err, NatsTokenError::NotRegularFile { .. }),
            "{err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_private_regular_file_is_followed() {
        // sops-nix / agenix / Home Manager secrets are symlinks into a
        // generation directory; the policy is enforced on the target.
        let dir = tempfile::tempdir().unwrap();
        let target = write_token(dir.path(), "target", SENTINEL.as_bytes(), 0o400);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(
            read_nats_token_file_with(&link, None)
                .unwrap()
                .expose_secret(),
            SENTINEL
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_world_readable_target_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = write_token(dir.path(), "target", SENTINEL.as_bytes(), 0o644);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = read_nats_token_file_with(&link, None).unwrap_err();
        assert!(
            matches!(err, NatsTokenError::InsecurePermissions { .. }),
            "{err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &link).unwrap();
        let err = read_nats_token_file_with(&link, None).unwrap_err();
        assert!(matches!(err, NatsTokenError::Missing { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_device_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink("/dev/null", &link).unwrap();
        let err = read_nats_token_file_with(&link, None).unwrap_err();
        assert!(
            matches!(err, NatsTokenError::NotRegularFile { .. }),
            "{err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn systemd_credentials_dir_tolerates_group_acl_mask_but_not_world() {
        // systemd grants a non-root unit read access to $CREDENTIALS_DIRECTORY
        // files via a POSIX ACL; the ACL mask shows up as group bits (0440).
        let creds = tempfile::tempdir().unwrap();
        let acl_masked = write_token(creds.path(), "nats-token", SENTINEL.as_bytes(), 0o440);
        assert_eq!(
            read_nats_token_file_with(&acl_masked, Some(creds.path()))
                .unwrap()
                .expose_secret(),
            SENTINEL
        );
        // Outside the credentials dir the same mode is refused.
        assert!(matches!(
            read_nats_token_file_with(&acl_masked, None).unwrap_err(),
            NatsTokenError::InsecurePermissions { .. }
        ));
        let world = write_token(creds.path(), "world", SENTINEL.as_bytes(), 0o444);
        assert!(matches!(
            read_nats_token_file_with(&world, Some(creds.path())).unwrap_err(),
            NatsTokenError::InsecurePermissions { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn errors_never_contain_token_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_token(dir.path(), "t", format!("{SENTINEL}\n").as_bytes(), 0o644);
        let err = read_nats_token_file_with(&path, None).unwrap_err();
        assert!(!err.to_string().contains(SENTINEL));
        assert!(!format!("{err:?}").contains(SENTINEL));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_reads_config_file_and_refuses_inline() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_token(dir.path(), "t", format!("{SENTINEL}\n").as_bytes(), 0o600);
        let sync = sync_with(None, path.to_str());
        let token = resolve_with_env(&sync, None, None).unwrap().unwrap();
        assert_eq!(token.expose_secret(), SENTINEL);

        let inline = sync_with(Some(SENTINEL), None);
        assert!(matches!(
            resolve_with_env(&inline, None, None).unwrap_err(),
            NatsTokenError::InlineTokenRefused
        ));

        assert!(resolve_with_env(&sync_with(None, None), None, None)
            .unwrap()
            .is_none());
    }
}
