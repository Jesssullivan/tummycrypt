//! Real local native/rclone copy and resume comparison on an immutable corpus.
//! Verification is outside timing and warms the OS cache. Initial means fresh
//! private application state, not cold storage. Outputs are retained, never deleted.

use std::fs;
use std::io::{self, Read as _, Seek as _, Write as _};
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

use clap::{Parser, ValueEnum};
use tcfs_bulkload_agent::freshness::NullCache;
use tcfs_bulkload_agent::transfer::{self, TransferTiming};
use tcfs_bulkload_agent::transfer_store::ChunkTiming;
use tcfs_bulkload_agent::walk::{walk, HashPolicy, WalkOptions};
use tcfs_bulkload_proto::{FileKind, Frame, FrameKind, RowSchema};

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
    /// Exact source revision used to build the native binary under test.
    #[arg(long)]
    revision: String,
}

#[derive(Debug)]
struct DeltaEvidence {
    files: Vec<Vec<u8>>,
    bytes: u64,
    before_identity: String,
    after_identity: String,
}

#[derive(Debug)]
struct Sample {
    sequence: usize,
    arm: Arm,
    phase: &'static str,
    elapsed_ms: f64,
    elapsed_ns: u128,
    transferred: Option<u64>,
    source_read: Option<u64>,
    rss_kib: u64,
    workload_bytes: u64,
    chunk_timing: Option<ChunkTiming>,
    transfer_timing: Option<TransferTiming>,
}

struct SampleRun<'a> {
    source: &'a Path,
    expected: &'a [RowSchema],
    root: PathBuf,
    source_state: Option<PathBuf>,
    sequence: usize,
    arm: Arm,
    phase: &'static str,
    workload_bytes: u64,
    initialize: bool,
}

struct Fixture {
    source: PathBuf,
    expected: Vec<RowSchema>,
    sealed_expected: Vec<RowSchema>,
    sealed_identity: String,
    private_identity: String,
    seed_source_read: u64,
    seed_received: u64,
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

fn add_one_percent_delta(source: &Path, expected: &[RowSchema]) -> io::Result<DeltaEvidence> {
    let total_bytes = expected
        .iter()
        .filter(|row| row.kind == FileKind::Regular)
        .try_fold(0_u64, |total, row| total.checked_add(row.size))
        .ok_or_else(|| io::Error::other("regular-file byte count overflow"))?;
    if total_bytes == 0 {
        return Err(io::Error::other(
            "corpus has no non-empty regular file for delta",
        ));
    }
    let before_identity = corpus_identity(expected)?;
    let target = total_bytes.div_ceil(100);
    let mut remaining = target;
    let mut files = Vec::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    for row in expected
        .iter()
        .filter(|row| row.kind == FileKind::Regular && row.size > 0)
    {
        if remaining == 0 {
            break;
        }
        let mutate = remaining.min(row.size);
        let path = source.join(std::ffi::OsString::from_vec(row.rel_path.clone()));
        let mut file = fs::OpenOptions::new().read(true).write(true).open(path)?;
        let mut file_remaining = mutate;
        while file_remaining > 0 {
            let count = usize::try_from(file_remaining.min(buffer.len() as u64))
                .map_err(io::Error::other)?;
            let slice = buffer
                .get_mut(..count)
                .ok_or_else(|| io::Error::other("mutation buffer bounds"))?;
            file.read_exact(slice)?;
            for byte in slice.iter_mut() {
                *byte ^= 0xa5;
            }
            file.seek(std::io::SeekFrom::Current(
                -i64::try_from(count).map_err(io::Error::other)?,
            ))?;
            file.write_all(slice)?;
            file_remaining -= count as u64;
        }
        file.sync_all()?;
        files.push(row.rel_path.clone());
        remaining -= mutate;
    }
    if remaining != 0 {
        return Err(io::Error::other("delta mutation did not reach target"));
    }
    fs::File::open(source)?.sync_all()?;
    let after = rows(source)?;
    let after_identity = corpus_identity(&after)?;
    if before_identity == after_identity {
        return Err(io::Error::other(
            "delta mutation did not change corpus identity",
        ));
    }
    Ok(DeltaEvidence {
        files,
        bytes: target,
        before_identity,
        after_identity,
    })
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
        || cli.revision.trim().is_empty()
    {
        return Err(io::Error::other(
            "absolute paths, non-empty revision and 1..=5 repetitions required",
        ));
    }
    if cli.only.is_none() && cli.reps != 3 {
        return Err(io::Error::other(
            "the enforceable native/rclone gate requires exactly three native repetitions",
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

fn rclone_version(binary: &Path) -> io::Result<String> {
    let mut command = Command::new(binary);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("RCLONE_") {
            command.env_remove(key);
        }
    }
    let output = command.arg("version").stdin(Stdio::null()).output()?;
    if !output.status.success() {
        return Err(io::Error::other("rclone version command failed"));
    }
    String::from_utf8(output.stdout)
        .map_err(io::Error::other)?
        .lines()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("rclone version output was empty"))
}

struct StopBeforeDone<W>(W);

impl<W: io::Write> io::Write for StopBeforeDone<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if Frame::decode(data).is_ok_and(|(frame, consumed)| {
            consumed == data.len() && matches!(frame.kind, FrameKind::TransferDone { .. })
        }) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.0.write_all(data)?;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

fn interrupt_after_payload(
    source: &Path,
    destination: &Path,
    source_state: &Path,
    destination_state: &Path,
) -> io::Result<()> {
    let (sender, mut receiver) = std::os::unix::net::UnixStream::pair()?;
    std::thread::scope(|scope| -> io::Result<()> {
        let producer = std::thread::Builder::new().spawn_scoped(scope, move || {
            let mut input = sender.try_clone()?;
            transfer::serve(&mut input, &mut StopBeforeDone(sender))
        })?;
        let mut output = receiver.try_clone()?;
        let receive_outcome = transfer::receive(
            &mut receiver,
            &mut output,
            source,
            source_state,
            destination,
            destination_state,
        );
        drop(output);
        drop(receiver);
        let served = producer
            .join()
            .map_err(|_| io::Error::other("interruption producer panicked"))?;
        if served.is_ok() || receive_outcome.is_ok() {
            return Err(io::Error::other(
                "injected interruption unexpectedly completed",
            ));
        }
        Ok(())
    })
}

fn remove_delta_targets(destination: &Path, files: &[Vec<u8>]) -> io::Result<()> {
    for relative in files {
        fs::remove_file(destination.join(std::ffi::OsString::from_vec(relative.clone())))?;
    }
    fs::File::open(destination)?.sync_all()
}

fn regular_bytes(rows: &[RowSchema]) -> u64 {
    rows.iter()
        .filter(|row| row.kind == FileKind::Regular)
        .fold(0_u64, |total, row| total.saturating_add(row.size))
}

fn median(samples: &[Sample], arm: Arm, phase: &str) -> io::Result<f64> {
    let mut values = samples
        .iter()
        .filter(|sample| sample.arm == arm && sample.phase == phase)
        .map(|sample| sample.elapsed_ms)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return Err(io::Error::other("median has no samples"));
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    let high = values
        .get(middle)
        .copied()
        .ok_or_else(|| io::Error::other("median midpoint missing"))?;
    Ok(if values.len().is_multiple_of(2) {
        let low = values
            .get(middle.saturating_sub(1))
            .copied()
            .ok_or_else(|| io::Error::other("median lower midpoint missing"))?;
        f64::midpoint(low, high)
    } else {
        high
    })
}

fn metric(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
}

fn print_sample(sample: &Sample, verification_rows: usize) {
    let throughput = if sample.elapsed_ns > 0 {
        u128::from(sample.workload_bytes).saturating_mul(1_000_000_000) / sample.elapsed_ns
    } else {
        0
    };
    let rss_scope = match sample.arm {
        Arm::Native => "cumulative-process-peak",
        Arm::Rclone => "cumulative-children-peak",
    };
    println!(
        "sample sequence={} arm={:?} phase={} elapsed_ms={:.3} throughput_bytes_s={} workload_bytes={} transferred_content_bytes={} source_bytes_read={} max_rss_kib={} rss_scope={} verification=full-blake3-outside-timing verification_rows={verification_rows}",
        sample.sequence,
        sample.arm,
        sample.phase,
        sample.elapsed_ms,
        throughput,
        sample.workload_bytes,
        metric(sample.transferred),
        metric(sample.source_read),
        sample.rss_kib,
        rss_scope,
    );
    if let (Some(chunk), Some(transfer)) = (sample.chunk_timing, sample.transfer_timing) {
        println!(
            "native_timing sequence={} phase={} scope=cumulative-process-worker-sums walk_ns={} reuse_census_ns={} cdc_hash_ns={} queue_wait_ns={} transfer_ns={} materialize_ns={} publish_groups={} pack_append_ns={} file_syncs={} file_sync_ns={} sqlite_commits={} sqlite_commit_ns={} legacy_put_calls={} legacy_put_ns={} legacy_dir_syncs={} legacy_dir_sync_ns={}",
            sample.sequence,
            sample.phase,
            transfer.walk_ns,
            transfer.reuse_census_ns,
            transfer.cdc_hash_ns,
            transfer.queue_wait_ns,
            transfer.transfer_ns,
            transfer.materialize_ns,
            chunk.publish_groups,
            chunk.pack_append_ns,
            chunk.file_syncs,
            chunk.file_sync_ns,
            chunk.sqlite_commits,
            chunk.sqlite_commit_ns,
            chunk.put_calls,
            chunk.put_ns,
            chunk.dir_syncs,
            chunk.dir_sync_ns,
        );
    }
}

fn run_sample(cli: &Cli, run: &SampleRun<'_>) -> io::Result<Sample> {
    let destination = run.root.join("destination");
    if run.initialize {
        private_dir(&run.root)?;
        private_dir(&destination)?;
    }
    if rows(run.source)? != run.expected {
        return Err(io::Error::other("fixture changed before arm"));
    }
    let chunk_before = ChunkTiming::snapshot();
    let transfer_before = TransferTiming::snapshot();
    let started = Instant::now();
    let (transferred, source_read) = match run.arm {
        Arm::Native => {
            let source_state = run
                .source_state
                .clone()
                .unwrap_or_else(|| run.root.join("source-state"));
            let stats = transfer::copy(
                run.source,
                &destination,
                &source_state,
                &run.root.join("destination-state"),
            )
            .map_err(io::Error::other)?;
            if !stats.refusals.is_empty() {
                return Err(io::Error::other(format!(
                    "native transfer emitted refusals: {:?}",
                    stats.refusals
                )));
            }
            (Some(stats.bytes_received), Some(stats.source_bytes_read))
        }
        Arm::Rclone => {
            rclone_copy(
                cli.rclone
                    .as_deref()
                    .ok_or_else(|| io::Error::other("rclone missing"))?,
                run.source,
                &destination,
            )?;
            (None, None)
        }
    };
    let elapsed = started.elapsed();
    let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
    let rss_kib = max_rss_kib(match run.arm {
        Arm::Native => libc::RUSAGE_SELF,
        Arm::Rclone => libc::RUSAGE_CHILDREN,
    })?;
    let chunk_timing = ChunkTiming::snapshot().since(chunk_before);
    let transfer_timing = TransferTiming::snapshot().since(transfer_before);
    if rows(run.source)? != run.expected || !same_payload(run.expected, &rows(&destination)?) {
        return Err(io::Error::other(
            "source mutation or destination content/mode mismatch",
        ));
    }
    let sample = Sample {
        sequence: run.sequence,
        arm: run.arm,
        phase: run.phase,
        elapsed_ms,
        elapsed_ns: elapsed.as_nanos(),
        transferred,
        source_read,
        rss_kib,
        workload_bytes: run.workload_bytes,
        chunk_timing: (run.arm == Arm::Native).then_some(chunk_timing),
        transfer_timing: (run.arm == Arm::Native).then_some(transfer_timing),
    };
    print_sample(&sample, run.expected.len());
    Ok(sample)
}

fn seed_fixture(sealed_source: &Path, work: &Path) -> io::Result<Fixture> {
    let sealed_expected = rows(sealed_source)?;
    let sealed_identity = corpus_identity(&sealed_expected)?;
    let source = work.join("native-sealed-fixture");
    private_dir(&source)?;
    let stats = transfer::copy(
        sealed_source,
        &source,
        &work.join("fixture-source-state"),
        &work.join("fixture-destination-state"),
    )
    .map_err(io::Error::other)?;
    if !stats.refusals.is_empty() {
        return Err(io::Error::other(format!(
            "native fixture transfer emitted refusals: {:?}",
            stats.refusals
        )));
    }
    if rows(sealed_source)? != sealed_expected {
        return Err(io::Error::other(
            "sealed corpus changed during fixture transfer",
        ));
    }
    let expected = rows(&source)?;
    if !same_payload(&sealed_expected, &expected) {
        return Err(io::Error::other(
            "native fixture payload differs from sealed corpus",
        ));
    }
    Ok(Fixture {
        source,
        private_identity: corpus_identity(&expected)?,
        expected,
        sealed_expected,
        sealed_identity,
        seed_source_read: stats.source_bytes_read,
        seed_received: stats.bytes_received,
    })
}

fn print_header(cli: &Cli, fixture: &Fixture, rclone_identity: &str) {
    println!(
        "benchmark revision={} rclone_version={:?} scope=local-ordinary-file-copy verification=full-blake3-outside-timing cache=not-flushed outputs=retained delta_target=one-percent-regular-file-bytes delta_target_preconditioning=remove-mutated-private-targets-outside-timing sealed_corpus_blake3={} fixture_corpus_blake3={} source_rows={} fixture_seed_source_bytes_read={} fixture_seed_bytes_received={}",
        cli.revision,
        rclone_identity,
        fixture.sealed_identity,
        fixture.private_identity,
        fixture.expected.len(),
        fixture.seed_source_read,
        fixture.seed_received,
    );
    if let Some(rclone) = &cli.rclone {
        println!(
            "rclone_command binary={} args={:?}",
            rclone.display(),
            [
                "copy",
                "SOURCE",
                "DESTINATION",
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
            ]
        );
    }
}

fn run_unchanged_samples(
    cli: &Cli,
    work: &Path,
    fixture: &Fixture,
    arms: &[Arm],
) -> io::Result<Vec<Sample>> {
    let mut samples = Vec::new();
    let initial_bytes = regular_bytes(&fixture.expected);
    for (sequence, arm) in arms.iter().copied().enumerate() {
        samples.push(run_sample(
            cli,
            &SampleRun {
                source: &fixture.source,
                expected: &fixture.expected,
                root: work.join(format!("{sequence}-{arm:?}")),
                source_state: None,
                sequence,
                arm,
                phase: "initial",
                workload_bytes: initial_bytes,
                initialize: true,
            },
        )?);
    }
    for (sequence, arm) in arms.iter().copied().enumerate() {
        samples.push(run_sample(
            cli,
            &SampleRun {
                source: &fixture.source,
                expected: &fixture.expected,
                root: work.join(format!("{sequence}-{arm:?}")),
                source_state: None,
                sequence,
                arm,
                phase: "warm-resume",
                workload_bytes: 0,
                initialize: false,
            },
        )?);
    }
    if samples.iter().any(|sample| {
        sample.arm == Arm::Native
            && sample.phase == "warm-resume"
            && (sample.transferred != Some(0) || sample.source_read != Some(0))
    }) {
        return Err(io::Error::other(
            "warm native resume was not zero-read/zero-transfer",
        ));
    }
    let interrupted_root = work.join("interrupted-native");
    let interrupted_destination = interrupted_root.join("destination");
    private_dir(&interrupted_root)?;
    private_dir(&interrupted_destination)?;
    let source_state = work.join("0-Native/source-state");
    interrupt_after_payload(
        &fixture.source,
        &interrupted_destination,
        &source_state,
        &interrupted_root.join("destination-state"),
    )?;
    let interrupted = run_sample(
        cli,
        &SampleRun {
            source: &fixture.source,
            expected: &fixture.expected,
            root: interrupted_root,
            source_state: Some(source_state),
            sequence: 0,
            arm: Arm::Native,
            phase: "interrupted-resume",
            workload_bytes: 0,
            initialize: false,
        },
    )?;
    if interrupted.transferred != Some(0) || interrupted.source_read != Some(0) {
        return Err(io::Error::other(
            "interrupted native resume was not zero-read/zero-transfer",
        ));
    }
    samples.push(interrupted);
    Ok(samples)
}

fn run_delta_samples(
    cli: &Cli,
    sealed_source: &Path,
    work: &Path,
    fixture: &mut Fixture,
    arms: &[Arm],
    samples: &mut Vec<Sample>,
) -> io::Result<()> {
    let delta = add_one_percent_delta(&fixture.source, &fixture.expected)?;
    if rows(sealed_source)? != fixture.sealed_expected {
        return Err(io::Error::other(
            "sealed corpus changed during delta mutation",
        ));
    }
    fixture.expected = rows(&fixture.source)?;
    if corpus_identity(&fixture.expected)? != delta.after_identity {
        return Err(io::Error::other("delta identity changed before sampling"));
    }
    println!(
        "delta files_changed={} bytes_mutated={} corpus_bytes={} before_blake3={} after_blake3={} sealed_blake3={}",
        delta.files.len(),
        delta.bytes,
        regular_bytes(&fixture.expected),
        delta.before_identity,
        delta.after_identity,
        fixture.sealed_identity,
    );
    for (sequence, arm) in arms.iter().copied().enumerate() {
        let root = work.join(format!("{sequence}-{arm:?}"));
        remove_delta_targets(&root.join("destination"), &delta.files)?;
        samples.push(run_sample(
            cli,
            &SampleRun {
                source: &fixture.source,
                expected: &fixture.expected,
                root,
                source_state: None,
                sequence,
                arm,
                phase: "delta",
                workload_bytes: delta.bytes,
                initialize: false,
            },
        )?);
    }
    Ok(())
}

fn enforce_verdict(cli: &Cli, samples: &[Sample]) -> io::Result<()> {
    if cli.only.is_some() {
        println!("verdict status=diagnostic-only reason=single-arm-run");
        return Ok(());
    }
    let initial_native = median(samples, Arm::Native, "initial")?;
    let initial_rclone = median(samples, Arm::Rclone, "initial")?;
    let delta_native = median(samples, Arm::Native, "delta")?;
    let delta_rclone = median(samples, Arm::Rclone, "delta")?;
    let warm_zero = samples
        .iter()
        .filter(|sample| sample.arm == Arm::Native && sample.phase == "warm-resume")
        .all(|sample| sample.transferred == Some(0) && sample.source_read == Some(0));
    let interrupted_zero = samples
        .iter()
        .filter(|sample| sample.arm == Arm::Native && sample.phase == "interrupted-resume")
        .all(|sample| sample.transferred == Some(0) && sample.source_read == Some(0));
    let rss_ok = samples
        .iter()
        .filter(|sample| sample.arm == Arm::Native)
        .all(|sample| sample.rss_kib < 2 * 1024 * 1024);
    let initial_win = initial_native < initial_rclone;
    let delta_win = delta_native < delta_rclone;
    let passed = initial_win && delta_win && warm_zero && interrupted_zero && rss_ok;
    println!(
        "median phase=initial native_ms={initial_native:.3} rclone_ms={initial_rclone:.3} native_wins={initial_win}"
    );
    println!(
        "median phase=delta native_ms={delta_native:.3} rclone_ms={delta_rclone:.3} native_wins={delta_win}"
    );
    println!(
        "verdict status={} r23_initial_win={} r23_delta_win={} r25_warm_zero={} r25_interrupted_zero={} native_rss_below_2gib={}",
        if passed { "pass" } else { "fail" },
        initial_win,
        delta_win,
        warm_zero,
        interrupted_zero,
        rss_ok,
    );
    if passed {
        Ok(())
    } else {
        Err(io::Error::other("R23/R25 benchmark gate failed"))
    }
}

fn run(cli: &Cli) -> io::Result<()> {
    let (sealed_source, work) = prepare(cli)?;
    let mut fixture = seed_fixture(&sealed_source, &work)?;
    let rclone_identity = cli
        .rclone
        .as_deref()
        .map(rclone_version)
        .transpose()?
        .unwrap_or_else(|| "not-run".to_owned());
    print_header(cli, &fixture, &rclone_identity);
    let arms = arm_order(cli.reps)
        .filter(|arm| cli.only.is_none_or(|only| only == *arm))
        .collect::<Vec<_>>();
    let mut samples = run_unchanged_samples(cli, &work, &fixture, &arms)?;
    run_delta_samples(
        cli,
        &sealed_source,
        &work,
        &mut fixture,
        &arms,
        &mut samples,
    )?;
    enforce_verdict(cli, &samples)
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
    fn delta_mutates_exactly_one_percent_without_touching_sealed_input() -> io::Result<()> {
        let fixture = tempfile::tempdir()?;
        let sealed = fixture.path().join("sealed");
        let private = fixture.path().join("private");
        private_dir(&sealed)?;
        private_dir(&private)?;
        fs::write(sealed.join("a"), vec![0x11; 6_000])?;
        fs::write(sealed.join("b"), vec![0x22; 4_000])?;
        fs::copy(sealed.join("a"), private.join("a"))?;
        fs::copy(sealed.join("b"), private.join("b"))?;
        let sealed_before = rows(&sealed)?;
        let private_before = rows(&private)?;

        let delta = add_one_percent_delta(&private, &private_before)?;

        assert_eq!(delta.bytes, 100);
        assert_eq!(delta.files, vec![b"a".to_vec()]);
        assert_eq!(rows(&sealed)?, sealed_before);
        let changed = fs::read(private.join("a"))?;
        assert!(changed
            .get(..100)
            .is_some_and(|bytes| bytes.iter().all(|byte| *byte == 0xb4)));
        assert_eq!(changed.get(100), Some(&0x11));
        Ok(())
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
            revision: "test-revision".to_owned(),
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
