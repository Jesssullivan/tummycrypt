//! Real local native/rclone copy and resume comparison on an immutable corpus.
//! Verification is outside timing and warms the OS cache. Initial means fresh
//! private application state, not cold storage. Outputs are retained, never deleted.

use std::fs;
use std::io;
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
    /// Repetitions, 1 through 5; arm order alternates each repetition.
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

const fn arm_order(rep: usize) -> [Arm; 2] {
    if rep.is_multiple_of(2) {
        [Arm::Native, Arm::Rclone]
    } else {
        [Arm::Rclone, Arm::Native]
    }
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
    let (source, work) = prepare(cli)?;
    let expected = rows(&source)?;
    println!("scope=local-ordinary-file-copy verification=full-blake3-outside-timing cache=not-flushed outputs=retained");
    println!("rep arm phase elapsed_ms transferred_content_bytes source_bytes_read acceptance");
    for rep in 0..cli.reps {
        for arm in arm_order(rep)
            .into_iter()
            .filter(|arm| cli.only.is_none_or(|only| only == *arm))
        {
            let root = work.join(format!("{rep}-{arm:?}"));
            private_dir(&root)?;
            let destination = root.join("destination");
            private_dir(&destination)?;
            for phase in ["initial", "resume"] {
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
                let timing = ChunkTiming::snapshot().since(timing_before);
                if rows(&source)? != expected || !same_payload(&expected, &rows(&destination)?) {
                    return Err(io::Error::other(
                        "source mutation or destination content/mode mismatch",
                    ));
                }
                println!(
                    "{rep} {arm:?} {phase} {:.3} {} {} verified",
                    elapsed.as_secs_f64() * 1000.0,
                    metric(transferred),
                    metric(read)
                );
                if arm == Arm::Native {
                    println!("chunk_timing rep={rep} phase={phase} scope=process-worker-sums put_calls={} put_ns={} file_syncs={} file_sync_ns={} dir_syncs={} dir_sync_ns={}",
                        timing.put_calls, timing.put_ns, timing.file_syncs, timing.file_sync_ns, timing.dir_syncs, timing.dir_sync_ns);
                }
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
        assert_eq!(arm_order(0), [Arm::Native, Arm::Rclone]);
        assert_eq!(arm_order(1), [Arm::Rclone, Arm::Native]);
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
        let expected = rows(&source)?;
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
