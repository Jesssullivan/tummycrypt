//! tcfs-file-provider: C FFI bridge for macOS/iOS FileProvider extensions via cbindgen
//!
//! This crate exposes tcfs storage, chunking, and sync operations
//! as a C-compatible FFI layer via cbindgen, enabling Swift consumers
//! to build native FileProvider extensions (.appex).
//!
//! ## Backends
//!
//! - `direct` (default): Talks directly to S3/SeaweedFS via OpenDAL.
//!   No daemon needed but no fleet sync, no NATS events.
//! - `grpc`: Delegates metadata/watch operations to tcfsd via gRPC while
//!   downloading FileProvider content in-process to avoid cross-sandbox writes.

use std::ffi::CString;
use std::os::raw::c_char;

/// Progress callback for file downloads.
/// Called with (completed_bytes, total_bytes, user_context).
pub type TcfsProgressCallback = Option<unsafe extern "C" fn(u64, u64, *const std::ffi::c_void)>;

/// Watch callback invoked when the background watch stream detects a change.
/// Called with (user_context). The Swift side should call signalEnumerator().
pub type TcfsWatchCallback = Option<unsafe extern "C" fn(*const std::ffi::c_void)>;

/// Error codes returned by FFI functions.
#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
pub enum TcfsError {
    /// Success (no error).
    TcfsErrorNone = 0,
    /// Invalid argument (null pointer, bad JSON, etc.).
    TcfsErrorInvalidArg = 1,
    /// Storage/network error communicating with S3/SeaweedFS.
    TcfsErrorStorage = 2,
    /// File or item not found.
    TcfsErrorNotFound = 3,
    /// Internal error (panic caught, unexpected state).
    TcfsErrorInternal = 4,
    /// Concurrent vclock modification detected.
    TcfsErrorConflict = 5,
    /// Item already exists at the target path.
    TcfsErrorAlreadyExists = 6,
    /// The requested immutable manifest version is no longer current.
    TcfsErrorVersionMismatch = 7,
    /// Incremental change history cannot safely continue from this anchor.
    TcfsErrorSyncAnchorExpired = 8,
}

/// Exact FileProvider reads bind the version exposed during enumeration to the
/// immutable manifest selected immediately before hydration. Keeping this as a
/// typed error lets every backend distinguish a stale Apple version token from
/// storage corruption and from true logical absence.
#[derive(Debug, thiserror::Error)]
#[error("requested FileProvider version {requested} is no longer current (current: {current})")]
pub(crate) struct FileProviderVersionMismatch {
    requested: String,
    current: String,
}

pub(crate) fn ensure_file_provider_version(
    requested: Option<&str>,
    current_manifest_id: &str,
) -> anyhow::Result<()> {
    let Some(requested) = requested.filter(|token| !token.is_empty()) else {
        return Ok(());
    };
    if requested == current_manifest_id {
        return Ok(());
    }
    Err(FileProviderVersionMismatch {
        requested: requested.to_string(),
        current: current_manifest_id.to_string(),
    }
    .into())
}

pub(crate) fn is_file_provider_version_mismatch(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<FileProviderVersionMismatch>())
}

/// A file item returned by directory enumeration.
///
/// The Swift layer reads these fields and maps them to
/// `NSFileProviderItem` properties.
#[repr(C)]
pub struct TcfsFileItem {
    /// Unique item identifier (UTF-8 C string, caller must free via `tcfs_string_free`).
    pub item_id: *mut c_char,
    /// Display filename (UTF-8 C string).
    pub filename: *mut c_char,
    /// File size in bytes.
    pub file_size: u64,
    /// Last-modified timestamp (Unix epoch seconds).
    pub modified_timestamp: i64,
    /// Whether this item is a directory.
    pub is_directory: bool,
    /// Opaque FileProvider version token (selected manifest object ID).
    pub content_hash: *mut c_char,
    /// Hydration state: "synced", "not_synced", "active", "locked", "conflict" (UTF-8 C string).
    pub hydration_state: *mut c_char,
}

/// A change event returned by `tcfs_provider_enumerate_changes`.
///
/// Represents a single file change since a given timestamp anchor.
/// The Swift layer uses this for incremental `enumerateChanges`.
#[repr(C)]
pub struct TcfsChangeEvent {
    /// File path (relative to mount root, UTF-8 C string).
    pub path: *mut c_char,
    /// Display filename (UTF-8 C string).
    pub filename: *mut c_char,
    /// Event type: "created", "modified", "deleted", "renamed" (UTF-8 C string).
    pub event_type: *mut c_char,
    /// Timestamp of the change (Unix epoch seconds).
    pub timestamp: i64,
    /// File size in bytes (0 for deleted items).
    pub file_size: u64,
    /// Opaque FileProvider version token (selected manifest object ID; empty
    /// for deleted items and directories).
    pub content_hash: *mut c_char,
    /// Whether this item is a directory.
    pub is_directory: bool,
}

// ============================================================================
// Direct backend (default): S3 via OpenDAL
// ============================================================================

#[cfg(feature = "direct")]
mod direct;

#[cfg(any(feature = "direct", feature = "grpc"))]
mod storage_bounds;

// Device-aware encryption-context wiring (TIN-1417 / B1), shared across the
// FileProvider backends.
#[cfg(any(feature = "direct", feature = "grpc"))]
mod device_ctx;

#[cfg(feature = "direct")]
pub use direct::*;

// ============================================================================
// gRPC backend: delegate to tcfsd daemon
// ============================================================================

#[cfg(feature = "grpc")]
mod grpc_backend;

#[cfg(feature = "grpc")]
pub use grpc_backend::*;

// ============================================================================
// UniFFI bindings for iOS (proc-macro based, no UDL)
// ============================================================================

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!();

#[cfg(feature = "uniffi")]
mod uniffi_bridge;

#[cfg(feature = "uniffi")]
pub use uniffi_bridge::*;

// ============================================================================
// Shared FFI helpers
// ============================================================================

/// Free an array of `TcfsChangeEvent` returned by `tcfs_provider_enumerate_changes`.
///
/// # Safety
///
/// - `events` must be a pointer returned by `tcfs_provider_enumerate_changes`, or null.
/// - `count` must match the count returned by the same call.
#[no_mangle]
pub unsafe extern "C" fn tcfs_change_events_free(events: *mut TcfsChangeEvent, count: usize) {
    if events.is_null() || count == 0 {
        return;
    }

    unsafe {
        let slice = std::slice::from_raw_parts_mut(events, count);
        for event in slice.iter_mut() {
            free_c_string(event.path);
            free_c_string(event.filename);
            free_c_string(event.event_type);
            free_c_string(event.content_hash);
        }
        let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(events, count));
    }
}

/// Free an array of `TcfsFileItem` returned by `tcfs_provider_enumerate`.
///
/// # Safety
///
/// - `items` must be a pointer returned by `tcfs_provider_enumerate`, or null.
/// - `count` must match the count returned by the same call.
#[no_mangle]
pub unsafe extern "C" fn tcfs_file_items_free(items: *mut TcfsFileItem, count: usize) {
    if items.is_null() || count == 0 {
        return;
    }

    unsafe {
        let slice = std::slice::from_raw_parts_mut(items, count);
        for item in slice.iter_mut() {
            free_c_string(item.item_id);
            free_c_string(item.filename);
            free_c_string(item.content_hash);
            free_c_string(item.hydration_state);
        }
        let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(items, count));
    }
}

/// Free a C string allocated by this crate.
///
/// # Safety
///
/// `s` must be a pointer returned by an FFI function in this crate, or null.
#[no_mangle]
pub unsafe extern "C" fn tcfs_string_free(s: *mut c_char) {
    free_c_string(s);
}

fn to_c_string(s: &str) -> *mut c_char {
    CString::new(s)
        .unwrap_or_else(|_| CString::new("").unwrap())
        .into_raw()
}

unsafe fn free_c_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            drop(CString::from_raw(s));
        }
    }
}
