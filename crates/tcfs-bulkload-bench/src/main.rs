//! Real local native/rclone copy and resume comparison on an immutable corpus.
//! Verification is outside timing and warms the OS cache. Initial means fresh
//! private application state, not cold storage. Outputs are retained, never deleted.

use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

use clap::{Parser, ValueEnum};
use tcfs_bulkload_agent::freshness::NullCache;
use tcfs_bulkload_agent::transfer;
use tcfs_bulkload_agent::transfer_store::ChunkTiming;
use tcfs_bulkload_agent::walk::{walk, HashPolicy, WalkOptions};
use tcfs_bulkload_proto::{FileKind, RowSchema};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Arm {
    #[value(alias = "agent")]
    Native,
    #[value(alias = "baseline")]
    Rclone,
}

/// Real local copy/resume benchmark. No remote endpoints or live corpus allowed.
#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    /// Absolute sealed corpus directory; source identity is checked between runs.
    #[arg(long)]
    corpus_root: PathBuf,
    /// Absolute NEW directory under an existing parent. Retained after completion.
    #[arg(long)]
    work_root: PathBuf,
    /// Absolute rclone executable. Required when running the rclone arm.
    #[arg(long)]
    rclone: Option<PathBuf>,
    /// Native samples, 1 through 5; arms alternate and begin/end with native.
    #[arg(long, default_value_t = 3)]
    reps: usize,
    /// Old agent/baseline spellings remain aliases, now performing real transfers.
    #[arg(long)]
    only: Option<Arm>,
}

fn main() -> ExitCode {
    match run(&Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("benchmark refused: {error}; private outputs retained");
            ExitCode::FAILURE
        }
    }
}

fn rows(root: &Path) -> io::Result<Vec<RowSchema>> {
    let result = walk(
        &WalkOptions {
            hash_policy: HashPolicy::Always,
            cross_device: true,
            ..WalkOptions::new(root.to_owned())
        },
        &mut NullCache,
    )
    .map_err(io::Error::other)?;
    if !result.refusals.is_empty() {
        return Err(io::Error::other("corpus verification refused entries"));
    }
    let mut rows = result.rows;
    if rows.iter().any(|row| {
        !matches!(
            row.kind,
            FileKind::Regular | FileKind::Directory | FileKind::Symlink
        )
    }) {
        return Err(io::Error::other("unsupported corpus entry"));
    }
    rows.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(rows)
}

fn same_payload(a: &[RowSchema], b: &[RowSchema]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.rel_path == b.rel_path
                && a.kind == b.kind
                && a.link_target == b.link_target
                && (a.kind != FileKind::Regular || (a.size == b.size && a.blake3 == b.blake3))
                && (a.kind == FileKind::Symlink || a.mode & 0o777 == b.mode & 0o777)
        })
}

fn corpus_identity(rows: &[RowSchema]) -> io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    for row in rows {
        hasher.update(&postcard::to_stdvec(row).map_err(io::Error::other)?);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn add_one_percent_delta(source: &Path, expected: &[RowSchema]) -> io::Result<usize> {
    let regular_files = expected
        .iter()
        .filter(|row| row.kind == FileKind::Regular && row.size > 0)
        .count();
    if regular_files == 0 {
        return Err(io::Error::other(
            "corpus has no non-empty regular file for delta",
        ));
    }
    let count = regular_files.div_ceil(100);
    for index in 0..count {
        let path = source.join(format!(".bulkload-bench-delta-{index:06}"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(b"bulkload controlled delta\n")?;
        file.sync_all()?;
    }
    fs::File::open(source)?.sync_all()?;
    Ok(count)
}

fn max_rss_kib(who: libc::c_int) -> io::Result<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `usage` is valid writable storage for `getrusage`.
    if unsafe { libc::getrusage(who, usage.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful `getrusage` initializes the entire `rusage` value.
    let max_rss = unsafe { usage.assume_init() }.ru_maxrss;
    #[cfg(target_os = "macos")]
    let max_rss = max_rss / 1024;
    u64::try_from(max_rss).map_err(io::Error::other)
}

fn private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)
}

fn prepare(cli: &Cli) -> io::Result<(PathBuf, PathBuf)> {
    if !(1..=5).contains(&cli.reps)
        || !cli.corpus_root.is_absolute()
        || !cli.work_root.is_absolute()
    {
        return Err(io::Error::other(
            "absolute paths and 1..=5 repetitions required",
        ));
    }
    let source = fs::canonicalize(&cli.corpus_root)?;
    let parent = fs::canonicalize(
        cli.work_root
            .parent()
            .ok_or_else(|| io::Error::other("missing work parent"))?,
    )?;
    let work = parent.join(
        cli.work_root
            .file_name()
            .ok_or_else(|| io::Error::other("missing work name"))?,
    );
    if source.starts_with(&work) || work.starts_with(&source) || !source.is_dir() {
        return Err(io::Error::other(
            "corpus and work roots must be disjoint directories",
        ));
    }
    if cli.only != Some(Arm::Native)
        && !cli
            .rclone
            .as_ref()
            .is_some_and(|p| p.is_absolute() && p.is_file())
    {
        return Err(io::Error::other(
            "explicit absolute rclone executable required",
        ));
    }
    private_dir(&work)?;
    Ok((source, work))
}

fn arm_order(native_samples: usize) -> impl Iterator<Item = Arm> {
    (0..(native_samples * 2 - 1)).map(|index| {
        if index.is_multiple_of(2) {
            Arm::Native
        } else {
            Arm::Rclone
        }
    })
}

fn rclone_copy(binary: &Path, source: &Path, destination: &Path) -> io::Result<()> {
    let mut command = Command::new(binary);
    // Do not let operator config/environment select remote backends or exclusions.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("RCLONE_") {
            command.env_remove(key);
        }
    }
    let status = command
        .args(["copy"])
        .arg(source)
        .arg(destination)
        .args([
            "--config",
            "/dev/null",
            "--create-empty-src-dirs",
            "--links",
            "--metadata",
            "--transfers",
            "4",
            "--checkers",
            "4",
            "--stats",
            "0",
            "--log-level",
            "ERROR",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!("rclone failed: {status}")));
    }
    Ok(())
}

fn metric(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
}

fn run(cli: &Cli) -> io::Result<()> {
    let (sealed_source, work) = prepare(cli)?;
    let sealed_expected = rows(&sealed_source)?;
    let sealed_identity = corpus_identity(&sealed_expected)?;
    let source = work.join("native-sealed-fixture");
    private_dir(&source)?;
    let fixture_source_state = work.join("fixture-source-state");
    let fixture_destination_state = work.join("fixture-destination-state");
    let fixture_stats = transfer::copy(
        &sealed_source,
        &source,
        &fixture_source_state,
        &fixture_destination_state,
    )
    .map_err(io::Error::other)?;
    if !fixture_stats.refusals.is_empty()
        || !same_payload(&sealed_expected, &rows(&source)?)
        || rows(&sealed_source)? != sealed_expected
    {
        return Err(io::Error::other(
            "native fixture creation did not preserve the sealed corpus",
        ));
    }
    let mut expected = rows(&source)?;
    let identity = corpus_identity(&expected)?;
    println!("scope=local-ordinary-file-copy verification=full-blake3-outside-timing cache=not-flushed outputs=retained sealed_corpus_blake3={sealed_identity} fixture_corpus_blake3={identity} source_rows={} fixture_seed_source_bytes_read={} fixture_seed_bytes_received={}", expected.len(), fixture_stats.source_bytes_read, fixture_stats.bytes_received);
    println!("rep arm phase elapsed_ms transferred_content_bytes source_bytes_read max_rss_kib delta_files source_verification_rows acceptance");
    let arms = arm_order(cli.reps)
        .filter(|arm| cli.only.is_none_or(|only| only == *arm))
        .collect::<Vec<_>>();
    for phase in ["initial", "resume", "delta"] {
        let delta_files = if phase == "delta" {
            let count = add_one_percent_delta(&source, &expected)?;
            expected = rows(&source)?;
            count
        } else {
            0
        };
        for (rep, arm) in arms.iter().copied().enumerate() {
            let root = work.join(format!("{rep}-{arm:?}"));
            let destination = root.join("destination");
            if phase == "initial" {
                private_dir(&root)?;
                private_dir(&destination)?;
            }
            if rows(&source)? != expected {
                return Err(io::Error::other("immutable corpus changed before arm"));
            }
            let timing_before = ChunkTiming::snapshot();
            let started = Instant::now();
            let (transferred, read) = match arm {
                Arm::Native => {
                    let stats = transfer::copy(
                        &source,
                        &destination,
                        &root.join("source-state"),
                        &root.join("destination-state"),
                    )
                    .map_err(io::Error::other)?;
                    if !stats.refusals.is_empty() {
                        return Err(io::Error::other("native transfer refused entries"));
                    }
                    (Some(stats.bytes_received), Some(stats.source_bytes_read))
                }
                Arm::Rclone => {
                    rclone_copy(
                        cli.rclone
                            .as_deref()
                            .ok_or_else(|| io::Error::other("rclone missing"))?,
                        &source,
                        &destination,
                    )?;
                    (None, None)
                }
            };
            let elapsed = started.elapsed();
            let max_rss_kib = max_rss_kib(match arm {
                Arm::Native => libc::RUSAGE_SELF,
                Arm::Rclone => libc::RUSAGE_CHILDREN,
            })?;
            let timing = ChunkTiming::snapshot().since(timing_before);
            if rows(&source)? != expected || !same_payload(&expected, &rows(&destination)?) {
                return Err(io::Error::other(
                    "source mutation or destination content/mode mismatch",
                ));
            }
            println!(
                "{rep} {arm:?} {phase} {:.3} {} {} {max_rss_kib} {delta_files} {} verified",
                elapsed.as_secs_f64() * 1000.0,
                metric(transferred),
                metric(read),
                expected.len()
            );
            if arm == Arm::Native {
                println!("chunk_timing rep={rep} phase={phase} scope=process-worker-sums put_calls={} put_ns={} file_syncs={} file_sync_ns={} dir_syncs={} dir_sync_ns={}",
                    timing.put_calls, timing.put_ns, timing.file_syncs, timing.file_sync_ns, timing.dir_syncs, timing.dir_sync_ns);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn cli_and_order() {
        Cli::command().debug_assert();
        assert_eq!(arm_order(1).collect::<Vec<_>>(), [Arm::Native]);
        assert_eq!(
            arm_order(3).collect::<Vec<_>>(),
            [
                Arm::Native,
                Arm::Rclone,
                Arm::Native,
                Arm::Rclone,
                Arm::Native
            ]
        );
        assert_eq!(metric(None), "unknown");
        assert_eq!(metric(Some(0)), "0");
    }

    #[test]
    fn native_copy_resume_and_acceptance() -> io::Result<()> {
        let fixture = tempfile::tempdir()?;
        let source = fixture.path().join("source");
        private_dir(&source)?;
        fs::write(source.join("raw"), b"raw\0bytes\r\n")?;
        std::os::unix::fs::symlink("raw", source.join("link"))?;
        private_dir(&source.join("empty"))?;
        let cli = Cli {
            corpus_root: source.clone(),
            work_root: fixture.path().join("work"),
            rclone: None,
            reps: 1,
            only: Some(Arm::Native),
        };
        run(&cli)?;
        // A repeated invocation cannot accidentally reuse another run's state.
        assert!(prepare(&cli).is_err());
        let destination = cli.work_root.join("0-Native/destination");
        let expected = rows(&cli.work_root.join("native-sealed-fixture"))?;
        assert!(same_payload(&expected, &rows(&destination)?));
        fs::write(destination.join("raw"), b"bad\0bytes\r\n")?;
        assert!(!same_payload(&expected, &rows(&destination)?));
        let overlapping = Cli {
            work_root: source.join("nested"),
            ..cli
        };
        assert!(prepare(&overlapping).is_err());
        Ok(())
    }
}
