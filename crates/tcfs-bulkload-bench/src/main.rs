//! `tcfs-bulkload-bench` -- the M0 real-corpus A/B harness.
//!
//! This is deliberately **not** a criterion benchmark. The thing being
//! measured is a one-shot walk of a real corpus of millions of files on real
//! storage; criterion's model -- warm up, then run the same tiny operation
//! thousands of times against a hot page cache -- measures the opposite of
//! what R25 cares about. So: N reps, median and spread, and the headline
//! metrics printed as first-class columns.
//!
//! # Status (M0)
//!
//! The harness is a skeleton. It compiles, it runs, and its shape is the shape
//! the real numbers will land in. It is not yet meaningful:
//!
//! * The `agent` arm runs the real walker but against a
//!   [`NullCache`](tcfs_bulkload_agent::freshness::NullCache), so there is no
//!   resume to measure yet.
//! * `bytes_reread_on_resume` and `files_statted_twice` are wired through from
//!   the walker for the agent arm and stubbed at `0` for the baseline arm --
//!   see the TODOs below.
//!
//! # Usage
//!
//! ```text
//! tcfs-bulkload-bench --corpus-root /path/to/corpus --reps 3
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::Parser;
use tcfs_bulkload_agent::freshness::NullCache;
use tcfs_bulkload_agent::walk::{self, HashPolicy, WalkOptions};
use tcfs_bulkload_proto::BulkloadRefusal;

/// The M0 real-corpus A/B harness.
#[derive(Debug, Parser)]
#[command(name = "tcfs-bulkload-bench", version, about, long_about = None)]
struct Cli {
    /// Corpus root to walk. Must be an absolute path to an existing directory.
    #[arg(long, value_name = "PATH")]
    corpus_root: PathBuf,

    /// Repetitions per arm. The reported figure is the median.
    #[arg(long, default_value_t = 3, value_name = "N")]
    reps: usize,

    /// Run only the named arm ("baseline" or "agent"). Default: both.
    #[arg(long, value_name = "ARM")]
    only: Option<String>,
}

/// One arm's result for one repetition.
#[derive(Debug, Clone, Copy)]
struct Rep {
    elapsed: Duration,
    files: u64,
    bytes: u64,
    bytes_reread_on_resume: u64,
    files_statted_twice: u64,
}

/// One arm's aggregate across reps.
#[derive(Debug)]
struct ArmSummary {
    name: &'static str,
    reps: usize,
    median_ms: f64,
    spread_ms: f64,
    files: u64,
    bytes: u64,
    bytes_reread_on_resume: u64,
    files_statted_twice: u64,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(refusal) => {
            eprintln!("tcfs-bulkload-bench: refused: {refusal}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<(), BulkloadRefusal> {
    if !cli.corpus_root.is_absolute() {
        return Err(BulkloadRefusal::PathNotAbsolute);
    }
    if !cli.corpus_root.is_dir() {
        return Err(BulkloadRefusal::SnapshotCustodyUnavailable);
    }
    if cli.reps == 0 {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }

    let wanted = cli.only.as_deref();
    let mut summaries = Vec::new();

    if matches!(wanted, None | Some("baseline")) {
        summaries.push(measure("baseline", cli.reps, || {
            baseline_walk(&cli.corpus_root)
        })?);
    }
    if matches!(wanted, None | Some("agent")) {
        summaries.push(measure("agent", cli.reps, || agent_walk(&cli.corpus_root))?);
    }
    if summaries.is_empty() {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }

    println!("corpus_root: {}", cli.corpus_root.display());
    println!("reps:        {}\n", cli.reps);
    print_table(&summaries);
    println!(
        "\nNOTE (M0): this harness is a skeleton. The agent arm runs against a null\n\
         freshness cache, so no resume is exercised yet and the two headline columns\n\
         are not yet meaningful. See the M2 resume lane."
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// arms
// ---------------------------------------------------------------------------

/// The `rclone check`-style arm: a plain recursive stat walk with no cache and
/// no parallelism. This is the bar the agent has to beat.
fn baseline_walk(root: &Path) -> Result<Rep, BulkloadRefusal> {
    let started = Instant::now();
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                files += 1;
                bytes = bytes.saturating_add(meta.len());
            }
        }
    }

    Ok(Rep {
        elapsed: started.elapsed(),
        files,
        bytes,
        // TODO(M2): a resume-aware baseline re-reads everything by
        // construction; wire this to the real re-read accounting once the
        // resume path exists so the two arms are comparable.
        bytes_reread_on_resume: 0,
        // TODO(M2): count the baseline's second stat per seat (read_dir
        // metadata plus the transfer-time stat) once the transfer half lands.
        files_statted_twice: 0,
    })
}

/// The agent arm: the real walker, currently against a null freshness cache.
fn agent_walk(root: &Path) -> Result<Rep, BulkloadRefusal> {
    let options = WalkOptions {
        hash_policy: HashPolicy::Never,
        ..WalkOptions::new(root.to_path_buf())
    };
    let started = Instant::now();
    // TODO(M3): swap NullCache for the persistent SqliteCache so the second
    // rep measures a warm resume instead of a second cold walk.
    let mut cache = NullCache;
    let outcome = walk::walk(&options, &mut cache)?;
    let elapsed = started.elapsed();

    Ok(Rep {
        elapsed,
        files: outcome.stats.seats_seen,
        bytes: outcome.stats.bytes_seen,
        bytes_reread_on_resume: outcome.stats.bytes_reread_on_resume,
        files_statted_twice: outcome.stats.files_statted_twice,
    })
}

// ---------------------------------------------------------------------------
// measurement
// ---------------------------------------------------------------------------

fn measure<F>(name: &'static str, reps: usize, mut arm: F) -> Result<ArmSummary, BulkloadRefusal>
where
    F: FnMut() -> Result<Rep, BulkloadRefusal>,
{
    let mut results = Vec::with_capacity(reps);
    for _ in 0..reps {
        results.push(arm()?);
    }
    let last = results.last().copied().ok_or(BulkloadRefusal::BudgetExceeded)?;

    let mut millis: Vec<f64> = results
        .iter()
        .map(|rep| rep.elapsed.as_secs_f64() * 1000.0)
        .collect();
    millis.sort_by(f64::total_cmp);

    let median_ms = median(&millis).ok_or(BulkloadRefusal::BudgetExceeded)?;
    let lo = millis.first().copied().unwrap_or(median_ms);
    let hi = millis.last().copied().unwrap_or(median_ms);

    Ok(ArmSummary {
        name,
        reps,
        median_ms,
        spread_ms: hi - lo,
        files: last.files,
        bytes: last.bytes,
        bytes_reread_on_resume: last.bytes_reread_on_resume,
        files_statted_twice: last.files_statted_twice,
    })
}

/// Median of a pre-sorted slice.
fn median(sorted: &[f64]) -> Option<f64> {
    let len = sorted.len();
    if len == 0 {
        return None;
    }
    let mid = len / 2;
    if len % 2 == 1 {
        sorted.get(mid).copied()
    } else {
        let lo = sorted.get(mid - 1).copied()?;
        let hi = sorted.get(mid).copied()?;
        Some(lo.midpoint(hi))
    }
}

// ---------------------------------------------------------------------------
// output
// ---------------------------------------------------------------------------

fn print_table(summaries: &[ArmSummary]) {
    println!(
        "{:<10} {:>5} {:>12} {:>12} {:>12} {:>16} {:>24} {:>21}",
        "arm",
        "reps",
        "median_ms",
        "spread_ms",
        "files",
        "bytes",
        "bytes_reread_on_resume",
        "files_statted_twice",
    );
    println!("{}", "-".repeat(120));
    for summary in summaries {
        println!(
            "{:<10} {:>5} {:>12.2} {:>12.2} {:>12} {:>16} {:>24} {:>21}",
            summary.name,
            summary.reps,
            summary.median_ms,
            summary.spread_ms,
            summary.files,
            summary.bytes,
            summary.bytes_reread_on_resume,
            summary.files_statted_twice,
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::{median, Cli};
    use clap::CommandFactory as _;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn median_handles_odd_even_and_empty() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[1.0]), Some(1.0));
        assert_eq!(median(&[1.0, 3.0]), Some(2.0));
        assert_eq!(median(&[1.0, 3.0, 100.0]), Some(3.0));
        assert_eq!(median(&[1.0, 3.0, 5.0, 7.0]), Some(4.0));
    }
}
