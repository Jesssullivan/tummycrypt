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
    pull HOST SOURCE DEST SOURCE_STATE DEST_STATE [REMOTE_EXECUTABLE [SSH_CONFIG]]
                Native SSH pull; remote tcfs-bulkload-agent must be installed
    serve       Serve one framed request on stdin/stdout (for SSH)
    git-export REPO NEW_CAPTURE_DIR
                Archive refs/stashes and staged/worktree trees in a bundle
    estate-add PLAN SOURCE_REPO DEST_REPO [ABSENT_WORKSPACE]
                Append an explicit reviewed item; no automatic worktree proliferation
    estate-show PLAN
                Print the exact selected sources, repositories and restore targets
    estate-add-batch PLAN SOURCE DEST WORKSPACE_OR_DASH [SOURCE DEST WORKSPACE_OR_DASH ...]
                Append selected items in one linear plan update
    estate-capture PLAN PRIVATE_STATE CORPUS JOBS
                Capture reviewed Git items with successful capture reuse (jobs 1 or 2)
    estate-apply PLAN CORPUS PRIVATE_STATE SOURCE JOBS
                Import refs and restore only explicitly selected absent workspaces
    git-import REPO BUNDLE SOURCE
                Preserve bundle refs in a content-addressed carry namespace
    git-restore BUNDLE ABSENT_DEST SOURCE
                Restore captured staged/unstaged work into a new repository
    git-restore-linked BUNDLE REPOSITORY ABSENT_DEST SOURCE
                Restore captured work into a new linked worktree without switching others
    git-repair-missing-index BUNDLE REPOSITORY SOURCE NEW_RECEIPT
                Create a missing same-HEAD staged index only; never rewrite payload
    git-attach-matching-payload BUNDLE REPOSITORY DESTINATION SOURCE NEW_RECEIPT
                Attach exact matching payload using existing common Git administration
    git-attach-standalone-payload BUNDLE DESTINATION SOURCE NEW_RECEIPT ORIGIN_FROM ORIGIN_TO
                Attach exact payload with retained config and explicit local origin mapping
    git-restore-registered-payload BUNDLE REPOSITORY DESTINATION ADMIN SOURCE NEW_RECEIPT
                Restore absent payload only, preserving matching retained registration/index
    snapshot SOURCE OUTPUT [MAX_STEPS]
                Capture live SQLite through its online backup API
    compose BASE INCOMING OUTPUT SOURCE_ID [MAX_STEPS]
                Compose retained SQLite snapshots into a private candidate
    compose-state BASE INCOMING OUTPUT SOURCE_ID SOURCE_HOME DEST_HOME [MAX_STEPS]
                Compose Codex state with retained rollout path mapping
    hydrate-state SNAPSHOT SOURCE_HOME DEST_HOME MAX_BYTES JOBS [GZIP ZSTD]
                Add missing raw rollouts from retained compressed files; never replace
    apply-state-candidate LIVE BASE CANDIDATE MAX_ROWS
                Explicit external live-state import; requires operator authorization
    help        Print this message

BOUNDARIES:
    copy/pull require an existing destination directory.
    copy/pull preserve divergent destinations and refuse live SQLite files.
    They enumerate the source each run; completed content is resumable.
    File manifests allow 131072 chunks and frames at most 8 MiB; oversized files refuse.
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
        Some(
            "copy" | "pull" | "snapshot" | "compose" | "compose-state" | "git-export"
            | "git-import" | "git-restore" | "git-restore-linked",
        ) => native_command(
            command
                .as_ref()
                .and_then(|value| value.to_str())
                .unwrap_or(""),
            &args.collect::<Vec<_>>(),
        ),
        Some("hydrate-state") => hydrate_command(&args.collect::<Vec<_>>()),
        Some("git-repair-missing-index") => repair_index_command(&args.collect::<Vec<_>>()),
        Some("git-restore-registered-payload") => registered_command(&args.collect::<Vec<_>>()),
        Some("git-attach-matching-payload") => attach_payload_command(&args.collect::<Vec<_>>()),
        Some("git-attach-standalone-payload") => {
            attach_standalone_command(&args.collect::<Vec<_>>())
        }
        Some(
            name @ ("estate-add" | "estate-add-batch" | "estate-show" | "estate-capture"
            | "estate-apply"),
        ) => estate_command(name, &args.collect::<Vec<_>>()),
        Some("apply-state-candidate") => apply_state_command(&args.collect::<Vec<_>>()),
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
    if command == "git-restore-linked" && args.len() == 4 {
        let source = args
            .get(3)
            .and_then(|value| value.to_str())
            .ok_or(BulkloadRefusal::PathNotPortable)?;
        tcfs_bulkload_agent::git_carry::restore_linked(path(0)?, path(1)?, path(2)?, source)?;
        println!("linked restoration complete");
        return Ok(());
    }
    match command {
        "git-restore" if args.len() == 3 => {
            let source = args
                .get(2)
                .and_then(|value| value.to_str())
                .ok_or(BulkloadRefusal::PathNotPortable)?;
            tcfs_bulkload_agent::git_carry::restore_bundle(path(0)?, path(1)?, source)?;
            println!("{}", path(1)?.display());
            Ok(())
        }
        "git-export" if args.len() == 2 => {
            let bundle = tcfs_bulkload_agent::git_carry::export_repository(path(0)?, path(1)?)?;
            println!("{}", bundle.display());
            Ok(())
        }
        "git-import" if args.len() == 3 => {
            let source = args
                .get(2)
                .and_then(|value| value.to_str())
                .ok_or(BulkloadRefusal::PathNotPortable)?;
            let count = tcfs_bulkload_agent::git_carry::import_bundle(path(0)?, path(1)?, source)?;
            println!("{count}");
            Ok(())
        }
        "copy" if args.len() == 4 => {
            let stats =
                tcfs_bulkload_agent::transfer::copy(path(0)?, path(1)?, path(2)?, path(3)?)?;
            report_transfer(&stats)
        }
        "pull" if (5..=7).contains(&args.len()) => pull_command(args),
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

fn repair_index_command(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() != 4 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |i| {
        args.get(i)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let source = args
        .get(2)
        .and_then(|value| value.to_str())
        .ok_or(BulkloadRefusal::PathNotPortable)?;
    tcfs_bulkload_agent::git_carry::repair_missing_index(path(0)?, path(1)?, source, path(3)?)?;
    println!("missing index repaired; payload parity not asserted");
    Ok(())
}

fn registered_command(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() != 6 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |i| {
        args.get(i)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let source = args
        .get(4)
        .and_then(|arg| arg.to_str())
        .ok_or(BulkloadRefusal::PathNotPortable)?;
    tcfs_bulkload_agent::git_carry::registered::restore(
        path(0)?,
        path(1)?,
        path(2)?,
        path(3)?,
        source,
        path(5)?,
    )?;
    println!("registered payload restored; original administration and staging preserved");
    Ok(())
}
fn attach_standalone_command(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() != 6 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |i| {
        args.get(i)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let source = args
        .get(2)
        .and_then(|value| value.to_str())
        .ok_or(BulkloadRefusal::PathNotPortable)?;
    tcfs_bulkload_agent::git_carry::attach_standalone_payload(
        path(0)?,
        path(1)?,
        source,
        path(3)?,
        path(4)?,
        path(5)?,
    )?;
    println!("standalone payload attached; config retained, selected origin mapping activated");
    Ok(())
}

fn attach_payload_command(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() != 5 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |i| {
        args.get(i)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let source = args
        .get(3)
        .and_then(|value| value.to_str())
        .ok_or(BulkloadRefusal::PathNotPortable)?;
    tcfs_bulkload_agent::git_carry::attach_matching_payload(
        path(0)?,
        path(1)?,
        path(2)?,
        source,
        path(4)?,
    )?;
    println!("matching payload attached; existing common Git administration preserved");
    Ok(())
}

// Escaped paths are intentional: receipts must not permit embedded newlines.
#[allow(clippy::unnecessary_debug_formatting)]
fn estate_command(command: &str, args: &[std::ffi::OsString]) -> Result<()> {
    use tcfs_bulkload_agent::estate;
    let path = |i| {
        args.get(i)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let jobs = || {
        args.last()
            .and_then(|arg| arg.to_str())
            .ok_or(BulkloadRefusal::FieldDomainViolation)?
            .parse::<usize>()
            .map_err(|_| BulkloadRefusal::FieldDomainViolation)
    };
    let receipt = |row: &estate::Receipt| {
        let mut output = std::io::stdout().lock();
        writeln!(
            output,
            "item={} source={:?} outcome={} reason={:?}",
            row.item, row.source, row.outcome, row.reason
        )?;
        output.flush()?;
        Ok(())
    };
    match command {
        "estate-show" if args.len() == 1 => {
            for item in estate::inspect(path(0)?)? {
                println!("{item:?}");
            }
            Ok(())
        }
        "estate-add" if (3..=4).contains(&args.len()) => {
            estate::add(path(0)?, path(1)?, path(2)?, args.get(3).map(Path::new))
        }
        "estate-add-batch" if args.len() >= 4 && (args.len() - 1).is_multiple_of(3) => {
            let mut items = Vec::new();
            for group in args
                .get(1..)
                .ok_or(BulkloadRefusal::RequiredFieldMissing)?
                .chunks_exact(3)
            {
                let source = group
                    .first()
                    .map(PathBuf::from)
                    .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
                let repository = group
                    .get(1)
                    .map(PathBuf::from)
                    .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
                let workspace = group.get(2).filter(|p| *p != "-").map(PathBuf::from);
                items.push(estate::Item {
                    source,
                    repository,
                    workspace,
                });
            }
            estate::add_batch(path(0)?, &items)
        }
        "estate-capture" if args.len() == 4 => {
            estate::capture(path(0)?, path(1)?, path(2)?, jobs()?, &receipt)
        }
        "estate-apply" if args.len() == 5 => {
            let source = args
                .get(3)
                .and_then(|arg| arg.to_str())
                .ok_or(BulkloadRefusal::FieldDomainViolation)?;
            estate::apply(path(0)?, path(1)?, path(2)?, source, jobs()?, &receipt)
        }
        _ => Err(BulkloadRefusal::RequiredFieldMissing),
    }
}

fn apply_state_command(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() != 4 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |index: usize| {
        args.get(index)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let max_rows = args
        .get(3)
        .and_then(|value| value.to_str())
        .ok_or(BulkloadRefusal::FieldDomainViolation)?
        .parse()
        .map_err(|_| BulkloadRefusal::FieldDomainViolation)?;
    tcfs_bulkload_agent::provider_sqlite::online::apply_state_candidate(
        path(0)?,
        path(1)?,
        path(2)?,
        max_rows,
        &|receipt| {
            let mut output = std::io::stdout().lock();
            writeln!(
                output,
                "table={} inserted={} corrected={} conflicts={}",
                receipt.table, receipt.inserted, receipt.corrected, receipt.conflicts
            )?;
            output.flush()?;
            Ok(())
        },
    )
}

fn hydrate_command(args: &[std::ffi::OsString]) -> Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    if args.len() != 5 && args.len() != 7 {
        return Err(BulkloadRefusal::RequiredFieldMissing);
    }
    let path = |index: usize| {
        args.get(index)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let number = |index: usize| -> Result<u64> {
        args.get(index)
            .and_then(|value| value.to_str())
            .ok_or(BulkloadRefusal::FieldDomainViolation)?
            .parse()
            .map_err(|_| BulkloadRefusal::FieldDomainViolation)
    };
    let mapping = tcfs_bulkload_agent::provider_sqlite::PathMapping {
        source_home: path(1)?,
        destination_home: path(2)?,
    };
    let gzip = args
        .get(5)
        .map_or_else(|| Path::new("/usr/bin/gzip"), Path::new);
    let zstd = args
        .get(6)
        .map_or_else(|| Path::new("/usr/bin/zstd"), Path::new);
    let receipt = |report: &tcfs_bulkload_agent::provider_sqlite::hydrate::Hydrated| -> Result<()> {
        let mut output = std::io::stdout().lock();
        writeln!(
            output,
            "published={} bytes={} blake3={} source_identity={:?} path={} source={}",
            report.published,
            report.bytes,
            report.blake3,
            report.source_identity,
            report.path.as_os_str().as_bytes().escape_ascii(),
            report.source.as_os_str().as_bytes().escape_ascii()
        )?;
        output.flush()?;
        Ok(())
    };
    let reports = tcfs_bulkload_agent::provider_sqlite::hydrate::hydrate_state(
        path(0)?,
        &mapping,
        number(3)?,
        usize::try_from(number(4)?).map_err(|_| BulkloadRefusal::BudgetExceeded)?,
        gzip,
        zstd,
        &receipt,
    )?;
    println!("hydrated_files={}", reports.len());
    Ok(())
}

fn pull_command(args: &[std::ffi::OsString]) -> Result<()> {
    let path = |index: usize| {
        args.get(index)
            .map(Path::new)
            .ok_or(BulkloadRefusal::RequiredFieldMissing)
    };
    let host = args.first().ok_or(BulkloadRefusal::RequiredFieldMissing)?;
    let remote = match args.get(5) {
        None => "tcfs-bulkload-agent",
        Some(value) => {
            let value = value.to_str().ok_or(BulkloadRefusal::PathNotPortable)?;
            if !Path::new(value).is_absolute()
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
            {
                return Err(BulkloadRefusal::PathNotPortable);
            }
            value
        }
    };
    let mut ssh = Command::new("ssh");
    ssh.args(["-T", "-oBatchMode=yes", "-oConnectTimeout=15"]);
    if let Some(config) = args.get(6) {
        if !Path::new(config).is_absolute() {
            return Err(BulkloadRefusal::PathNotAbsolute);
        }
        ssh.arg("-F").arg(config);
    }
    let mut child = ssh
        .arg("--")
        .arg(host)
        .args([remote, "serve"])
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
