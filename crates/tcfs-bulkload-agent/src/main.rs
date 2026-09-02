//! `tcfs-bulkload-agent` -- the thin darwin-side bulkload half.
//!
//! Argument parsing is hand-rolled on purpose. `clap` is not on the R34
//! dependency allowlist for the agent, and a binary whose whole point is a
//! closed dependency graph should not grow a parser crate to read one
//! subcommand.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tcfs_bulkload_agent::freshness::{Freshness, FreshnessCache as _, MemoryCache, StatIdentity};
use tcfs_bulkload_agent::hash;
use tcfs_bulkload_agent::walk::{self, HashPolicy, WalkOptions};
use tcfs_bulkload_proto::{BulkloadRefusal, FileKind, Frame, FrameKind, Result, RowSchema};

const USAGE: &str = "\
tcfs-bulkload-agent -- tcfs bulkload agent (M1 skeleton)

USAGE:
    tcfs-bulkload-agent <SUBCOMMAND>

SUBCOMMANDS:
    selftest    Hash a temporary file and round-trip a postcard frame
    walk PATH   Stat-walk PATH and print the row and refusal counts
    help        Print this message
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let outcome = match args.next().as_deref() {
        Some("selftest") => selftest(),
        Some("walk") => {
            if let Some(path) = args.next() {
                walk_command(Path::new(&path))
            } else {
                eprintln!("tcfs-bulkload-agent: walk requires a PATH\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("tcfs-bulkload-agent: unknown subcommand {other:?}\n\n{USAGE}");
            return ExitCode::from(2);
        }
        None => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => {
            eprintln!("tcfs-bulkload-agent: refused: {refusal}");
            ExitCode::FAILURE
        }
    }
}

/// Exercise the pieces M1 actually ships: hash a real file off disk, put its
/// row in a frame, encode it with postcard, decode it back, and prove the
/// round trip is exact.
fn selftest() -> Result<()> {
    println!("tcfs-bulkload-agent selftest");

    let path = scratch_path("selftest");
    let payload = b"tcfs bulkload M1 selftest payload";
    write_scratch(&path, payload)?;

    let digest = hash::hash_file(&path);
    let meta = std::fs::metadata(&path);
    let cleanup = std::fs::remove_file(&path);

    let digest = digest?;
    let meta = meta?;
    cleanup?;

    if digest != hash::hash_bytes(payload) {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    println!("  hashed        {} bytes", payload.len());
    println!("  blake3        {}", hex(&digest));
    println!("  crc32c        {:08x}", hash::checksum(payload));
    println!("  cdc chunks    {}", hash::chunk_boundaries(payload).len());

    let row = row_for(payload.len(), &meta, digest);
    let identity = StatIdentity::from_row(&row);
    let mut cache = MemoryCache::new();
    if cache.lookup(&identity)? != Freshness::Stale {
        return Err(BulkloadRefusal::ContractSelfInconsistent);
    }
    cache.record(&identity)?;
    if cache.lookup(&identity)? != Freshness::Fresh {
        return Err(BulkloadRefusal::ContractSelfInconsistent);
    }
    println!("  freshness     stale -> record -> fresh (ok)");

    let frame = Frame::new(FrameKind::Row(row));
    let encoded = frame.encode()?;
    let (decoded, consumed) = Frame::decode(&encoded)?;
    if decoded != frame || consumed != encoded.len() {
        return Err(BulkloadRefusal::FrameCodec);
    }
    println!("  frame bytes   {}", encoded.len());
    println!("  frame decoded {decoded:?}");
    println!("  round trip    exact (ok)");
    println!("selftest: ok");
    Ok(())
}

fn walk_command(root: &Path) -> Result<()> {
    let root = std::fs::canonicalize(root)?;
    let mut cache = MemoryCache::new();
    let options = WalkOptions {
        hash_policy: HashPolicy::Never,
        ..WalkOptions::new(root)
    };
    let outcome = walk::walk(&options, &mut cache)?;
    println!("rows                     {}", outcome.rows.len());
    println!("refusals                 {}", outcome.refusals.len());
    println!("seats_seen               {}", outcome.stats.seats_seen);
    println!("bytes_seen               {}", outcome.stats.bytes_seen);
    println!("fresh_skipped            {}", outcome.stats.fresh_skipped);
    println!(
        "bytes_reread_on_resume   {}",
        outcome.stats.bytes_reread_on_resume
    );
    println!(
        "files_statted_twice      {}",
        outcome.stats.files_statted_twice
    );
    Ok(())
}

fn row_for(len: usize, meta: &std::fs::Metadata, digest: [u8; 32]) -> RowSchema {
    use std::os::unix::fs::MetadataExt as _;
    RowSchema {
        rel_path: b"selftest".to_vec(),
        kind: FileKind::Regular,
        dev: meta.dev(),
        ino: meta.ino(),
        size: u64::try_from(len).unwrap_or(u64::MAX),
        mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
        ctime_ns: i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec()),
        mode: meta.mode(),
        nlink: meta.nlink(),
        link_target: None,
        blake3: Some(digest),
    }
}

fn scratch_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "tcfs-bulkload-agent-{name}-{}",
        std::process::id()
    ));
    path
}

fn write_scratch(path: &std::path::Path, payload: &[u8]) -> Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(payload)?;
    file.sync_all()?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut acc, byte| {
        let _ = write!(acc, "{byte:02x}");
        acc
    })
}
