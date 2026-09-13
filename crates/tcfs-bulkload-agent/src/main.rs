//! Native ordinary-file transport and offline provider composition on Unix.
//!
//! Argument parsing is hand-rolled on purpose. `clap` is not on the R34
//! dependency allowlist for the agent, and a binary whose whole point is a
//! closed dependency graph should not grow a parser crate to read one
//! subcommands.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use tcfs_bulkload_agent::freshness::{Freshness, FreshnessCache as _, MemoryCache, StatIdentity};
use tcfs_bulkload_agent::hash;
use tcfs_bulkload_agent::walk::{self, HashPolicy, WalkOptions};
use tcfs_bulkload_proto::{BulkloadRefusal, FileKind, Frame, FrameKind, Result, RowSchema};

const USAGE: &str = "\
tcfs-bulkload-agent -- ordinary-file transport and offline SQLite composition

USAGE:
    tcfs-bulkload-agent <SUBCOMMAND>

SUBCOMMANDS:
    selftest    Hash a temporary file and round-trip a postcard frame
    walk PATH   Stat-walk PATH and print the row and refusal counts
    copy SOURCE DEST SOURCE_STATE DEST_STATE
                Native local copy with private resumable chunk stores
    pull HOST SOURCE DEST SOURCE_STATE DEST_STATE
                Native SSH pull; remote tcfs-bulkload-agent must be installed
    serve       Serve one framed request on stdin/stdout (for SSH)
    snapshot SOURCE OUTPUT [MAX_STEPS]
                Capture live SQLite through its online backup API
    compose BASE INCOMING OUTPUT SOURCE_ID [MAX_STEPS]
                Compose retained SQLite snapshots into a private candidate
    compose-state BASE INCOMING OUTPUT SOURCE_ID SOURCE_HOME DEST_HOME [MAX_STEPS]
                Compose Codex state with retained rollout path mapping
    help        Print this message

BOUNDARIES:
    copy/pull preserve divergent destinations and refuse live SQLite files.
    They enumerate the source each run; completed content is resumable.
    File manifests are bounded to 8 MiB; oversized manifests refuse.
    Git-native divergent union is not supplied by copy/pull.
    compose commands write offline candidates, never install live databases.
";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let command = args.next();
    let outcome = match command.as_ref().and_then(|value| value.to_str()) {
        Some("selftest") => selftest(),
        Some("walk") => {
            if let Some(path) = args.next() {
                walk_command(Path::new(&path))
            } else {
                eprintln!("tcfs-bulkload-agent: walk requires a PATH\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
        Some("copy" | "pull" | "snapshot" | "compose" | "compose-state") => native_command(
            command
                .as_ref()
                .and_then(|value| value.to_str())
                .unwrap_or(""),
            &args.collect::<Vec<_>>(),
        ),
        Some("serve") => tcfs_bulkload_agent::transfer::serve(
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
        ),
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

fn native_command(command: &str, args: &[std::ffi::OsString]) -> Result<()> {
    let path = |index: usize| {
        args.get(index)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    match command {
        "copy" if args.len() == 4 => {
            let stats =
                tcfs_bulkload_agent::transfer::copy(path(0)?, path(1)?, path(2)?, path(3)?)?;
            report_transfer(&stats)
        }
        "pull" if args.len() == 5 => {
            let host = args.first().ok_or(BulkloadRefusal::RequiredFieldMissing)?;
            let mut child = Command::new("ssh")
                .args(["-T", "-oBatchMode=yes", "-oConnectTimeout=15", "--"])
                .arg(host)
                .args(["tcfs-bulkload-agent", "serve"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()?;
            let result = {
                let mut output = child.stdin.take().ok_or(BulkloadRefusal::Io(None))?;
                let mut input = child.stdout.take().ok_or(BulkloadRefusal::Io(None))?;
                tcfs_bulkload_agent::transfer::receive(
                    &mut input,
                    &mut output,
                    path(1)?,
                    path(3)?,
                    path(2)?,
                    path(4)?,
                )
            };
            let exit_status = child.wait()?;
            let stats = result?;
            if !exit_status.success() {
                return Err(BulkloadRefusal::Io(None));
            }
            report_transfer(&stats)
        }
        "snapshot" if (2..=3).contains(&args.len()) => {
            tcfs_bulkload_agent::provider_sqlite::snapshot(
                path(0)?,
                path(1)?,
                steps(args.get(2))?,
            )?;
            println!("snapshot complete");
            Ok(())
        }
        "compose" if (4..=5).contains(&args.len()) => {
            let source_id = args
                .get(3)
                .and_then(|value| value.to_str())
                .ok_or(BulkloadRefusal::PathNotPortable)?;
            let stats = tcfs_bulkload_agent::provider_sqlite::compose_snapshots(
                path(0)?,
                path(1)?,
                path(2)?,
                source_id,
                steps(args.get(4))?,
            )?;
            println!(
                "inserted={} equivalent={} preserved={} unresolved={} paths_corrected={} unavailable_rollouts={}",
                stats.inserted, stats.equivalent, stats.preserved, stats.unresolved,
                stats.paths_corrected, stats.unavailable_rollouts
            );
            Ok(())
        }
        "compose-state" if (6..=7).contains(&args.len()) => {
            let source_id = args
                .get(3)
                .and_then(|value| value.to_str())
                .ok_or(BulkloadRefusal::PathNotPortable)?;
            let mapping = tcfs_bulkload_agent::provider_sqlite::PathMapping {
                source_home: path(4)?,
                destination_home: path(5)?,
            };
            let stats = tcfs_bulkload_agent::provider_sqlite::compose_state_snapshots(
                path(0)?,
                path(1)?,
                path(2)?,
                source_id,
                steps(args.get(6))?,
                &mapping,
            )?;
            println!(
                "inserted={} equivalent={} preserved={} unresolved={} paths_corrected={} unavailable_rollouts={}",
                stats.inserted, stats.equivalent, stats.preserved, stats.unresolved,
                stats.paths_corrected, stats.unavailable_rollouts
            );
            Ok(())
        }
        _ => Err(BulkloadRefusal::RequiredFieldMissing),
    }
}

fn steps(value: Option<&std::ffi::OsString>) -> Result<u32> {
    value.map_or(Ok(1_000_000), |value| {
        value
            .to_str()
            .ok_or(BulkloadRefusal::FieldDomainViolation)?
            .parse()
            .map_err(|_| BulkloadRefusal::FieldDomainViolation)
    })
}

fn report_transfer(stats: &tcfs_bulkload_agent::transfer::TransferStats) -> Result<()> {
    println!(
        "completed={} reused={} bytes_received={} source_bytes_read={} refusals={}",
        stats.completed,
        stats.reused,
        stats.bytes_received,
        stats.source_bytes_read,
        stats.refusals.len()
    );
    for (path, code) in &stats.refusals {
        eprintln!("refused {}: {code}", path.escape_ascii());
    }
    if stats.refusals.is_empty() {
        Ok(())
    } else {
        Err(BulkloadRefusal::ContractSelfInconsistent)
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
    path.push(format!("tcfs-bulkload-agent-{name}-{}", std::process::id()));
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
