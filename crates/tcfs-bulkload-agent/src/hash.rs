//! Content hashing and chunking for the agent half.
//!
//! Files are opened with `O_NOFOLLOW | O_CLOEXEC`, mirroring the Python
//! engine's open flags: a seat the walker classified as a regular file must
//! still be a regular file when its bytes are read, or the read is refused
//! rather than followed somewhere else.

use std::ffi::CString;
use std::fs::File;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::FromRawFd;
use std::path::Path;

use tcfs_bulkload_proto::{BulkloadRefusal, Result};

/// Read buffer size. Large enough to keep blake3's SIMD lanes busy without
/// putting a megabyte per rayon worker on the stack.
const READ_CHUNK_BYTES: usize = 256 * 1024;

/// fastcdc content-defined chunking parameters (min / avg / max bytes).
pub const CDC_MIN_BYTES: u32 = 16 * 1024;
/// See [`CDC_MIN_BYTES`].
pub const CDC_AVG_BYTES: u32 = 64 * 1024;
/// See [`CDC_MIN_BYTES`].
pub const CDC_MAX_BYTES: u32 = 256 * 1024;

/// Open `path` without following a terminal symlink.
///
/// # Errors
///
/// Refuses with [`BulkloadRefusal::PathNotPortable`] if the path contains an
/// interior NUL, and [`BulkloadRefusal::Io`] carrying the errno otherwise. A
/// path that became a symlink between `lstat` and open surfaces as `ELOOP`.
pub fn open_nofollow(path: &Path) -> Result<File> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| BulkloadRefusal::PathNotPortable)?;
    // SAFETY: `c_path` is a valid NUL-terminated C string that outlives the
    // call, and the flag set contains no mode-bearing flag (no O_CREAT), so
    // the two-argument form of `open` is correct.
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(BulkloadRefusal::from(std::io::Error::last_os_error()));
    }
    // SAFETY: `fd` is a fresh, open, owned descriptor that nothing else holds.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// blake3 of the bytes of `path`, streamed.
///
/// # Errors
///
/// Refuses if the file cannot be opened without following a symlink, or if a
/// read fails partway through.
pub fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let mut file = open_nofollow(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0_u8; READ_CHUNK_BYTES];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        let filled = buf.get(..read).ok_or(BulkloadRefusal::Io(None))?;
        hasher.update(filled);
    }
    Ok(*hasher.finalize().as_bytes())
}

/// blake3 of an in-memory buffer.
#[must_use]
pub fn hash_bytes(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}

/// CRC32-C of an in-memory buffer.
///
/// Used as the cheap per-frame integrity check on the wire, where a full
/// blake3 would dominate the cost of a small control record.
#[must_use]
pub fn checksum(data: &[u8]) -> u32 {
    crc32c::crc32c(data)
}

/// Content-defined chunk boundaries over `data`, as `(offset, length)` pairs.
///
/// M1 exposes the boundaries only; the M2 lane turns these into the
/// deduplicated transfer unit.
#[must_use]
pub fn chunk_boundaries(data: &[u8]) -> Vec<(usize, usize)> {
    fastcdc::v2020::FastCDC::new(data, CDC_MIN_BYTES, CDC_AVG_BYTES, CDC_MAX_BYTES)
        .map(|chunk| (chunk.offset, chunk.length))
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use std::io::Write as _;

    use super::{chunk_boundaries, checksum, hash_bytes, hash_file, open_nofollow, CDC_MIN_BYTES};

    fn scratch(name: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("tcfs-bulkload-agent-test-{}-{name}", std::process::id()));
        dir
    }

    #[test]
    fn hashes_a_file_like_it_hashes_the_bytes() {
        let path = scratch("hash");
        let payload = b"tcfs bulkload M1";
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(payload).unwrap();
        drop(file);

        assert_eq!(hash_file(&path).unwrap(), hash_bytes(payload));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn refuses_to_follow_a_symlink() {
        let target = scratch("nofollow-target");
        let link = scratch("nofollow-link");
        std::fs::write(&target, b"payload").unwrap();
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let refused = open_nofollow(&link);
        assert!(refused.is_err(), "O_NOFOLLOW must refuse a symlink");

        std::fs::remove_file(&link).unwrap();
        std::fs::remove_file(&target).unwrap();
    }

    #[test]
    fn chunks_cover_the_input_exactly_once() {
        let data: Vec<u8> = (0..(CDC_MIN_BYTES * 8))
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect();
        let chunks = chunk_boundaries(&data);
        assert!(!chunks.is_empty());

        let mut cursor = 0_usize;
        for (offset, length) in &chunks {
            assert_eq!(*offset, cursor, "chunks must be contiguous");
            cursor += *length;
        }
        assert_eq!(cursor, data.len(), "chunks must cover the whole input");
    }

    #[test]
    fn checksum_is_stable_and_content_sensitive() {
        assert_eq!(checksum(b"abc"), checksum(b"abc"));
        assert_ne!(checksum(b"abc"), checksum(b"abd"));
    }
}
