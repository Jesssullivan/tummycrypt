//! Extended-ACL checks for trusted local mutation boundaries.
//!
//! Unix mode bits alone do not prove that a path is unwritable by another
//! principal: POSIX ACLs on Linux and NFSv4-style extended ACLs on macOS can
//! grant additional write rights. Registered-root and raw-Git mutation paths
//! call this module in addition to their uid/mode/symlink checks.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use anyhow::Context;
use anyhow::Result;
use std::fs::File;
use std::path::Path;

/// Fail closed when an extended ACL can grant write-like access that is not
/// represented by the path's ordinary Unix mode bits.
///
/// Linux takes the deliberately conservative route of rejecting any POSIX
/// access or default ACL. macOS must retain compatibility with the standard
/// deny-only ACL on user home directories, so it accepts deny-only/read-only
/// ACLs and rejects every allow entry carrying a write-like permission.
#[allow(clippy::needless_return)]
pub fn reject_write_grant_acl(path: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        return linux::reject_write_grant_acl(path);
    }

    #[cfg(target_os = "macos")]
    {
        return macos::reject_write_grant_acl(path);
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        anyhow::bail!(
            "extended ACL inspection is supported only on Linux and macOS: {}",
            path.display()
        );
    }
}

/// Descriptor-anchored variant of [`reject_write_grant_acl`].
///
/// Mutation code that already holds an `O_NOFOLLOW` directory capability must
/// not fall back to a pathname for its ACL decision: the pathname may have
/// been replaced since capture. `display_path` is used only in diagnostics.
#[allow(clippy::needless_return)]
pub fn reject_write_grant_acl_fd(file: &File, display_path: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        return linux::reject_write_grant_acl_fd(file, display_path);
    }

    #[cfg(target_os = "macos")]
    {
        return macos::reject_write_grant_acl_fd(file, display_path);
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = file;
        anyhow::bail!(
            "descriptor ACL inspection is supported only on Linux and macOS: {}",
            display_path.display()
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn c_path(path: &Path) -> Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;

    std::ffi::CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("ACL path contains NUL: {}", path.display()))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn metadata_identity(metadata: &std::fs::Metadata) -> (u64, u64, u32) {
    use std::os::unix::fs::MetadataExt;

    // Darwin exposes S_IFMT as u16, while Linux exposes it as u32.
    #[allow(clippy::unnecessary_cast)]
    let file_type = metadata.mode() & (libc::S_IFMT as u32);

    (
        metadata.dev(),
        metadata.ino(),
        file_type,
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn revalidate_path_identity(
    path: &Path,
    before: &std::fs::Metadata,
    operation: &str,
) -> Result<()> {
    let after = std::fs::symlink_metadata(path).with_context(|| {
        format!(
            "revalidating ACL path after {operation}: {}",
            path.display()
        )
    })?;
    anyhow::ensure!(
        metadata_identity(before) == metadata_identity(&after),
        "trusted path identity changed during {operation}: {}",
        path.display()
    );
    Ok(())
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{c_path, metadata_identity, revalidate_path_identity, Context, File, Path, Result};
    use std::os::fd::AsRawFd;

    const ACL_XATTRS: [&[u8]; 2] = [b"system.posix_acl_access\0", b"system.posix_acl_default\0"];

    #[derive(Debug, Eq, PartialEq)]
    enum ProbeOutcome {
        Absent,
        Present,
    }

    /// Only ENODATA proves the ACL is absent; every other errno fails closed.
    ///
    /// ENOTSUP/EOPNOTSUPP in particular must NOT be treated as "no ACL". It
    /// means the POSIX ACL surface could not be inspected here, not that no
    /// write-granting ACL exists:
    ///   - network filesystems (NFS/CIFS) report EOPNOTSUPP for
    ///     `system.posix_acl_*` while still enforcing server-side NFSv4/rich
    ///     ACLs that can grant writes, and
    ///   - the Nix Linux build sandbox installs a seccomp filter that fails
    ///     the entire xattr syscall family with ENOTSUP (so an ACL applied
    ///     from outside the sandbox would be invisible to this probe).
    ///
    /// On local ACL-supporting filesystems (verified: XFS, kernel 6.19) the
    /// probes genuinely return ENODATA when no ACL is set, so fail-closed
    /// here does not reject ordinary trusted paths (TIN-2853).
    fn classify_probe_result(
        result: libc::ssize_t,
        errno: Option<i32>,
    ) -> std::result::Result<ProbeOutcome, ()> {
        if result >= 0 {
            return Ok(ProbeOutcome::Present);
        }
        if errno == Some(libc::ENODATA) {
            return Ok(ProbeOutcome::Absent);
        }
        Err(())
    }

    pub(super) fn reject_write_grant_acl(path: &Path) -> Result<()> {
        // Establish that the path exists before interpreting ENODATA below as
        // "no ACL". Callers also perform an lstat-style topology check, but the
        // ACL boundary remains fail-closed when used independently.
        let before = std::fs::symlink_metadata(path)
            .with_context(|| format!("inspecting ACL path metadata: {}", path.display()))?;
        // Linux symlink permissions are ignored and POSIX ACL xattrs are not
        // supported on the link inode. Topology callers still reject the
        // symlink itself; treating its inapplicable ACL surface as empty lets
        // them return that precise fail-closed topology error.
        if before.file_type().is_symlink() {
            revalidate_path_identity(path, &before, "symlink ACL classification")?;
            return Ok(());
        }
        let path_c = c_path(path)?;

        for name in ACL_XATTRS {
            // POSIX default ACLs apply only to directories. Linux reports
            // EOPNOTSUPP when probing system.posix_acl_default on a regular
            // file even when the filesystem fully supports access ACLs.
            if name == ACL_XATTRS[1] && !before.is_dir() {
                continue;
            }
            // SAFETY: both pointers reference NUL-terminated byte strings for
            // the duration of the call. A null value buffer with size zero is
            // the documented size/presence probe and does not write memory.
            let result = unsafe {
                libc::lgetxattr(
                    path_c.as_ptr(),
                    name.as_ptr().cast(),
                    std::ptr::null_mut(),
                    0,
                )
            };
            let error = std::io::Error::last_os_error();
            match classify_probe_result(result, error.raw_os_error()) {
                Ok(ProbeOutcome::Absent) => {}
                Ok(ProbeOutcome::Present) => {
                    let name = std::str::from_utf8(&name[..name.len() - 1])
                        .expect("static ACL xattr name is UTF-8");
                    anyhow::bail!(
                        "extended POSIX ACL {name:?} is outside the trusted mutation boundary: {}",
                        path.display()
                    );
                }
                Err(()) => {
                    return Err(error).with_context(|| {
                        format!(
                            "inspecting extended POSIX ACL on trusted path: {}",
                            path.display()
                        )
                    });
                }
            }
        }

        revalidate_path_identity(path, &before, "extended ACL inspection")?;
        Ok(())
    }

    pub(super) fn reject_write_grant_acl_fd(file: &File, display_path: &Path) -> Result<()> {
        let before = file.metadata().with_context(|| {
            format!(
                "inspecting descriptor ACL metadata: {}",
                display_path.display()
            )
        })?;

        for name in ACL_XATTRS {
            // See the pathname probe above: default ACLs are meaningful only
            // for directories, and Linux rejects this xattr on regular files.
            if name == ACL_XATTRS[1] && !before.is_dir() {
                continue;
            }
            // SAFETY: file owns a live descriptor and name is a static,
            // NUL-terminated xattr name. The null buffer is a size/presence
            // probe and cannot be written through.
            let result = unsafe {
                libc::fgetxattr(
                    file.as_raw_fd(),
                    name.as_ptr().cast(),
                    std::ptr::null_mut(),
                    0,
                )
            };
            let error = std::io::Error::last_os_error();
            match classify_probe_result(result, error.raw_os_error()) {
                Ok(ProbeOutcome::Absent) => {}
                Ok(ProbeOutcome::Present) => {
                    let name = std::str::from_utf8(&name[..name.len() - 1])
                        .expect("static ACL xattr name is UTF-8");
                    anyhow::bail!(
                        "extended POSIX ACL {name:?} is outside the trusted mutation boundary: {}",
                        display_path.display()
                    );
                }
                Err(()) => {
                    return Err(error).with_context(|| {
                        format!(
                            "inspecting extended POSIX ACL on trusted descriptor: {}",
                            display_path.display()
                        )
                    });
                }
            }
        }

        let after = file.metadata().with_context(|| {
            format!(
                "revalidating descriptor after ACL inspection: {}",
                display_path.display()
            )
        })?;
        anyhow::ensure!(
            metadata_identity(&before) == metadata_identity(&after),
            "trusted descriptor identity changed during ACL inspection: {}",
            display_path.display()
        );
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::{classify_probe_result, ProbeOutcome};

        #[test]
        fn linux_acl_probe_decision_is_fail_closed() {
            assert_eq!(classify_probe_result(0, None), Ok(ProbeOutcome::Present));
            assert_eq!(
                classify_probe_result(-1, Some(libc::ENODATA)),
                Ok(ProbeOutcome::Absent)
            );
            assert!(classify_probe_result(-1, Some(libc::EACCES)).is_err());
            // ENOTSUP is an undetermined ACL surface (seccomp-filtered xattr
            // in the Nix sandbox, NFS/CIFS with non-POSIX ACLs), never proof
            // of absence. It must stay fail-closed (TIN-2853).
            assert!(classify_probe_result(-1, Some(libc::EOPNOTSUPP)).is_err());
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{c_path, metadata_identity, revalidate_path_identity, Context, File, Path, Result};
    use libc::{c_char, c_int, c_void};
    use std::os::fd::AsRawFd;

    type Acl = *mut c_void;
    type AclEntry = *mut c_void;

    const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
    const ACL_FIRST_ENTRY: c_int = 0;
    const ACL_NEXT_ENTRY: c_int = -1;
    const ACL_EXTENDED_ALLOW: c_int = 1;
    const ACL_EXTENDED_DENY: c_int = 2;

    const ACL_WRITE_DATA: u64 = 1 << 2;
    const ACL_DELETE: u64 = 1 << 4;
    const ACL_APPEND_DATA: u64 = 1 << 5;
    const ACL_DELETE_CHILD: u64 = 1 << 6;
    const ACL_WRITE_ATTRIBUTES: u64 = 1 << 8;
    const ACL_WRITE_EXTATTRIBUTES: u64 = 1 << 10;
    const ACL_WRITE_SECURITY: u64 = 1 << 12;
    const ACL_CHANGE_OWNER: u64 = 1 << 13;
    const WRITE_LIKE_PERMISSIONS: u64 = ACL_WRITE_DATA
        | ACL_DELETE
        | ACL_APPEND_DATA
        | ACL_DELETE_CHILD
        | ACL_WRITE_ATTRIBUTES
        | ACL_WRITE_EXTATTRIBUTES
        | ACL_WRITE_SECURITY
        | ACL_CHANGE_OWNER;

    #[link(name = "System")]
    unsafe extern "C" {
        fn acl_get_link_np(path: *const c_char, acl_type: c_int) -> Acl;
        fn acl_get_fd_np(fd: c_int, acl_type: c_int) -> Acl;
        fn acl_get_entry(acl: Acl, entry_id: c_int, entry: *mut AclEntry) -> c_int;
        fn acl_get_tag_type(entry: AclEntry, tag_type: *mut c_int) -> c_int;
        fn acl_get_permset_mask_np(entry: AclEntry, mask: *mut u64) -> c_int;
        fn acl_free(object: *mut c_void) -> c_int;
    }

    struct OwnedAcl(Acl);

    impl Drop for OwnedAcl {
        fn drop(&mut self) {
            // SAFETY: the pointer was returned by acl_get_link_np and remains
            // uniquely owned by this guard. acl_free accepts all ACL objects.
            let _ = unsafe { acl_free(self.0) };
        }
    }

    fn allow_mask_has_write_right(mask: u64) -> bool {
        mask & WRITE_LIKE_PERMISSIONS != 0
    }

    fn validate_entry_policy(tag_type: c_int, mask: Option<u64>) -> std::result::Result<(), ()> {
        match (tag_type, mask) {
            (ACL_EXTENDED_DENY, _) => Ok(()),
            (ACL_EXTENDED_ALLOW, Some(mask)) if !allow_mask_has_write_right(mask) => Ok(()),
            _ => Err(()),
        }
    }

    #[derive(Debug, Eq, PartialEq)]
    enum EntryOutcome {
        Inspect,
        End,
    }

    fn classify_entry_status(
        status: c_int,
        errno: Option<c_int>,
        inspected_entry: bool,
    ) -> std::result::Result<EntryOutcome, ()> {
        if status == 0 {
            return Ok(EntryOutcome::Inspect);
        }
        if status == -1 && inspected_entry && errno == Some(libc::EINVAL) {
            return Ok(EntryOutcome::End);
        }
        Err(())
    }

    fn validate_acl(acl: Acl, display_path: &Path) -> Result<()> {
        let acl = OwnedAcl(acl);

        let mut entry_id = ACL_FIRST_ENTRY;
        let mut inspected_entry = false;
        loop {
            let mut entry: AclEntry = std::ptr::null_mut();
            // SAFETY: acl is live and entry points to writable storage for the
            // borrowed opaque entry pointer. The pointer is owned by the ACL.
            let status = unsafe { acl_get_entry(acl.0, entry_id, &mut entry) };
            let error = std::io::Error::last_os_error();
            match classify_entry_status(status, error.raw_os_error(), inspected_entry) {
                Ok(EntryOutcome::End) => break,
                Ok(EntryOutcome::Inspect) => {}
                Err(()) => {
                    return Err(error).with_context(|| {
                        format!("enumerating extended ACL: {}", display_path.display())
                    });
                }
            }
            anyhow::ensure!(
                !entry.is_null(),
                "extended ACL returned a null entry: {}",
                display_path.display()
            );
            inspected_entry = true;

            let mut tag_type = 0;
            // SAFETY: entry is borrowed from the live ACL and tag_type is valid
            // writable storage for the C enum value.
            if unsafe { acl_get_tag_type(entry, &mut tag_type) } != 0 {
                return Err(std::io::Error::last_os_error()).with_context(|| {
                    format!("reading extended ACL tag: {}", display_path.display())
                });
            }

            match tag_type {
                ACL_EXTENDED_DENY => {
                    validate_entry_policy(tag_type, None).map_err(|()| {
                        anyhow::anyhow!(
                            "unsupported extended ACL deny entry on trusted path: {}",
                            display_path.display()
                        )
                    })?;
                }
                ACL_EXTENDED_ALLOW => {
                    let mut mask = 0u64;
                    // SAFETY: entry is live and mask points to writable u64
                    // storage matching Darwin's acl_permset_mask_t.
                    if unsafe { acl_get_permset_mask_np(entry, &mut mask) } != 0 {
                        return Err(std::io::Error::last_os_error()).with_context(|| {
                            format!(
                                "reading extended ACL permissions: {}",
                                display_path.display()
                            )
                        });
                    }
                    anyhow::ensure!(
                        validate_entry_policy(tag_type, Some(mask)).is_ok(),
                        "extended ACL grants write-like access outside the trusted mutation boundary: {}",
                        display_path.display()
                    );
                }
                other => anyhow::bail!(
                    "unsupported extended ACL tag {other} on trusted path: {}",
                    display_path.display()
                ),
            }

            entry_id = ACL_NEXT_ENTRY;
        }

        Ok(())
    }

    pub(super) fn reject_write_grant_acl(path: &Path) -> Result<()> {
        // On macOS acl_get_link_np reports ENOENT both for a vanished pathname
        // and for an extant path with no extended ACL. Confirm existence first;
        // a same-principal replacement remains inside the documented trust
        // boundary, while cross-principal replacement is fenced by callers.
        let before = std::fs::symlink_metadata(path)
            .with_context(|| format!("inspecting ACL path metadata: {}", path.display()))?;
        let path_c = c_path(path)?;

        // SAFETY: path_c is NUL-terminated and valid for the call. The returned
        // opaque ACL, when non-null, is released exactly once by OwnedAcl.
        let acl = unsafe { acl_get_link_np(path_c.as_ptr(), ACL_TYPE_EXTENDED) };
        if acl.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOENT) {
                revalidate_path_identity(path, &before, "extended ACL inspection")?;
                return Ok(());
            }
            return Err(error)
                .with_context(|| format!("inspecting extended ACL: {}", path.display()));
        }

        validate_acl(acl, path)?;

        revalidate_path_identity(path, &before, "extended ACL inspection")?;
        Ok(())
    }

    pub(super) fn reject_write_grant_acl_fd(file: &File, display_path: &Path) -> Result<()> {
        let before = file.metadata().with_context(|| {
            format!(
                "inspecting descriptor ACL metadata: {}",
                display_path.display()
            )
        })?;

        // SAFETY: file owns a live descriptor. The returned opaque ACL, when
        // non-null, is released exactly once by validate_acl's OwnedAcl.
        let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
        if acl.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOENT) {
                let after = file.metadata().with_context(|| {
                    format!(
                        "revalidating descriptor after ACL inspection: {}",
                        display_path.display()
                    )
                })?;
                anyhow::ensure!(
                    metadata_identity(&before) == metadata_identity(&after),
                    "trusted descriptor identity changed during ACL inspection: {}",
                    display_path.display()
                );
                return Ok(());
            }
            return Err(error).with_context(|| {
                format!(
                    "inspecting extended ACL on trusted descriptor: {}",
                    display_path.display()
                )
            });
        }

        validate_acl(acl, display_path)?;
        let after = file.metadata().with_context(|| {
            format!(
                "revalidating descriptor after ACL inspection: {}",
                display_path.display()
            )
        })?;
        anyhow::ensure!(
            metadata_identity(&before) == metadata_identity(&after),
            "trusted descriptor identity changed during ACL inspection: {}",
            display_path.display()
        );
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::{
            allow_mask_has_write_right, classify_entry_status, validate_entry_policy, EntryOutcome,
            ACL_APPEND_DATA, ACL_CHANGE_OWNER, ACL_DELETE, ACL_DELETE_CHILD, ACL_EXTENDED_ALLOW,
            ACL_EXTENDED_DENY, ACL_WRITE_ATTRIBUTES, ACL_WRITE_DATA, ACL_WRITE_EXTATTRIBUTES,
            ACL_WRITE_SECURITY,
        };

        fn set_acl(path: &std::path::Path, spec: &str) {
            let output = std::process::Command::new("/bin/chmod")
                .args(["+a", spec])
                .arg(path)
                .output()
                .expect("run macOS chmod ACL fixture");
            assert!(
                output.status.success(),
                "chmod +a failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        #[test]
        fn write_like_permission_mask_is_conservative() {
            for permission in [
                ACL_WRITE_DATA,
                ACL_DELETE,
                ACL_APPEND_DATA,
                ACL_DELETE_CHILD,
                ACL_WRITE_ATTRIBUTES,
                ACL_WRITE_EXTATTRIBUTES,
                ACL_WRITE_SECURITY,
                ACL_CHANGE_OWNER,
            ] {
                assert!(allow_mask_has_write_right(permission));
                assert!(validate_entry_policy(ACL_EXTENDED_ALLOW, Some(permission)).is_err());
            }
            assert!(!allow_mask_has_write_right(1 << 1)); // read/list only
            assert!(validate_entry_policy(ACL_EXTENDED_ALLOW, Some(1 << 1)).is_ok());
            assert!(validate_entry_policy(ACL_EXTENDED_DENY, Some(ACL_DELETE)).is_ok());
            assert!(validate_entry_policy(99, Some(0)).is_err());
        }

        #[test]
        fn darwin_acl_entry_status_zero_means_inspect() {
            assert_eq!(
                classify_entry_status(0, None, false),
                Ok(EntryOutcome::Inspect)
            );
        }

        #[test]
        fn darwin_acl_entry_status_einval_ends_only_after_an_entry() {
            assert_eq!(
                classify_entry_status(-1, Some(libc::EINVAL), true),
                Ok(EntryOutcome::End)
            );
            assert!(classify_entry_status(-1, Some(libc::EINVAL), false).is_err());
            assert!(classify_entry_status(-1, Some(libc::EIO), true).is_err());
        }

        #[test]
        fn darwin_real_acl_accepts_deny_and_rejects_write_allow() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("acl-entry");
            std::fs::write(&path, b"fixture").unwrap();

            set_acl(&path, "everyone deny delete");
            super::reject_write_grant_acl(&path).expect("deny-only ACL remains trusted");

            let output = std::process::Command::new("/bin/chmod")
                .arg("-N")
                .arg(&path)
                .output()
                .expect("clear macOS ACL fixture");
            assert!(
                output.status.success(),
                "chmod -N failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );

            set_acl(&path, "everyone allow delete");
            let error = super::reject_write_grant_acl(&path)
                .expect_err("write-grant ALLOW ACL must fail closed");
            assert!(error.to_string().contains("write-like access"), "{error:#}");

            let file = std::fs::File::open(&path).unwrap();
            let error = super::reject_write_grant_acl_fd(&file, &path)
                .expect_err("descriptor ACL inspection must reject write-grant ALLOW ACL");
            assert!(error.to_string().contains("write-like access"), "{error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{reject_write_grant_acl, reject_write_grant_acl_fd};

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn ordinary_private_temp_path_has_no_write_grant_acl() {
        let temp = tempfile::tempdir().unwrap();
        reject_write_grant_acl(temp.path()).unwrap();
        let directory = std::fs::File::open(temp.path()).unwrap();
        reject_write_grant_acl_fd(&directory, temp.path()).unwrap();

        let regular_path = temp.path().join("state.json");
        std::fs::write(&regular_path, b"{}").unwrap();
        reject_write_grant_acl(&regular_path).unwrap();
        let regular_file = std::fs::File::open(&regular_path).unwrap();
        reject_write_grant_acl_fd(&regular_file, &regular_path).unwrap();
    }
}
