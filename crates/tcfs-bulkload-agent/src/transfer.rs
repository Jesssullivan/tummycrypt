//! Native resumable transfer over framed bidirectional stdio.
//!
//! Only missing chunks cross the wire. Source reads are retained in a private
//! content store; destination completion binds both source and output identity.
//! Enumeration still walks the tree. This is not a no-rewalk performance claim.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const BATCH_ROWS: usize = 32;
const CAPTURE_WORKERS: usize = 4;
// At most four queued plus four in-flight manifests (5 MiB each), with
// CDC buffers bounded separately. The existing full census remains O(N).
const MAX_MANIFEST_CHUNKS: usize = 131_072;

use tcfs_bulkload_proto::frame::{ChunkSpec, LENGTH_PREFIX_BYTES, MAX_FRAME_BYTES};
use tcfs_bulkload_proto::FileKind;

use crate::freshness::{NullCache, StatIdentity};
use crate::materialize::Destination;
use crate::transfer_store::{row_key, Manifest, Store};
use crate::walk::{walk, WalkOptions};
use crate::{BulkloadRefusal, Frame, FrameKind, Result, RowSchema};

/// Receiver accounting; any refusal means the requested carry is incomplete.
#[derive(Debug, Default)]
pub struct TransferStats {
    /// Newly materialized or independently verified regular files.
    pub completed: u64,
    /// Outputs skipped using a previously persisted completion.
    pub reused: u64,
    /// Content bytes received across the transport.
    pub bytes_received: u64,
    /// Actual source file bytes read while preparing missing captures.
    pub source_bytes_read: u64,
    /// Relative paths and refusal codes. Contents and credentials are never logged.
    pub refusals: Vec<(Vec<u8>, String)>,
}

/// Run the same framed protocol locally over a bounded Unix stream pair.
///
/// # Errors
/// Refuses overlapping roots, malformed data and transport or source failures.
pub fn copy(
    source: &Path,
    destination: &Path,
    source_state: &Path,
    destination_state: &Path,
) -> Result<TransferStats> {
    let source_root = std::fs::canonicalize(source)?;
    let destination_root = std::fs::canonicalize(destination)?;
    if source_root.starts_with(&destination_root) || destination_root.starts_with(&source_root) {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
    let (mut sender, mut receiver) = std::os::unix::net::UnixStream::pair()?;
    #[cfg(test)]
    for stream in [&sender, &receiver] {
        stream.set_read_timeout(Some(std::time::Duration::from_secs(20)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(20)))?;
    }
    std::thread::scope(|scope| -> Result<TransferStats> {
        let producer = std::thread::Builder::new().spawn_scoped(scope, move || {
            let mut input = sender.try_clone()?;
            serve(&mut input, &mut sender)
        })?;
        let result = {
            let mut output = receiver.try_clone()?;
            receive(
                &mut receiver,
                &mut output,
                source,
                source_state,
                destination,
                destination_state,
            )
        };
        drop(receiver);
        producer.join().map_err(|_| BulkloadRefusal::Io(None))??;
        result
    })
}

/// Serve one transfer request from a caller-authenticated stdio transport.
///
/// # Errors
/// Refuses malformed frames, unsafe state roots, source failures and broken I/O.
pub fn serve<R: Read, W: Write>(input: &mut R, output: &mut W) -> Result<()> {
    let FrameKind::TransferOpen { root, state } = read_frame(input)?.kind else {
        return Err(BulkloadRefusal::FrameCodec);
    };
    let root = std::fs::canonicalize(path(root))?;
    let store = Store::open(&path(state))?;
    if store.root().starts_with(&root) || root.starts_with(store.root()) {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
    let meta = std::fs::metadata(&root)?;
    let authority = postcard::to_stdvec(&(
        store.authority()?,
        root.as_os_str().as_bytes(),
        meta.dev(),
        meta.ino(),
    ))?;
    write_frame(
        output,
        FrameKind::TransferStart {
            authority: authority.clone(),
        },
    )?;
    let mut census = walk(
        &WalkOptions {
            cross_device: true,
            ..WalkOptions::new(root.clone())
        },
        &mut NullCache,
    )?;
    census
        .rows
        .sort_by(|left, right| left.rel_path.cmp(&right.rel_path));
    for refused in census.refusals {
        write_frame(
            output,
            FrameKind::Refusal {
                code: refused.refusal.code().to_owned(),
                rel_path: refused.rel_path,
            },
        )?;
    }
    let mut source_bytes_read = 0;
    let mut rows = 0;
    for batch in census.rows.chunks(BATCH_ROWS) {
        rows += batch.len() as u64;
        write_frame(
            output,
            FrameKind::TransferBatch {
                rows: batch.to_vec(),
            },
        )?;
        let FrameKind::WantFiles { needed } = read_frame(input)?.kind else {
            return Err(BulkloadRefusal::FrameCodec);
        };
        if needed.len() != batch.len() {
            return Err(BulkloadRefusal::FrameCodec);
        }
        if batch
            .iter()
            .zip(&needed)
            .any(|(row, needed)| *needed && row.kind != FileKind::Regular)
        {
            return Err(BulkloadRefusal::FrameCodec);
        }
        source_bytes_read += send_batch(input, output, &root, &authority, &store, batch, &needed)?;
    }
    write_frame(
        output,
        FrameKind::TransferDone {
            rows,
            source_bytes_read,
        },
    )
}

fn send_batch<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    root: &Path,
    authority: &[u8],
    store: &Store,
    batch: &[RowSchema],
    needed: &[bool],
) -> Result<u64> {
    let next = AtomicUsize::new(0);
    let state = store.root();
    std::thread::scope(|scope| -> Result<u64> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(CAPTURE_WORKERS);
        for _ in 0..CAPTURE_WORKERS.min(needed.iter().filter(|value| **value).count()) {
            let sender = sender.clone();
            let next = &next;
            std::thread::Builder::new().spawn_scoped(scope, move || {
                let opened = Store::open(state);
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(row) = batch.get(index) else { break };
                    if !needed.get(index).copied().unwrap_or(false) {
                        continue;
                    }
                    let mut bytes = 0;
                    let captured = opened
                        .as_ref()
                        .map_or(Err(BulkloadRefusal::Io(None)), |store| {
                            capture(root, authority, row, store, &mut bytes)
                        });
                    if sender.send((index, bytes, captured)).is_err() {
                        break;
                    }
                }
            })?;
        }
        drop(sender);
        let mut bytes_read = 0_u64;
        let mut completed = 0;
        for (index, bytes, captured) in receiver {
            bytes_read = bytes_read.saturating_add(bytes);
            completed += 1;
            let row = batch.get(index).ok_or(BulkloadRefusal::FrameCodec)?;
            write_frame(
                output,
                FrameKind::FileContent {
                    index: u32::try_from(index).map_err(|_| BulkloadRefusal::FrameCodec)?,
                },
            )?;
            match captured {
                Ok(manifest) => send_content(input, output, &manifest, store, row)?,
                Err(refusal) => write_frame(
                    output,
                    FrameKind::Refusal {
                        code: refusal.code().to_owned(),
                        rel_path: row.rel_path.clone(),
                    },
                )?,
            }
        }
        if completed != needed.iter().filter(|value| **value).count() {
            return Err(BulkloadRefusal::Io(None));
        }
        write_frame(output, FrameKind::BatchDone)?;
        Ok(bytes_read)
    })
}

fn send_content<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    manifest: &Manifest,
    store: &Store,
    row: &RowSchema,
) -> Result<()> {
    write_frame(
        output,
        FrameKind::Manifest {
            digest: manifest.digest,
            chunks: manifest.chunks.clone(),
        },
    )?;
    let FrameKind::WantChunks { digests } = read_frame(input)?.kind else {
        return Err(BulkloadRefusal::FrameCodec);
    };
    let permitted: HashSet<_> = manifest.chunks.iter().map(|chunk| chunk.digest).collect();
    let mut requested = HashSet::new();
    for digest in digests {
        if !permitted.contains(&digest) || !requested.insert(digest) {
            return Err(BulkloadRefusal::FrameCodec);
        }
        match store.chunk(&digest) {
            Ok(Some(data)) => write_frame(output, FrameKind::Chunk { digest, data })?,
            result => {
                let refusal = result.err().unwrap_or(BulkloadRefusal::SealedObjectMissing);
                write_frame(
                    output,
                    FrameKind::Refusal {
                        code: refusal.code().to_owned(),
                        rel_path: row.rel_path.clone(),
                    },
                )?;
                break;
            }
        }
    }
    if !matches!(read_frame(input)?.kind, FrameKind::Applied { .. }) {
        return Err(BulkloadRefusal::FrameCodec);
    }
    Ok(())
}

/// Receive an ordinary-file carry without replacing divergent outputs.
///
/// # Errors
/// Refuses protocol errors and unsafe roots. Per-path conflicts remain in stats.
pub fn receive<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    source: &Path,
    source_state: &Path,
    destination: &Path,
    destination_state: &Path,
) -> Result<TransferStats> {
    let store = Store::open(destination_state)?;
    let mut target = Destination::open(destination)?;
    if store.root().starts_with(target.path()) || target.path().starts_with(store.root()) {
        return Err(BulkloadRefusal::SnapshotRootsOverlap);
    }
    write_frame(
        output,
        FrameKind::TransferOpen {
            root: source.as_os_str().as_bytes().to_vec(),
            state: source_state.as_os_str().as_bytes().to_vec(),
        },
    )?;
    let FrameKind::TransferStart { authority } = read_frame(input)?.kind else {
        return Err(BulkloadRefusal::FrameCodec);
    };
    let target_meta = std::fs::metadata(target.path())?;
    let output_authority = postcard::to_stdvec(&(
        authority,
        target.path().as_os_str().as_bytes(),
        target_meta.dev(),
        target_meta.ino(),
    ))?;
    let mut stats = TransferStats::default();
    let mut rows = 0;
    loop {
        match read_frame(input)?.kind {
            FrameKind::TransferBatch { rows: batch } => {
                if batch.is_empty() || batch.len() > BATCH_ROWS {
                    return Err(BulkloadRefusal::BudgetExceeded);
                }
                rows += batch.len() as u64;
                let mut needed = batch
                    .iter()
                    .map(|row| prepare_row(&mut target, &store, &output_authority, row, &mut stats))
                    .collect::<Result<Vec<_>>>()?;
                write_frame(
                    output,
                    FrameKind::WantFiles {
                        needed: needed.clone(),
                    },
                )?;
                loop {
                    match read_frame(input)?.kind {
                        FrameKind::FileContent { index } => {
                            let index = index as usize;
                            let requested =
                                needed.get_mut(index).ok_or(BulkloadRefusal::FrameCodec)?;
                            if !*requested {
                                return Err(BulkloadRefusal::FrameCodec);
                            }
                            *requested = false;
                            receive_content(
                                input,
                                output,
                                &target,
                                &store,
                                &output_authority,
                                batch.get(index).ok_or(BulkloadRefusal::FrameCodec)?,
                                &mut stats,
                            )?;
                        }
                        FrameKind::BatchDone if needed.iter().all(|value| !*value) => break,
                        _ => return Err(BulkloadRefusal::FrameCodec),
                    }
                }
            }
            FrameKind::Refusal { code, rel_path } => stats.refusals.push((rel_path, code)),
            FrameKind::TransferDone {
                rows: sent,
                source_bytes_read,
            } if sent == rows => {
                stats.source_bytes_read = source_bytes_read;
                if stats.refusals.is_empty() {
                    target.finish_directories(&store)?;
                }
                return Ok(stats);
            }
            _ => return Err(BulkloadRefusal::FrameCodec),
        }
    }
}

fn prepare_row(
    target: &mut Destination,
    store: &Store,
    authority: &[u8],
    row: &RowSchema,
    stats: &mut TransferStats,
) -> Result<bool> {
    let key = row_key(authority, row)?;
    let preparation = match row.kind {
        FileKind::Directory => target.directory(row, store, authority).map(|()| false),
        FileKind::Symlink => target.symlink(row).map(|()| false),
        FileKind::Regular => target.identity(row).and_then(|identity| {
            if let Some(identity) = identity {
                if store.output_matches(&key, &identity)? {
                    stats.reused += 1;
                    return Ok(false);
                }
            }
            Ok(true)
        }),
        _ => Err(BulkloadRefusal::FieldDomainViolation),
    };
    let needed = match preparation {
        Ok(needed) => needed,
        Err(refusal) => {
            stats
                .refusals
                .push((row.rel_path.clone(), refusal.code().to_owned()));
            false
        }
    };
    Ok(needed)
}

fn receive_content<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    target: &Destination,
    store: &Store,
    authority: &[u8],
    row: &RowSchema,
    stats: &mut TransferStats,
) -> Result<()> {
    let key = row_key(authority, row)?;
    let manifest = match read_frame(input)?.kind {
        FrameKind::Manifest { digest, chunks } if chunks.len() <= MAX_MANIFEST_CHUNKS => {
            Manifest { digest, chunks }
        }
        FrameKind::Refusal { code, rel_path } => {
            stats.refusals.push((rel_path, code));
            return Ok(());
        }
        _ => return Err(BulkloadRefusal::FrameCodec),
    };
    let received = receive_chunks(input, output, &manifest, store, stats);
    let applied = received
        .and_then(|()| target.file(row, &manifest, store))
        .and_then(|identity| store.record_output(&key, &identity));
    write_frame(
        output,
        FrameKind::Applied {
            success: applied.is_ok(),
        },
    )?;
    match applied {
        Ok(()) => stats.completed += 1,
        Err(refusal) => stats
            .refusals
            .push((row.rel_path.clone(), refusal.code().to_owned())),
    }
    Ok(())
}

fn receive_chunks<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    manifest: &Manifest,
    store: &Store,
    stats: &mut TransferStats,
) -> Result<()> {
    let mut missing = Vec::new();
    let mut seen = HashSet::new();
    for chunk in &manifest.chunks {
        if chunk.size > u64::from(crate::hash::CDC_MAX_BYTES) {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        if seen.insert(chunk.digest) && store.chunk(&chunk.digest)?.is_none() {
            missing.push(chunk.digest);
        }
    }
    write_frame(
        output,
        FrameKind::WantChunks {
            digests: missing.clone(),
        },
    )?;
    for expected in missing {
        match read_frame(input)?.kind {
            FrameKind::Chunk { digest, data } if digest == expected => {
                stats.bytes_received = stats.bytes_received.saturating_add(data.len() as u64);
                store.put_chunk(&digest, &data)?;
            }
            FrameKind::Refusal { .. } => return Err(BulkloadRefusal::SealedObjectMissing),
            _ => return Err(BulkloadRefusal::FrameCodec),
        }
    }
    Ok(())
}

fn capture(
    root: &Path,
    authority: &[u8],
    row: &RowSchema,
    store: &Store,
    bytes_read: &mut u64,
) -> Result<Manifest> {
    if row.size > (MAX_MANIFEST_CHUNKS as u64) * u64::from(crate::hash::CDC_MAX_BYTES) {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    let key = row_key(authority, row)?;
    if let Some(manifest) = store.capture(&key)? {
        if manifest.chunks.len() > MAX_MANIFEST_CHUNKS {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        let available =
            manifest
                .chunks
                .iter()
                .try_fold(true, |available, chunk| -> Result<bool> {
                    Ok(available && store.chunk(&chunk.digest)?.is_some())
                })?;
        if available {
            return Ok(manifest);
        }
    }
    if row.rel_path.ends_with(b"-wal")
        || row.rel_path.ends_with(b"-shm")
        || row.rel_path.ends_with(b"-journal")
    {
        return Err(BulkloadRefusal::SqliteStateChanged);
    }
    let file_path = root.join(path(row.rel_path.clone()));
    let mut file = crate::hash::open_nofollow(&file_path)?;
    let expected = StatIdentity::from_row(row);
    if StatIdentity::from_metadata(&file.metadata()?) != expected {
        return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
    }
    let mut prefix = Vec::new();
    let mut reader = CountReader {
        input: &mut file,
        count: bytes_read,
    };
    (&mut reader).take(16).read_to_end(&mut prefix)?;
    if prefix.starts_with(b"SQLite format 3\0")
        || prefix.starts_with(&[0x37, 0x7f, 0x06, 0x82])
        || prefix.starts_with(&[0x37, 0x7f, 0x06, 0x83])
    {
        return Err(BulkloadRefusal::SqliteStateChanged);
    }
    let mut hasher = blake3::Hasher::new();
    let mut chunks = Vec::new();
    for chunk in fastcdc::v2020::StreamCDC::new(
        prefix.as_slice().chain(reader),
        crate::hash::CDC_MIN_BYTES,
        crate::hash::CDC_AVG_BYTES,
        crate::hash::CDC_MAX_BYTES,
    ) {
        let chunk = chunk.map_err(|_| BulkloadRefusal::Io(None))?;
        if chunks.len() >= MAX_MANIFEST_CHUNKS {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        hasher.update(&chunk.data);
        let digest = crate::hash::hash_bytes(&chunk.data);
        store.put_chunk(&digest, &chunk.data)?;
        chunks.push(ChunkSpec {
            digest,
            size: chunk.data.len() as u64,
        });
    }
    if StatIdentity::from_metadata(&file.metadata()?) != expected {
        return Err(BulkloadRefusal::SourceChangedAfterSnapshot);
    }
    let manifest = Manifest {
        digest: *hasher.finalize().as_bytes(),
        chunks,
    };
    store.record_capture(&key, &manifest)?;
    Ok(manifest)
}

struct CountReader<'a, R> {
    input: R,
    count: &'a mut u64,
}
impl<R: Read> Read for CountReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.input.read(buffer)?;
        *self.count = self.count.saturating_add(count as u64);
        Ok(count)
    }
}

/// Read one bounded frame without searching for delimiters.
///
/// # Errors
/// Refuses truncated/oversized/invalid frames and incompatible protocol versions.
pub fn read_frame<R: Read>(input: &mut R) -> Result<Frame> {
    let mut header = [0_u8; LENGTH_PREFIX_BYTES];
    input.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    let mut bytes = header.to_vec();
    bytes.resize(LENGTH_PREFIX_BYTES + length, 0);
    input.read_exact(
        bytes
            .get_mut(LENGTH_PREFIX_BYTES..)
            .ok_or(BulkloadRefusal::FrameCodec)?,
    )?;
    Ok(Frame::decode(&bytes)?.0)
}

/// Write one bounded frame and flush the request/reply boundary.
///
/// # Errors
/// Refuses oversized messages and broken transports.
pub fn write_frame<W: Write>(output: &mut W, kind: FrameKind) -> Result<()> {
    output.write_all(&Frame::new(kind).encode()?)?;
    output.flush()?;
    Ok(())
}

fn path(bytes: Vec<u8>) -> PathBuf {
    std::ffi::OsString::from_vec(bytes).into()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Corpus {
        base: PathBuf,
    }
    impl Corpus {
        fn new() -> Self {
            let base = std::env::temp_dir().join(format!(
                "tcfs-native-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&base).unwrap();
            for path in ["source", "destination"] {
                std::fs::create_dir(base.join(path)).unwrap();
            }
            Self { base }
        }
        fn run(&self) -> Result<TransferStats> {
            copy(
                &self.base.join("source"),
                &self.base.join("destination"),
                &self.base.join("source-state"),
                &self.base.join("destination-state"),
            )
        }
    }
    impl Drop for Corpus {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn parallel_batches_resume_and_continue_after_a_refused_file() {
        let corpus = Corpus::new();
        let bytes = vec![71_u8; 65_536];
        for index in 0..70 {
            std::fs::write(
                corpus.base.join("source").join(format!("file-{index:03}")),
                &bytes,
            )
            .unwrap();
        }
        std::fs::write(
            corpus.base.join("source/file-035.db"),
            b"SQLite format 3\0refused",
        )
        .unwrap();
        let first = corpus.run().unwrap();
        assert_eq!(first.completed, 70);
        assert_eq!(first.refusals.len(), 1);
        assert_eq!(first.source_bytes_read, 70 * 65_536 + 16);
        assert_eq!(first.bytes_received, 65_536);
        for index in 0..70 {
            assert_eq!(
                std::fs::read(
                    corpus
                        .base
                        .join("destination")
                        .join(format!("file-{index:03}"))
                )
                .unwrap(),
                bytes
            );
        }
        let second = corpus.run().unwrap();
        assert_eq!(second.reused, 70);
        assert_eq!(second.refusals.len(), 1);
        assert_eq!(second.source_bytes_read, 16);
        assert_eq!(second.bytes_received, 0);
    }

    #[test]
    fn round_trip_resumes_without_source_reads_and_preserves_divergence() {
        let corpus = Corpus::new();
        let source = corpus.base.join("source");
        let destination = corpus.base.join("destination");
        std::fs::create_dir(source.join("nested")).unwrap();
        let bytes: Vec<u8> = (0..800_000)
            .map(|value| u8::try_from(value % 251).unwrap())
            .collect();
        std::fs::write(source.join("nested/data"), &bytes).unwrap();
        std::fs::write(source.join(".credential"), b"account-file").unwrap();
        std::fs::set_permissions(
            source.join(".credential"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        std::os::unix::fs::symlink("../elsewhere", source.join("link")).unwrap();
        let first = corpus.run().unwrap();
        assert!(first.refusals.is_empty(), "{:?}", first.refusals);
        assert_eq!(
            std::fs::read(destination.join("nested/data")).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::read_link(destination.join("link")).unwrap(),
            Path::new("../elsewhere")
        );
        assert_eq!(
            std::fs::metadata(destination.join(".credential"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let second = corpus.run().unwrap();
        assert_eq!(second.reused, 2);
        assert_eq!(second.source_bytes_read, 0);
        assert_eq!(second.bytes_received, 0);
        std::fs::set_permissions(
            destination.join("nested"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let metadata_conflict = corpus.run().unwrap();
        assert_eq!(metadata_conflict.refusals.len(), 1);
        assert_eq!(
            std::fs::metadata(destination.join("nested"))
                .unwrap()
                .mode()
                & 0o777,
            0o700
        );
        std::fs::set_permissions(
            destination.join("nested"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::write(destination.join(".credential"), b"sting-unique").unwrap();
        let third = corpus.run().unwrap();
        assert_eq!(third.refusals.len(), 1);
        assert_eq!(
            std::fs::read(destination.join(".credential")).unwrap(),
            b"sting-unique"
        );
        assert_eq!(
            std::fs::read(source.join(".credential")).unwrap(),
            b"account-file"
        );
    }

    #[test]
    fn sqlite_headers_and_destination_symlinks_are_not_raw_copied() {
        let corpus = Corpus::new();
        let source = corpus.base.join("source");
        let destination = corpus.base.join("destination");
        std::fs::write(source.join("credentials.db"), b"SQLite format 3\0opaque").unwrap();
        std::fs::create_dir(source.join("nested")).unwrap();
        std::fs::write(source.join("nested/secret"), b"secret").unwrap();
        let outside = corpus.base.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, destination.join("nested")).unwrap();
        let result = corpus.run().unwrap();
        assert_eq!(result.refusals.len(), 3);
        assert!(!destination.join("credentials.db").exists());
        assert!(!outside.join("secret").exists());
    }

    #[test]
    fn interrupted_directory_mode_finalization_resumes_only_its_owned_inode() {
        let corpus = Corpus::new();
        let source = corpus.base.join("source");
        let destination = corpus.base.join("destination");
        std::fs::create_dir(source.join("nested")).unwrap();
        let mut row = walk(&WalkOptions::new(source), &mut NullCache)
            .unwrap()
            .rows
            .remove(0);
        row.mode = 0o40_555;
        let store = Store::open(&corpus.base.join("destination-state")).unwrap();
        {
            let mut first = Destination::open(&destination).unwrap();
            first.directory(&row, &store, b"authority").unwrap();
            assert_eq!(
                std::fs::metadata(destination.join("nested"))
                    .unwrap()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        let mut resumed = Destination::open(&destination).unwrap();
        resumed.directory(&row, &store, b"authority").unwrap();
        resumed.finish_directories(&store).unwrap();
        assert_eq!(
            std::fs::metadata(destination.join("nested"))
                .unwrap()
                .mode()
                & 0o777,
            0o555
        );
    }
}
