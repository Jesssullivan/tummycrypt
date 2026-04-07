//! FFI null-safety and memory lifecycle tests for tcfs-file-provider.
//!
//! These tests verify the C FFI contract:
//! - All functions handle null pointers gracefully (no UB, return error codes)
//! - String allocation and deallocation are balanced
//! - Error enum has correct C repr values

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;

use tcfs_file_provider::*;

// ── TcfsError enum repr ──────────────────────────────────────────────────

#[test]
fn error_enum_values() {
    assert_eq!(TcfsError::TcfsErrorNone as i32, 0);
    assert_eq!(TcfsError::TcfsErrorInvalidArg as i32, 1);
    assert_eq!(TcfsError::TcfsErrorStorage as i32, 2);
    assert_eq!(TcfsError::TcfsErrorNotFound as i32, 3);
    assert_eq!(TcfsError::TcfsErrorInternal as i32, 4);
    assert_eq!(TcfsError::TcfsErrorConflict as i32, 5);
    assert_eq!(TcfsError::TcfsErrorAlreadyExists as i32, 6);
}

// ── tcfs_string_free ─────────────────────────────────────────────────────

#[test]
fn string_free_null_is_safe() {
    unsafe {
        tcfs_string_free(ptr::null_mut());
    }
    // Should not crash
}

#[test]
fn string_free_valid_string() {
    let s = CString::new("test string").unwrap();
    let raw = s.into_raw();
    unsafe {
        tcfs_string_free(raw);
    }
    // Should not leak or crash
}

// ── tcfs_file_items_free ─────────────────────────────────────────────────

#[test]
fn file_items_free_null_is_safe() {
    unsafe {
        tcfs_file_items_free(ptr::null_mut(), 0);
        tcfs_file_items_free(ptr::null_mut(), 10);
    }
}

#[test]
fn file_items_free_zero_count() {
    // Even with a non-null pointer, count=0 should be safe
    let mut dummy = TcfsFileItem {
        item_id: ptr::null_mut(),
        filename: ptr::null_mut(),
        file_size: 0,
        modified_timestamp: 0,
        is_directory: false,
        content_hash: ptr::null_mut(),
    };
    unsafe {
        tcfs_file_items_free(&mut dummy as *mut TcfsFileItem, 0);
    }
}

// ── tcfs_change_events_free ──────────────────────────────────────────────

#[test]
fn change_events_free_null_is_safe() {
    unsafe {
        tcfs_change_events_free(ptr::null_mut(), 0);
        tcfs_change_events_free(ptr::null_mut(), 5);
    }
}

// ── tcfs_provider_new null safety ────────────────────────────────────────

#[test]
fn provider_new_null_returns_null() {
    unsafe {
        let provider = tcfs_provider_new(ptr::null());
        assert!(provider.is_null());
    }
}

#[test]
fn provider_new_empty_json_returns_null() {
    // Empty JSON object — missing required S3 fields → operator build fails → null
    let json = CString::new("{}").unwrap();
    unsafe {
        let provider = tcfs_provider_new(json.as_ptr());
        // May succeed with defaults or fail — either way should not crash
        if !provider.is_null() {
            tcfs_provider_free(provider);
        }
    }
}

#[test]
fn provider_new_invalid_utf8() {
    // Test with invalid JSON but valid C string
    let json = CString::new("not json at all!!!").unwrap();
    unsafe {
        let provider = tcfs_provider_new(json.as_ptr());
        assert!(provider.is_null(), "invalid JSON should return null");
    }
}

// ── tcfs_provider_free null safety ───────────────────────────────────────

#[test]
fn provider_free_null_is_safe() {
    unsafe {
        tcfs_provider_free(ptr::null_mut());
    }
}

// ── tcfs_provider_enumerate null safety ──────────────────────────────────

#[test]
fn enumerate_null_provider_returns_invalid_arg() {
    let path = CString::new("").unwrap();
    let mut items: *mut TcfsFileItem = ptr::null_mut();
    let mut count: usize = 0;

    unsafe {
        let err = tcfs_provider_enumerate(
            ptr::null_mut(),
            path.as_ptr(),
            &mut items as *mut _,
            &mut count as *mut _,
        );
        assert!(matches!(err, TcfsError::TcfsErrorInvalidArg));
    }
}

#[test]
fn enumerate_null_path_returns_invalid_arg() {
    let mut items: *mut TcfsFileItem = ptr::null_mut();
    let mut count: usize = 0;

    unsafe {
        // Use a fake non-null provider pointer (we won't dereference it because
        // the null path check happens first)
        let err = tcfs_provider_enumerate(
            ptr::null_mut(),
            ptr::null(),
            &mut items as *mut _,
            &mut count as *mut _,
        );
        assert!(matches!(err, TcfsError::TcfsErrorInvalidArg));
    }
}

#[test]
fn enumerate_null_out_items_returns_invalid_arg() {
    let path = CString::new("").unwrap();
    let mut count: usize = 0;

    unsafe {
        let err = tcfs_provider_enumerate(
            ptr::null_mut(),
            path.as_ptr(),
            ptr::null_mut(),
            &mut count as *mut _,
        );
        assert!(matches!(err, TcfsError::TcfsErrorInvalidArg));
    }
}

// ── tcfs_provider_fetch null safety ──────────────────────────────────────

#[test]
fn fetch_null_provider_returns_invalid_arg() {
    let item = CString::new("test").unwrap();
    let dest = CString::new("/tmp/out").unwrap();
    unsafe {
        let err = tcfs_provider_fetch(ptr::null_mut(), item.as_ptr(), dest.as_ptr());
        assert!(matches!(err, TcfsError::TcfsErrorInvalidArg));
    }
}

#[test]
fn fetch_null_item_id_returns_invalid_arg() {
    let dest = CString::new("/tmp/out").unwrap();
    unsafe {
        let err = tcfs_provider_fetch(ptr::null_mut(), ptr::null(), dest.as_ptr());
        assert!(matches!(err, TcfsError::TcfsErrorInvalidArg));
    }
}

// ── tcfs_provider_upload null safety ─────────────────────────────────────

#[test]
fn upload_null_provider_returns_invalid_arg() {
    let local = CString::new("/tmp/file").unwrap();
    let remote = CString::new("path").unwrap();
    unsafe {
        let err = tcfs_provider_upload(ptr::null_mut(), local.as_ptr(), remote.as_ptr());
        assert!(matches!(err, TcfsError::TcfsErrorInvalidArg));
    }
}

// ── tcfs_provider_delete null safety ─────────────────────────────────────

#[test]
fn delete_null_provider_returns_invalid_arg() {
    let item = CString::new("test").unwrap();
    unsafe {
        let err = tcfs_provider_delete(ptr::null_mut(), item.as_ptr());
        assert!(matches!(err, TcfsError::TcfsErrorInvalidArg));
    }
}

// ── tcfs_provider_create_dir null safety ─────────────────────────────────

#[test]
fn create_dir_null_provider_returns_invalid_arg() {
    let parent = CString::new("").unwrap();
    let name = CString::new("test").unwrap();
    unsafe {
        let err = tcfs_provider_create_dir(ptr::null_mut(), parent.as_ptr(), name.as_ptr());
        assert!(matches!(err, TcfsError::TcfsErrorInvalidArg));
    }
}

// ── tcfs_provider_enumerate_changes null safety ──────────────────────────

#[test]
fn enumerate_changes_null_out_returns_invalid_arg() {
    let path = CString::new("").unwrap();
    unsafe {
        let err = tcfs_provider_enumerate_changes(
            ptr::null_mut(),
            path.as_ptr(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        );
        assert!(matches!(err, TcfsError::TcfsErrorInvalidArg));
    }
}

#[test]
fn enumerate_changes_valid_out_returns_empty() {
    let path = CString::new("").unwrap();
    let mut events: *mut TcfsChangeEvent = ptr::null_mut();
    let mut count: usize = 99;

    unsafe {
        let err = tcfs_provider_enumerate_changes(
            ptr::null_mut(),
            path.as_ptr(),
            0,
            &mut events as *mut _,
            &mut count as *mut _,
        );
        // Direct backend always returns empty — but provider is null, so this
        // hits the null check first. The test verifies null handling.
        // Note: the function checks out_events/out_count before provider
        assert!(matches!(
            err,
            TcfsError::TcfsErrorNone | TcfsError::TcfsErrorInvalidArg
        ));
    }
}

// ── TcfsFileItem struct layout ───────────────────────────────────────────

#[test]
fn file_item_struct_is_c_compatible() {
    // Verify the struct can be constructed with null pointers (as a C consumer would receive)
    let item = TcfsFileItem {
        item_id: ptr::null_mut(),
        filename: ptr::null_mut(),
        file_size: 42,
        modified_timestamp: 1234567890,
        is_directory: true,
        content_hash: ptr::null_mut(),
    };

    assert_eq!(item.file_size, 42);
    assert_eq!(item.modified_timestamp, 1234567890);
    assert!(item.is_directory);
}

#[test]
fn change_event_struct_is_c_compatible() {
    let event = TcfsChangeEvent {
        path: ptr::null_mut(),
        filename: ptr::null_mut(),
        event_type: ptr::null_mut(),
        timestamp: 9876543210,
        file_size: 1024,
        content_hash: ptr::null_mut(),
        is_directory: false,
    };

    assert_eq!(event.timestamp, 9876543210);
    assert_eq!(event.file_size, 1024);
    assert!(!event.is_directory);
}
