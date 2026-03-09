//! FUSE user-space library async version implementation.
//!
//! This is an improved rewrite of the FUSE user-space library to fully take advantage of Rust's
//! architecture.
//!
//! This library doesn't depend on `libfuse`, unless enable `unprivileged` feature, this feature
//! will support mount the filesystem without root permission by using `fusermount3` binary.
//!
//! # Features:
//!
//! - `file-lock`: enable POSIX file lock feature.
//! - `async-io-runtime`: use [async_io](https://docs.rs/async-io) and
//!   [async-global-executor](https://docs.rs/async-global-executor) to drive async io and task.
//! - `tokio-runtime`: use [tokio](https://docs.rs/tokio) runtime to drive async io and task.
//! - `unprivileged`: allow mount filesystem without root permission by using `fusermount3`.
//!
//! # Notes:
//!
//! You must enable `async-io-runtime` or `tokio-runtime` feature.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

#[cfg(any(
    all(target_os = "linux", feature = "unprivileged"),
    target_os = "macos"
))]
use std::io::{self, ErrorKind};
#[cfg(target_os = "macos")]
use std::path::Path;
#[cfg(any(
    all(target_os = "linux", feature = "unprivileged"),
    target_os = "macos"
))]
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use errno::Errno;
pub use helper::{mode_from_kind_and_perm, perm_from_mode_and_kind};
pub use mount_options::MountOptions;
use nix::sys::stat::mode_t;
use raw::abi::{
    fuse_setattr_in, FATTR_ATIME, FATTR_ATIME_NOW, FATTR_CTIME, FATTR_GID, FATTR_LOCKOWNER,
    FATTR_MODE, FATTR_MTIME, FATTR_MTIME_NOW, FATTR_SIZE, FATTR_UID,
};
#[cfg(target_os = "macos")]
use raw::abi::{FATTR_BKUPTIME, FATTR_CHGTIME, FATTR_CRTIME, FATTR_FLAGS};

mod errno;
mod helper;
mod mount_options;
pub mod notify;
pub mod path;
pub mod raw;

/// Filesystem Inode.
pub type Inode = u64;

/// pre-defined Result, the Err type is [`Errno`].
pub type Result<T> = std::result::Result<T, Errno>;

/// File types
#[derive(Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub enum FileType {
    /// Named pipe (S_IFIFO)
    NamedPipe,
    /// Character device (S_IFCHR)
    CharDevice,
    /// Block device (S_IFBLK)
    BlockDevice,
    /// Directory (S_IFDIR)
    Directory,
    /// Regular file (S_IFREG)
    RegularFile,
    /// Symbolic link (S_IFLNK)
    Symlink,
    /// Unix domain socket (S_IFSOCK)
    Socket,
}

impl FileType {
    /// convert [`FileType`] into [`mode_t`]
    pub const fn const_into_mode_t(self) -> mode_t {
        match self {
            FileType::NamedPipe => libc::S_IFIFO,
            FileType::CharDevice => libc::S_IFCHR,
            FileType::BlockDevice => libc::S_IFBLK,
            FileType::Directory => libc::S_IFDIR,
            FileType::RegularFile => libc::S_IFREG,
            FileType::Symlink => libc::S_IFLNK,
            FileType::Socket => libc::S_IFSOCK,
        }
    }
}

impl From<FileType> for mode_t {
    fn from(kind: FileType) -> Self {
        kind.const_into_mode_t()
    }
}

/// the setattr argument.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct SetAttr {
    /// set file or directory mode.
    pub mode: Option<mode_t>,
    /// set file or directory uid.
    pub uid: Option<u32>,
    /// set file or directory gid.
    pub gid: Option<u32>,
    /// set file or directory size.
    pub size: Option<u64>,
    /// the lock_owner argument.
    pub lock_owner: Option<u64>,
    /// set file or directory atime.
    pub atime: Option<Timestamp>,
    /// set file or directory mtime.
    pub mtime: Option<Timestamp>,
    /// set file or directory ctime.
    pub ctime: Option<Timestamp>,
    #[cfg(target_os = "macos")]
    pub crtime: Option<Timestamp>,
    #[cfg(target_os = "macos")]
    pub chgtime: Option<Timestamp>,
    #[cfg(target_os = "macos")]
    pub bkuptime: Option<Timestamp>,
    #[cfg(target_os = "macos")]
    pub flags: Option<u32>,
}

/// Helper for constructing Timestamps from fuse_setattr_in, which sign-casts
/// the seconds.
macro_rules! fsai2ts {
    ( $secs: expr, $nsecs: expr) => {
        Some(Timestamp::new($secs as i64, $nsecs))
    };
}

impl From<&fuse_setattr_in> for SetAttr {
    fn from(setattr_in: &fuse_setattr_in) -> Self {
        let mut set_attr = Self::default();

        if setattr_in.valid & FATTR_MODE > 0 {
            set_attr.mode = Some(setattr_in.mode as mode_t);
        }

        if setattr_in.valid & FATTR_UID > 0 {
            set_attr.uid = Some(setattr_in.uid);
        }

        if setattr_in.valid & FATTR_GID > 0 {
            set_attr.gid = Some(setattr_in.gid);
        }

        if setattr_in.valid & FATTR_SIZE > 0 {
            set_attr.size = Some(setattr_in.size);
        }

        if setattr_in.valid & FATTR_ATIME > 0 {
            set_attr.atime = fsai2ts!(setattr_in.atime, setattr_in.atimensec);
        }

        if setattr_in.valid & FATTR_ATIME_NOW > 0 {
            set_attr.atime = Some(SystemTime::now().into());
        }

        if setattr_in.valid & FATTR_MTIME > 0 {
            set_attr.mtime = fsai2ts!(setattr_in.mtime, setattr_in.mtimensec);
        }

        if setattr_in.valid & FATTR_MTIME_NOW > 0 {
            set_attr.mtime = Some(SystemTime::now().into());
        }

        if setattr_in.valid & FATTR_LOCKOWNER > 0 {
            set_attr.lock_owner = Some(setattr_in.lock_owner);
        }

        if setattr_in.valid & FATTR_CTIME > 0 {
            set_attr.ctime = fsai2ts!(setattr_in.ctime, setattr_in.ctimensec);
        }

        #[cfg(target_os = "macos")]
        if setattr_in.valid & FATTR_CRTIME > 0 {
            set_attr.ctime = fsai2ts!(setattr_in.crtime, setattr_in.crtimensec);
        }

        #[cfg(target_os = "macos")]
        if setattr_in.valid & FATTR_CHGTIME > 0 {
            set_attr.ctime = fsai2ts!(setattr_in.chgtime, setattr_in.chgtimensec);
        }

        #[cfg(target_os = "macos")]
        if setattr_in.valid & FATTR_BKUPTIME > 0 {
            set_attr.ctime = fsai2ts!(setattr_in.bkuptime, setattr_in.bkuptimensec);
        }

        #[cfg(target_os = "macos")]
        if setattr_in.valid & FATTR_FLAGS > 0 {
            set_attr.flags = Some(setattr_in.flags);
        }

        set_attr
    }
}

/// A file's timestamp, according to FUSE.
///
/// Nearly the same as a `libc::timespec`, except for the width of the nsec
/// field.
// Could implement From for Duration, and/or libc::timespec, if desired
#[derive(Debug, Clone, Copy, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct Timestamp {
    pub sec: i64,
    pub nsec: u32,
}

impl Timestamp {
    /// Create a new timestamp from its component parts.
    ///
    /// `nsec` should be less than 1_000_000_000.
    pub fn new(sec: i64, nsec: u32) -> Self {
        Timestamp { sec, nsec }
    }
}

impl From<SystemTime> for Timestamp {
    fn from(t: SystemTime) -> Self {
        let d = t
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0));
        Timestamp {
            sec: d.as_secs().try_into().unwrap_or(i64::MAX),
            nsec: d.subsec_nanos(),
        }
    }
}

#[cfg(all(target_os = "linux", feature = "unprivileged"))]
fn find_fusermount3() -> io::Result<PathBuf> {
    which::which("fusermount3").map_err(|err| {
        io::Error::new(
            ErrorKind::Other,
            format!("find fusermount3 binary failed {err:?}"),
        )
    })
}

#[cfg(target_os = "macos")]
fn find_macfuse_mount() -> io::Result<PathBuf> {
    if Path::new("/Library/Filesystems/macfuse.fs/Contents/Resources/mount_macfuse").exists() {
        Ok(PathBuf::from(
            "/Library/Filesystems/macfuse.fs/Contents/Resources/mount_macfuse",
        ))
    } else {
        Err(io::Error::new(
            ErrorKind::NotFound,
            "macfuse mount binary not found, Please install macfuse first.",
        ))
    }
}

/// FUSE-T support: NFS-loopback FUSE implementation for macOS.
///
/// FUSE-T provides a libfuse-compatible C API that internally translates
/// between NFS v4 and the FUSE protocol. The fd returned by fuse_chan_fd()
/// speaks standard FUSE protocol, so BlockFuseConnection works unchanged.
#[cfg(all(target_os = "macos", feature = "fuse-t"))]
pub(crate) mod fuse_t {
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::io;
    use std::os::unix::io::RawFd;
    use std::path::Path;

    use tracing::{debug, warn};

    // Opaque FUSE-T types
    type FuseChan = c_void;
    type FuseArgs = c_void;

    /// Dynamically load libfuse-t and mount via its C API.
    ///
    /// Returns the raw fd that speaks FUSE protocol, plus an opaque handle
    /// that must be kept alive (dropping it would tear down the NFS server).
    pub(crate) fn fuse_t_mount(mountpoint: &Path) -> io::Result<(RawFd, FuseTHandle)> {
        let lib_path = if Path::new("/usr/local/lib/libfuse-t.dylib").exists() {
            "/usr/local/lib/libfuse-t.dylib"
        } else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "libfuse-t.dylib not found at /usr/local/lib/libfuse-t.dylib",
            ));
        };

        debug!("loading FUSE-T from {}", lib_path);

        let lib_cstr = CString::new(lib_path).unwrap();

        // dlopen the library
        let lib = unsafe { libc::dlopen(lib_cstr.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if lib.is_null() {
            let err = unsafe { std::ffi::CStr::from_ptr(libc::dlerror()) };
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("dlopen libfuse-t failed: {}", err.to_string_lossy()),
            ));
        }

        // Resolve fuse_mount(const char *mountpoint, struct fuse_args *args) -> fuse_chan*
        let fuse_mount_sym = unsafe {
            libc::dlsym(lib, b"fuse_mount\0".as_ptr() as *const c_char)
        };
        if fuse_mount_sym.is_null() {
            unsafe { libc::dlclose(lib); }
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "dlsym fuse_mount failed in libfuse-t",
            ));
        }

        // Resolve fuse_chan_fd(struct fuse_chan *ch) -> int
        let fuse_chan_fd_sym = unsafe {
            libc::dlsym(lib, b"fuse_chan_fd\0".as_ptr() as *const c_char)
        };
        if fuse_chan_fd_sym.is_null() {
            unsafe { libc::dlclose(lib); }
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "dlsym fuse_chan_fd failed in libfuse-t",
            ));
        }

        // Resolve fuse_unmount(const char *mountpoint, struct fuse_chan *ch) -> void
        let fuse_unmount_sym = unsafe {
            libc::dlsym(lib, b"fuse_unmount\0".as_ptr() as *const c_char)
        };
        if fuse_unmount_sym.is_null() {
            unsafe { libc::dlclose(lib); }
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "dlsym fuse_unmount failed in libfuse-t",
            ));
        }

        let mountpoint_cstr = CString::new(
            mountpoint
                .to_str()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-UTF8 mountpoint"))?,
        )
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mountpoint contains NUL"))?;

        // Cast and call: fuse_mount(mountpoint, NULL)
        let fuse_mount_fn: unsafe extern "C" fn(*const c_char, *const FuseArgs) -> *mut FuseChan =
            unsafe { std::mem::transmute(fuse_mount_sym) };
        let chan = unsafe { fuse_mount_fn(mountpoint_cstr.as_ptr(), std::ptr::null()) };
        if chan.is_null() {
            unsafe { libc::dlclose(lib); }
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "fuse_mount returned NULL (FUSE-T mount failed)",
            ));
        }

        debug!("FUSE-T mount succeeded, extracting fd");

        // Cast and call: fuse_chan_fd(chan) -> int
        let fuse_chan_fd_fn: unsafe extern "C" fn(*mut FuseChan) -> c_int =
            unsafe { std::mem::transmute(fuse_chan_fd_sym) };
        let fd = unsafe { fuse_chan_fd_fn(chan) };
        if fd < 0 {
            // Unmount on error
            let fuse_unmount_fn: unsafe extern "C" fn(*const c_char, *mut FuseChan) =
                unsafe { std::mem::transmute(fuse_unmount_sym) };
            unsafe { fuse_unmount_fn(mountpoint_cstr.as_ptr(), chan); }
            unsafe { libc::dlclose(lib); }
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "fuse_chan_fd returned invalid fd",
            ));
        }

        // Dup the fd so we own an independent copy
        let dup_fd = unsafe { libc::dup(fd) };
        if dup_fd < 0 {
            let fuse_unmount_fn: unsafe extern "C" fn(*const c_char, *mut FuseChan) =
                unsafe { std::mem::transmute(fuse_unmount_sym) };
            unsafe { fuse_unmount_fn(mountpoint_cstr.as_ptr(), chan); }
            unsafe { libc::dlclose(lib); }
            return Err(io::Error::last_os_error());
        }

        debug!("FUSE-T fd={} (dup={}), NFS loopback active", fd, dup_fd);

        // Store cleanup handles
        let handle = FuseTHandle {
            lib,
            chan,
            fuse_unmount_sym,
            mountpoint: mountpoint_cstr,
        };

        Ok((dup_fd, handle))
    }

    /// Opaque handle that keeps the FUSE-T NFS server alive.
    /// On drop, calls fuse_unmount and dlcloses the library.
    pub(crate) struct FuseTHandle {
        lib: *mut c_void,
        chan: *mut FuseChan,
        fuse_unmount_sym: *mut c_void,
        mountpoint: CString,
    }

    // SAFETY: The handle is only used for cleanup (unmount + dlclose).
    // The pointers are opaque C handles that don't alias Rust data.
    unsafe impl Send for FuseTHandle {}
    unsafe impl Sync for FuseTHandle {}

    impl Drop for FuseTHandle {
        fn drop(&mut self) {
            debug!("FUSE-T cleanup: unmounting");
            let fuse_unmount_fn: unsafe extern "C" fn(*const c_char, *mut FuseChan) =
                unsafe { std::mem::transmute(self.fuse_unmount_sym) };
            unsafe {
                fuse_unmount_fn(self.mountpoint.as_ptr(), self.chan);
                libc::dlclose(self.lib);
            }
        }
    }
}
