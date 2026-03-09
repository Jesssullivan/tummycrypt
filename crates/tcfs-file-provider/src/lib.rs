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
//! - `grpc`: Delegates all operations to tcfsd via Unix domain socket gRPC.
//!   Full fleet sync, NATS events, conflict resolution.

use std::ffi::CString;
use std::os::raw::c_char;

/// Error codes returned by FFI functions.
#[repr(C)]
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
    /// Content hash (BLAKE3 hex, UTF-8 C string).
    pub content_hash: *mut c_char,
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
    /// Content hash (BLAKE3 hex, UTF-8 C string, empty for deleted items).
    pub content_hash: *mut c_char,
    /// Whether this item is a directory.
    pub is_directory: bool,
}

// ============================================================================
// Direct backend (default): S3 via OpenDAL
// ============================================================================

#[cfg(feature = "direct")]
mod direct;

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
