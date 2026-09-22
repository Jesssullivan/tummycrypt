//! Explicit reviewed Git batches. Capture reuse still performs a metadata census.
//!
//! Ref custody and usable restored workspaces are separate outcomes. Completed
//! restores are never replayed over subsequent operator edits.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{git_carry, BulkloadRefusal, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub source: PathBuf,
    pub repository: PathBuf,
    pub workspace: Option<PathBuf>,
}

#[derive(Default, Serialize, Deserialize)]
struct Plan {
    items: Vec<Item>,
}

#[derive(Serialize, Deserialize)]
struct Capture {
    key: [u8; 32],
    bundle: String,
    digest: [u8; 32],
    identity: crate::freshness::StatIdentity,
}

// Separate sidecars preserve the existing Capture postcard wire layout.
#[derive(Clone, Serialize, Deserialize)]
struct Base {
    bundle: String,
    digest: [u8; 32],
    identity: crate::freshness::StatIdentity,
}

#[derive(Debug)]
pub struct Receipt {
    pub item: String,
    pub source: PathBuf,
    pub outcome: &'static str,
    pub reason: Option<String>,
    /// One line per ref or seat that drifted under the capture this receipt
    /// names, followed by any seat the apply destination already held that
    /// the drift list says was never captured (R-N29). Empty is the ordinary
    /// case. The durable statement is the `{bundle}.drift` sidecar.
    pub drift: Vec<String>,
    /// Bytes streamed from source file descriptors by this operation. A reuse
    /// hit and an apply read no source bytes; an incremental pass reads
    /// exactly the drifted seats (R25).
    pub bytes_read: u64,
}

/// One item's completed operation and what it carried.
struct Completion {
    outcome: &'static str,
    drift: Vec<String>,
    held: Vec<String>,
    bytes_read: u64,
}

impl Completion {
    const fn clean(outcome: &'static str) -> Self {
        Self {
            outcome,
            drift: Vec::new(),
            held: Vec::new(),
            bytes_read: 0,
        }
    }
}

// The authority digest of the key parts a capture was taken under. A later
// pass with the same authority extends that capture: only the ref inventory
// and the worktree census differ, and the census difference is exactly the
// set of seats the incremental pass re-reads. Same sidecar shape as `Base`.
#[derive(Serialize, Deserialize)]
struct Parts {
    authority: [u8; 32],
}

// A drifted capture deliberately omits the drifted seats' bytes, so its sidecar
// is the record of what it does not hold. Absent means the pass raced nothing.
fn retained_drift(corpus: &Path, bundle: &str) -> Result<git_carry::CaptureDrift> {
    let path = corpus.join(format!("{bundle}.drift"));
    if path.try_exists()? {
        read(&path)
    } else {
        Ok(git_carry::CaptureDrift::default())
    }
}

// Absent for captures retained from before this sidecar existed: such a
// capture is still reused blob-for-blob, it just never reports as extended.
fn retained_authority(corpus: &Path, bundle: &str) -> Result<Option<[u8; 32]>> {
    let path = corpus.join(format!("{bundle}.parts"));
    if path.try_exists()? {
        Ok(Some(read::<Parts>(&path)?.authority))
    } else {
        Ok(None)
    }
}

/// Read the exact reviewed items without performing capture or apply.
///
/// # Errors
/// Refuses malformed, oversized or symlinked plans.
pub fn inspect(plan: &Path) -> Result<Vec<Item>> {
    Ok(read::<Plan>(plan)?.items)
}

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    postcard::from_bytes(&bytes).map_err(|_| BulkloadRefusal::FrameCodec)
}

fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = postcard::to_allocvec(value).map_err(|_| BulkloadRefusal::FrameCodec)?;
    // All callers hold a plan or phase lock. Retain interrupted publications
    // rather than overwrite their bytes or make them a permanent resume blocker.
    let mut generation = 0u64;
    let (temporary, mut file) = loop {
        let temporary = path.with_extension(format!("pending-{generation}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                generation = generation
                    .checked_add(1)
                    .ok_or(BulkloadRefusal::FieldDomainViolation)?;
            }
            Err(error) => return Err(error.into()),
        }
    };
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    fs::File::open(path.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)?.sync_all()?;
    Ok(())
}

fn private_directory(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            // SAFETY: geteuid has no preconditions or side effects.
            if metadata.is_dir()
                && metadata.mode().trailing_zeros() >= 6
                && metadata.uid() == unsafe { libc::geteuid() }
            {
                Ok(())
            } else {
                Err(BulkloadRefusal::PathEscapesRoot)
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn filename(value: &str) -> bool {
    let mut parts = Path::new(value).components();
    matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none()
}

fn id(item: &Item) -> Result<String> {
    let bytes = postcard::to_allocvec(item).map_err(|_| BulkloadRefusal::FrameCodec)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Append one operator-selected source and destination. No inventory guesses.
///
/// # Errors
/// Refuses invalid paths, conflicting workspace targets, locked or malformed plans.
pub fn add(plan: &Path, source: &Path, repository: &Path, workspace: Option<&Path>) -> Result<()> {
    add_batch(
        plan,
        &[Item {
            source: source.to_owned(),
            repository: repository.to_owned(),
            workspace: workspace.map(Path::to_owned),
        }],
    )
}

/// Append a reviewed batch with one plan read/write and linear duplicate checks.
///
/// # Errors
/// Refuses malformed paths or conflicting targets without publishing a partial plan.
pub fn add_batch(plan: &Path, items: &[Item]) -> Result<()> {
    if !plan.is_absolute() {
        return Err(BulkloadRefusal::PathNotAbsolute);
    }
    let _lock = exclusive(&plan.with_extension("lock"))?;
    let mut contents: Plan = if plan.try_exists()? {
        read(plan)?
    } else {
        Plan::default()
    };
    let mut identities = std::collections::HashSet::new();
    let mut targets = std::collections::HashSet::new();
    for previous in &contents.items {
        identities.insert(id(previous)?);
        if let Some(target) = &previous.workspace {
            targets.insert(target.clone());
        }
    }
    for incoming in items {
        let mut item = incoming.clone();
        item.source = fs::canonicalize(&item.source)?;
        if !item.repository.is_absolute()
            || item.workspace.as_ref().is_some_and(|p| !p.is_absolute())
        {
            return Err(BulkloadRefusal::PathNotAbsolute);
        }
        if !identities.insert(id(&item)?) {
            continue;
        }
        if let Some(target) = &item.workspace {
            if !targets.insert(target.clone()) {
                return Err(BulkloadRefusal::GitDestinationOccupied);
            }
        }
        contents.items.push(item);
    }
    write(plan, &contents)
}

struct Exclusive(fs::File);

impl Drop for Exclusive {
    fn drop(&mut self) {
        // A concurrent fork can briefly inherit the open file description until
        // exec closes it. Explicit unlock ends our operation's ownership even
        // while such a descriptor exists; closing only this fd is insufficient.
        // SAFETY: this guard owns a live descriptor for the acquired flock.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn exclusive(path: &Path) -> Result<Exclusive> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    // SAFETY: the owned file descriptor remains open for the lock lifetime.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(Exclusive(file))
}

fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(buffer.get(..count).ok_or(BulkloadRefusal::FrameCodec)?);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn base_path(corpus: &Path, base: &Base) -> Result<PathBuf> {
    if !filename(&base.bundle) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    Ok(corpus.join(&base.bundle))
}

fn retained_base(corpus: &Path, base: &Base) -> Result<bool> {
    let path = base_path(corpus, base)?;
    Ok(path.try_exists()?
        && base.identity
            == crate::freshness::StatIdentity::from_metadata(&fs::symlink_metadata(path)?))
}

fn prepare_base(item: &Item, group: &str, state: &Path, corpus: &Path) -> Result<Base> {
    let record = corpus.join(format!("shared-{group}.base"));
    if record.try_exists()? {
        let base: Base = read(&record)?;
        if retained_base(corpus, &base)? {
            return Ok(base);
        }
        // Do not replace a missing or changed prerequisite while older deltas
        // still depend on it. Keep the missing custody visible.
        return Err(BulkloadRefusal::ReceiptBindingInvalid);
    }
    let mut generation = 0u64;
    let attempt = loop {
        let attempt = state.join(format!("shared-{group}-{generation}"));
        if !attempt.try_exists()? {
            break attempt;
        }
        generation = generation
            .checked_add(1)
            .ok_or(BulkloadRefusal::FieldDomainViolation)?;
    };
    let bundle = git_carry::shared::export_base(&item.source, &attempt)?;
    let digest = hash_file(&bundle)?;
    let name = format!(
        "shared-{}.bundle",
        blake3::Hash::from_bytes(digest).to_hex()
    );
    let published = corpus.join(&name);
    if published.try_exists()? {
        if hash_file(&published)? != digest {
            return Err(BulkloadRefusal::DigestMismatch);
        }
    } else {
        fs::hard_link(&bundle, &published)?;
    }
    // Git's successful pack write is not a durability guarantee. Flush the
    // payload before write() publishes and directory-syncs its dependency.
    fs::File::open(&published)?.sync_all()?;
    let base = Base {
        bundle: name,
        digest,
        identity: crate::freshness::StatIdentity::from_metadata(&fs::symlink_metadata(published)?),
    };
    write(&record, &base)?;
    Ok(base)
}

/// What a retained capture record offers the next pass.
enum Retained {
    /// Same key, no drift: the retained bundle is the capture.
    Hit,
    /// A retained bundle of this checkout whose blobs this pass may reuse:
    /// every seat at an unchanged `StatIdentity` costs zero source bytes (R25).
    /// `extends` says the difference is confined to the ref inventory and
    /// the worktree census, so this pass extends rather than starts over.
    Extend { bundle: PathBuf, extends: bool },
    /// Nothing retained.
    None,
}

fn retained_capture(
    record: &Path,
    corpus: &Path,
    key: [u8; 32],
    authority: [u8; 32],
) -> Result<Retained> {
    if !record.try_exists()? {
        return Ok(Retained::None);
    }
    let previous: Capture = read(record)?;
    if !filename(&previous.bundle) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    let bundle = corpus.join(&previous.bundle);
    if !bundle.try_exists()?
        || previous.identity
            != crate::freshness::StatIdentity::from_metadata(&fs::symlink_metadata(&bundle)?)
    {
        return Ok(Retained::None);
    }
    let drift = retained_drift(corpus, &previous.bundle)?;
    // A drifted bundle does not hold the drifted seats' bytes. It is never a
    // reuse hit; it is the input the next pass extends (R-N28).
    if previous.key == key && drift.is_empty() {
        if git_carry::shared::requires_base(&bundle)? {
            let bound: Base = read(&corpus.join(format!("{}.base", previous.bundle)))?;
            if !retained_base(corpus, &bound)? {
                return Err(BulkloadRefusal::ReceiptBindingInvalid);
            }
        }
        return Ok(Retained::Hit);
    }
    let extends = retained_authority(corpus, &previous.bundle)? == Some(authority);
    Ok(Retained::Extend { bundle, extends })
}

fn capture_item(
    item: &Item,
    state: &Path,
    corpus: &Path,
    base: Option<&Base>,
    policy: git_carry::CapturePolicy,
) -> Result<Completion> {
    let identity = id(item)?;
    let record = corpus.join(format!("{identity}.capture"));
    // The opaque key cannot say what moved. Keep its typed parts so the
    // post-capture re-read can separate tolerable drift from Git authority.
    let parts = git_carry::capture_key_parts_with_policy(&item.source, policy)?;
    let key = parts.digest()?;
    let authority = parts.authority()?;
    let (retained, extends) = match retained_capture(&record, corpus, key, authority)? {
        Retained::Hit => return Ok(Completion::clean("capture-reused-after-census")),
        Retained::Extend { bundle, extends } => (Some(bundle), extends),
        Retained::None => (None, false),
    };
    let mut generation = 0u64;
    let attempt = loop {
        let candidate = state.join(format!(
            "{identity}-{}-{generation}",
            blake3::Hash::from_bytes(key).to_hex()
        ));
        if !candidate.try_exists()? {
            break candidate;
        }
        generation = generation
            .checked_add(1)
            .ok_or(BulkloadRefusal::FieldDomainViolation)?;
    };
    // Failed private attempts are retained, never silently overwritten.
    let prerequisite = base.map(|base| base_path(corpus, base)).transpose()?;
    let export = git_carry::export_repository_with_drift(
        &item.source,
        &attempt,
        &git_carry::ExportOptions {
            prerequisite: prerequisite.as_deref(),
            policy,
            reuse: retained.as_deref(),
        },
    )?;
    // Not an opaque key comparison: the ref inventory and the worktree census
    // moving is the drift this capture already reported. Anything else moving
    // is Git authority changing under the capture and still refuses (R-N30).
    if !parts.drift_only(&git_carry::capture_key_parts_with_policy(
        &item.source,
        policy,
    )?) {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    let bundle = export.bundle;
    let digest = hash_file(&bundle)?;
    let name = format!(
        "{identity}-{}.bundle",
        blake3::Hash::from_bytes(digest).to_hex()
    );
    let published = corpus.join(&name);
    if published.try_exists()? {
        if hash_file(&published)? != digest {
            return Err(BulkloadRefusal::DigestMismatch);
        }
    } else {
        fs::hard_link(&bundle, &published)?;
    }
    // Completion may survive a crash only after its bundle bytes are durable.
    fs::File::open(&published)?.sync_all()?;
    let metadata = fs::symlink_metadata(&published)?;
    if let Some(base) = base {
        // Publish dependency custody before the unchanged completion codec.
        write(&corpus.join(format!("{name}.base")), base)?;
    }
    // Separate sidecars, exactly as the shared-base dependency is: the Capture
    // postcard is positional and gains no field, so every retained record and
    // every live restore journal still decodes.
    if !export.drift.is_empty() {
        write(&corpus.join(format!("{name}.drift")), &export.drift)?;
    }
    write(&corpus.join(format!("{name}.parts")), &Parts { authority })?;
    // The pre-pass key is recorded, as it always was. When the pass drifted it
    // no longer matches the source, so the next pass is an incremental one that
    // re-reads exactly the drifted seats and reuses every other blob.
    write(
        &record,
        &Capture {
            key,
            bundle: name,
            digest,
            identity: crate::freshness::StatIdentity::from_metadata(&metadata),
        },
    )?;
    let outcome = if !export.drift.is_empty() {
        "captured-with-drift"
    } else if extends {
        "capture-extended-from-drift"
    } else {
        "captured"
    };
    Ok(Completion {
        outcome,
        drift: export.drift.lines(),
        held: Vec::new(),
        bytes_read: export.bytes_read,
    })
}

fn execute(
    plan: &Plan,
    jobs: usize,
    operation: &(impl Fn(&Item) -> Result<Completion> + Sync),
    receipt: &(impl Fn(&Receipt) -> Result<()> + Sync),
) -> Result<()> {
    if !(1..=2).contains(&jobs) {
        return Err(BulkloadRefusal::FieldDomainViolation);
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .map_err(|_| BulkloadRefusal::FieldDomainViolation)?;
    let refused = std::sync::atomic::AtomicBool::new(false);
    pool.install(|| {
        plan.items.par_iter().try_for_each(|item| {
            let (outcome, reason, drift, bytes_read) = match operation(item) {
                Ok(done) => {
                    // The count rides in the layout-safe reason; the rows ride
                    // in the sidecar and the in-process receipt.
                    let reason =
                        (!done.drift.is_empty()).then(|| format!("drift={}", done.drift.len()));
                    let mut lines = done.drift;
                    lines.extend(done.held);
                    (done.outcome, reason, lines, done.bytes_read)
                }
                Err(error) => {
                    refused.store(true, std::sync::atomic::Ordering::Relaxed);
                    ("refused", Some(error.to_string()), Vec::new(), 0)
                }
            };
            receipt(&Receipt {
                item: id(item)?,
                source: item.source.clone(),
                outcome,
                reason,
                drift,
                bytes_read,
            })
        })
    })?;
    if refused.load(std::sync::atomic::Ordering::Relaxed) {
        Err(BulkloadRefusal::ContractSelfInconsistent)
    } else {
        Ok(())
    }
}

fn emit(state: &Path, row: &Receipt, receipt: &impl Fn(&Receipt) -> Result<()>) -> Result<()> {
    write(
        &state.join(format!("{}.outcome", row.item)),
        &(&row.source, row.outcome, &row.reason),
    )?;
    receipt(row)
}

#[derive(Default)]
struct CaptureGroups {
    items: std::collections::BTreeMap<String, String>,
    bases: std::collections::BTreeMap<String, Mutex<Option<Base>>>,
}

fn capture_groups(plan: &Plan) -> Result<CaptureGroups> {
    let mut candidates = std::collections::BTreeMap::<String, Vec<String>>::new();
    for item in &plan.items {
        // Invalid sources still run through the ordinary per-item refusal path
        // so one bad item cannot suppress receipts for unrelated valid work.
        let Ok(common) = git_carry::common_repository(&item.source) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&common) else {
            continue;
        };
        let bytes = postcard::to_allocvec(&(common, metadata.dev(), metadata.ino()))
            .map_err(|_| BulkloadRefusal::FrameCodec)?;
        candidates
            .entry(blake3::hash(&bytes).to_hex().to_string())
            .or_default()
            .push(id(item)?);
    }
    let mut groups = CaptureGroups::default();
    for (group, items) in candidates {
        if items.len() > 1 {
            for item in items {
                groups.items.insert(item, group.clone());
            }
            groups.bases.insert(group, Mutex::new(None));
        }
    }
    Ok(groups)
}

fn group_base(
    item: &Item,
    groups: &CaptureGroups,
    state: &Path,
    corpus: &Path,
) -> Result<Option<Base>> {
    let Some(group) = groups.items.get(&id(item)?) else {
        return Ok(None);
    };
    let mut base = groups
        .bases
        .get(group)
        .ok_or(BulkloadRefusal::GitAuthorityChanged)?
        .lock()
        .map_err(|_| BulkloadRefusal::GitAuthorityChanged)?;
    if base.is_none() {
        *base = Some(prepare_base(item, group, state, corpus)?);
    }
    // Drop the base-creation lock before the independent workspace capture.
    Ok(base.clone())
}

/// Capture explicit items with at most two Git pack workers at a time.
///
/// # Errors
/// Refuses insecure state directories, changing sources or failed captures; completed
/// items remain durable and each failed item is reported independently.
pub fn capture(
    plan: &Path,
    state: &Path,
    corpus: &Path,
    jobs: usize,
    receipt: &(impl Fn(&Receipt) -> Result<()> + Sync),
) -> Result<()> {
    capture_with_policy(
        plan,
        state,
        corpus,
        jobs,
        git_carry::CapturePolicy::default(),
        receipt,
    )
}

/// [`capture`] under an explicit capture policy.
///
/// A policy that carries the rebuildable set reproduces the pre-omission
/// capture keys, so retained full-fidelity captures are still reused.
///
/// # Errors
/// Refuses everything [`capture`] refuses.
pub fn capture_with_policy(
    plan: &Path,
    state: &Path,
    corpus: &Path,
    jobs: usize,
    policy: git_carry::CapturePolicy,
    receipt: &(impl Fn(&Receipt) -> Result<()> + Sync),
) -> Result<()> {
    private_directory(state)?;
    private_directory(corpus)?;
    let contents: Plan = read(plan)?;
    let _lock = exclusive(&state.join("estate.lock"))?;
    let groups = capture_groups(&contents)?;
    execute(
        &contents,
        jobs,
        &|item| {
            let base = group_base(item, &groups, state, corpus)?;
            capture_item(item, state, corpus, base.as_ref(), policy)
        },
        &|row| emit(state, row, receipt),
    )
}

type ImportedBases = Mutex<std::collections::BTreeSet<(PathBuf, [u8; 32])>>;

fn import_base(
    item: &Item,
    captured: &Capture,
    corpus: &Path,
    source: &str,
    imported: &ImportedBases,
) -> Result<()> {
    let bundle = corpus.join(&captured.bundle);
    if !git_carry::shared::requires_base(&bundle)? {
        return Ok(());
    }
    let base: Base = read(&corpus.join(format!("{}.base", captured.bundle)))?;
    let path = base_path(corpus, &base)?;
    // Existing shared repositories are the supported optimization. Creating a
    // standalone destination needs a separate private preseed implementation.
    if !item.repository.try_exists()?
        || item
            .workspace
            .as_ref()
            .is_some_and(|workspace| workspace == &item.repository)
    {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let key = (git_carry::common_repository(&item.repository)?, base.digest);
    if imported
        .lock()
        .map_err(|_| BulkloadRefusal::GitAuthorityChanged)?
        .contains(&key)
    {
        return Ok(());
    }
    if hash_file(&path)? != base.digest || git_carry::shared::requires_base(&path)? {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    git_carry::import_bundle(&item.repository, &path, source)?;
    imported
        .lock()
        .map_err(|_| BulkloadRefusal::GitAuthorityChanged)?
        .insert(key);
    Ok(())
}

// R-N29: an apply proceeds when the destination already holds a seat the drift
// list says was never captured, and names each such seat in its receipt. It
// never writes over it: the captured tree does not contain that seat at all.
fn held_uncaptured(workspace: &Path, drift: &git_carry::CaptureDrift) -> Vec<String> {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    let mut held = Vec::new();
    for row in drift.rows.iter().filter(|row| row.kind.is_seat()) {
        let relative = Path::new(std::ffi::OsStr::from_bytes(&row.name));
        if relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            continue;
        }
        if fs::symlink_metadata(workspace.join(relative)).is_ok() {
            // Debug-escaped on purpose: receipts must not carry raw newlines.
            #[allow(clippy::unnecessary_debug_formatting)]
            held.push(format!("HeldUncaptured {relative:?}"));
        }
    }
    held
}

fn apply_item(
    item: &Item,
    corpus: &Path,
    state: &Path,
    source: &str,
    imported: &ImportedBases,
) -> Result<Completion> {
    let identity = id(item)?;
    let captured: Capture = read(&corpus.join(format!("{identity}.capture")))?;
    if !filename(&captured.bundle) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    // The drift a capture recorded rides into every receipt that names its
    // bundle, so an apply never silently presents an incomplete capture.
    let drift = retained_drift(corpus, &captured.bundle)?;
    let journal = state.join(format!(
        "{identity}-{}-{}.done",
        blake3::hash(source.as_bytes()).to_hex(),
        blake3::Hash::from_bytes(captured.digest).to_hex()
    ));
    if journal.try_exists()? {
        let done: String = read(&journal)?;
        let outcome = match done.as_str() {
            "workspace-restored" => "previous-workspace-restoration-not-revalidated",
            "refs-imported" => "previous-ref-custody-not-workspace-parity",
            _ => return Err(BulkloadRefusal::ReceiptBindingInvalid),
        };
        return Ok(Completion {
            outcome,
            drift: drift.lines(),
            held: Vec::new(),
            bytes_read: 0,
        });
    }
    let bundle = corpus.join(&captured.bundle);
    if hash_file(&bundle)? != captured.digest {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    import_base(item, &captured, corpus, source, imported)?;
    let (outcome, held) = if let Some(workspace) = &item.workspace {
        if item.repository == *workspace {
            git_carry::restore_bundle(&bundle, workspace, source)?;
        } else {
            git_carry::restore_linked(&bundle, &item.repository, workspace, source)?;
        }
        ("workspace-restored", held_uncaptured(workspace, &drift))
    } else {
        git_carry::import_bundle(&item.repository, &bundle, source)?;
        ("refs-imported", Vec::new())
    };
    write(&journal, &outcome.to_owned())?;
    Ok(Completion {
        outcome,
        drift: drift.lines(),
        held,
        bytes_read: 0,
    })
}

/// Apply explicit restores only; common Git administration is serialized.
///
/// # Errors
/// Refuses occupied restore targets, invalid bundles or conflicting Git authority.
/// Previously completed work is retained without replaying over operator edits.
pub fn apply(
    plan: &Path,
    corpus: &Path,
    state: &Path,
    source: &str,
    jobs: usize,
    receipt: &(impl Fn(&Receipt) -> Result<()> + Sync),
) -> Result<()> {
    private_directory(state)?;
    let _lock = exclusive(&state.join("estate.lock"))?;
    let contents: Plan = read(plan)?;
    let mut locks = std::collections::BTreeMap::new();
    let mut groups = std::collections::BTreeMap::new();
    let imported = ImportedBases::default();
    for item in &contents.items {
        let common = if item.repository.try_exists()? {
            git_carry::common_repository(&item.repository)?
        } else {
            item.repository.clone()
        };
        groups.insert(id(item)?, common.clone());
        locks.entry(common).or_insert_with(|| Mutex::new(()));
    }
    execute(
        &contents,
        jobs,
        &|item| {
            let common = groups
                .get(&id(item)?)
                .ok_or(BulkloadRefusal::GitAuthorityChanged)?;
            let lock = locks
                .get(common)
                .ok_or(BulkloadRefusal::GitAuthorityChanged)?;
            let _guard = lock
                .lock()
                .map_err(|_| BulkloadRefusal::GitAuthorityChanged)?;
            apply_item(item, corpus, state, source, &imported)
        },
        &|row| emit(state, row, receipt),
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    fn git(path: &Path, args: &[&str]) {
        assert!(Command::new("git")
            .arg("-C")
            .arg(path)
            .args([
                "-c",
                "user.name=Bulkload test",
                "-c",
                "user.email=test@localhost",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null"
            ])
            .args(args)
            .output()
            .expect("git command")
            .status
            .success());
    }

    #[test]
    fn shared_capture_keeps_completion_codec_and_requires_retained_base() {
        let root = std::env::temp_dir().join(format!("tcfs-estate-shared-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "--template="]);
        fs::write(source.join("file"), b"base").unwrap();
        git(&source, &["add", "file"]);
        git(&source, &["commit", "-m", "base"]);
        let second = root.join("second");
        git(
            &source,
            &[
                "worktree",
                "add",
                "--detach",
                second.to_str().unwrap(),
                "HEAD",
            ],
        );
        fs::write(source.join("file"), b"first dirty").unwrap();
        fs::write(second.join("file"), b"second dirty").unwrap();
        let repository = root.join("repository");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "--template="]);
        let first_target = root.join("first-target");
        let second_target = root.join("second-target");
        let plan = root.join("plan");
        add(&plan, &source, &repository, Some(&first_target)).unwrap();
        add(&plan, &second, &repository, Some(&second_target)).unwrap();
        let state = root.join("state");
        let corpus = root.join("corpus");
        capture(&plan, &state, &corpus, 2, &|_| Ok(())).unwrap();
        let outcomes = Mutex::new(Vec::new());
        capture(&plan, &state, &corpus, 2, &|row| {
            outcomes.lock().unwrap().push(row.outcome);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            *outcomes.lock().unwrap(),
            vec!["capture-reused-after-census"; 2]
        );
        let items = inspect(&plan).unwrap();
        let mut dependencies = Vec::new();
        for item in &items {
            // Decode through the unchanged completion struct used by existing
            // independent standalone captures and their live restore journals.
            let record: Capture =
                read(&corpus.join(format!("{}.capture", id(item).unwrap()))).unwrap();
            let base: Base = read(&corpus.join(format!("{}.base", record.bundle))).unwrap();
            dependencies.push(base.bundle);
        }
        assert_eq!(dependencies.first(), dependencies.last());
        let base = corpus.join(dependencies.first().unwrap());
        let held = root.join("held-base");
        fs::rename(&base, &held).unwrap();
        assert!(capture(&plan, &state, &corpus, 2, &|_| Ok(())).is_err());
        let applied = root.join("applied");
        assert!(apply(&plan, &corpus, &applied, "neo", 2, &|_| Ok(())).is_err());
        assert!(!first_target.exists() && !second_target.exists());
        fs::rename(&held, &base).unwrap();
        apply(&plan, &corpus, &applied, "neo", 2, &|_| Ok(())).unwrap();
        assert_eq!(fs::read(first_target.join("file")).unwrap(), b"first dirty");
        assert_eq!(
            fs::read(second_target.join("file")).unwrap(),
            b"second dirty"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_operation_unlocks_even_with_an_inherited_description() {
        let root = std::env::temp_dir().join(format!("tcfs-estate-lock-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let path = root.join("estate.lock");
        let owner = exclusive(&path).unwrap();
        let inherited = owner.0.try_clone().unwrap();
        assert!(exclusive(&path).is_err());
        drop(owner);
        let next = exclusive(&path).expect("completed owner explicitly unlocked");
        drop(inherited);
        assert!(exclusive(&path).is_err());
        drop(next);
        assert!(exclusive(&path).is_ok());
        fs::remove_file(path).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn explicit_batches_reuse_capture_preserve_edits_and_refuse_public_corpus() {
        let root = std::env::temp_dir().join(format!("tcfs-estate-test-{}", std::process::id()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("owned root");
        let source = root.join("source");
        fs::create_dir(&source).expect("source");
        git(&source, &["init", "--template="]);
        fs::write(source.join("file"), b"base").expect("base");
        git(&source, &["add", "file"]);
        git(&source, &["commit", "-m", "base"]);
        fs::write(source.join("file"), b"dirty").expect("dirty");
        let target = root.join("destination");
        let plan = root.join("plan");
        let interrupted = plan.with_extension("pending-0");
        fs::write(&interrupted, b"retained interrupted plan").expect("simulate interrupted write");
        add(&plan, &source, &target, Some(&target)).expect("plan");
        assert_eq!(
            fs::read(&interrupted).expect("retained"),
            b"retained interrupted plan"
        );
        add(&plan, &source, &target, Some(&target)).expect("idempotent plan");
        assert_eq!(read::<Plan>(&plan).expect("read plan").items.len(), 1);
        let state = root.join("state");
        let corpus = root.join("corpus");
        capture(&plan, &state, &corpus, 2, &|_| Ok(())).expect("capture");
        let outcomes = Mutex::new(Vec::new());
        capture(&plan, &state, &corpus, 2, &|row| {
            outcomes.lock().expect("lock").push(row.outcome);
            Ok(())
        })
        .expect("reuse");
        assert_eq!(
            *outcomes.lock().expect("lock"),
            vec!["capture-reused-after-census"]
        );
        let applied = root.join("applied");
        apply(&plan, &corpus, &applied, "neo", 2, &|_| Ok(())).expect("restore");
        assert_eq!(
            fs::read(target.join("file")).expect("dirty restored"),
            b"dirty"
        );
        fs::write(target.join("file"), b"operator edited").expect("active write");
        apply(&plan, &corpus, &applied, "neo", 2, &|_| Ok(())).expect("do not replay");
        assert_eq!(
            fs::read(target.join("file")).expect("preserved"),
            b"operator edited"
        );
        let invalid = root.join("not-a-repository");
        fs::create_dir(&invalid).expect("invalid source");
        add(&plan, &invalid, &target, None).expect("explicit failing item");
        outcomes.lock().expect("lock").clear();
        assert!(capture(&plan, &state, &corpus, 2, &|row| {
            outcomes.lock().expect("lock").push(row.outcome);
            Ok(())
        })
        .is_err());
        let mut partial = outcomes.lock().expect("lock").clone();
        partial.sort_unstable();
        assert_eq!(partial, vec!["capture-reused-after-census", "refused"]);
        fs::set_permissions(&corpus, fs::Permissions::from_mode(0o755)).expect("public corpus");
        assert!(capture(&plan, &state, &corpus, 2, &|_| Ok(())).is_err());
        assert!(!filename("/"));
        assert!(!filename("../escape"));
        fs::remove_dir_all(&root).expect("remove owned fixture");
    }

    // ---- drift tolerance (R25, bulkload #34; rulings R-N28/R-N29/R-N30) ----

    fn drifting_plan(name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("tcfs-estate-drift-{name}-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "--template="]);
        fs::write(source.join("tracked"), b"tracked bytes at census").unwrap();
        git(&source, &["add", "tracked"]);
        git(&source, &["commit", "-m", "base"]);
        fs::write(source.join("big"), vec![b'b'; 65_536]).unwrap();
        fs::write(source.join("small"), b"small untracked").unwrap();
        let target = root.join("destination");
        let plan = root.join("plan");
        add(&plan, &source, &target, Some(&target)).unwrap();
        let corpus = root.join("corpus");
        (root, source, target, plan, corpus)
    }

    type Row = (&'static str, Option<String>, Vec<String>, u64);

    fn receipts(plan: &Path, state: &Path, corpus: &Path) -> Result<Vec<Row>> {
        let rows = Mutex::new(Vec::new());
        capture(plan, state, corpus, 2, &|row| {
            rows.lock().unwrap().push((
                row.outcome,
                row.reason.clone(),
                row.drift.clone(),
                row.bytes_read,
            ));
            Ok(())
        })?;
        Ok(rows.into_inner().unwrap())
    }

    // Arm the export-level hook so one estate capture races a rewrite of a
    // tracked seat, a new seat, and a branch created in the shared ref store.
    fn arm_drift(source: &Path) {
        let inside = fs::canonicalize(source).unwrap();
        git_carry::mid_pass::arm(source, move || {
            fs::write(inside.join("tracked"), b"rewritten mid-pass").unwrap();
            fs::write(inside.join("appeared"), b"new seat").unwrap();
            git(&inside, &["update-ref", "refs/heads/lane-a", "HEAD"]);
        });
    }

    #[test]
    fn a_drifted_capture_reports_captured_with_drift_and_exits_zero() {
        let (root, source, _, plan, corpus) = drifting_plan("exit-zero");
        let state = root.join("state");
        arm_drift(&source);
        let rows = receipts(&plan, &state, &corpus).expect("Ok(()): drift is not a refusal");
        assert_eq!(rows.len(), 1);
        let (outcome, reason, drift, bytes_read) = rows.first().unwrap();
        assert_eq!(*outcome, "captured-with-drift");
        assert_eq!(reason.as_deref(), Some("drift=3"));
        assert_eq!(
            *drift,
            vec![
                "RefAdded \"refs/heads/lane-a\"".to_owned(),
                "SeatAdded \"appeared\"".to_owned(),
                "SeatChanged \"tracked\"".to_owned(),
            ]
        );
        // The skipped seat cost nothing; everything else was read once.
        assert_eq!(*bytes_read, 65_536 + b"small untracked".len() as u64);
        // The durable outcome still decodes as the existing tuple, unchanged.
        let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
        let durable: (PathBuf, String, Option<String>) =
            read(&state.join(format!("{item}.outcome"))).unwrap();
        assert_eq!(durable.0, fs::canonicalize(&source).unwrap());
        assert_eq!(durable.1, "captured-with-drift");
        assert_eq!(durable.2.as_deref(), Some("drift=3"));
        // The Capture record is the unchanged codec; the rows ride beside it.
        let record: Capture = read(&corpus.join(format!("{item}.capture"))).unwrap();
        let sidecar: git_carry::CaptureDrift =
            read(&corpus.join(format!("{}.drift", record.bundle))).unwrap();
        assert_eq!(sidecar.len(), 3);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_drifted_capture_still_applies_and_its_drift_rides_into_the_receipt() {
        let (root, source, target, plan, corpus) = drifting_plan("applies");
        let state = root.join("state");
        arm_drift(&source);
        let rows = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(rows.first().unwrap().0, "captured-with-drift");
        let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
        let record: Capture = read(&corpus.join(format!("{item}.capture"))).unwrap();
        assert!(corpus.join(format!("{}.drift", record.bundle)).is_file());
        let applied = root.join("applied");
        let receipts = Mutex::new(Vec::new());
        apply(&plan, &corpus, &applied, "neo", 2, &|row| {
            receipts
                .lock()
                .unwrap()
                .push((row.outcome, row.reason.clone(), row.drift.clone()));
            Ok(())
        })
        .unwrap();
        let (outcome, reason, drift) = receipts.lock().unwrap().first().unwrap().clone();
        assert_eq!(outcome, "workspace-restored");
        assert_eq!(reason.as_deref(), Some("drift=3"));
        assert!(drift.contains(&"SeatChanged \"tracked\"".to_owned()));
        // No seat named in drift was restored with uncaptured bytes: the
        // drifted seats are absent, and the destination held nothing (R-N29).
        assert!(!target.join("tracked").exists());
        assert!(!target.join("appeared").exists());
        assert!(!drift.iter().any(|line| line.starts_with("HeldUncaptured")));
        assert_eq!(fs::read(target.join("small")).unwrap(), b"small untracked");
        assert_eq!(fs::read(target.join("big")).unwrap(), vec![b'b'; 65_536]);
        // R-N29: a destination that already holds an uncaptured seat is named,
        // never clobbered. The apply journal is done; the check is the receipt's.
        fs::write(target.join("appeared"), b"operator wrote this").unwrap();
        let held = held_uncaptured(
            &target,
            &read(&corpus.join(format!("{}.drift", record.bundle))).unwrap(),
        );
        assert_eq!(held, vec!["HeldUncaptured \"appeared\"".to_owned()]);
        assert_eq!(
            fs::read(target.join("appeared")).unwrap(),
            b"operator wrote this"
        );
        fs::remove_dir_all(root).unwrap();
    }

    // The guard for the journals under /srv/fast-local/jess/state/git-carry/neo/
    // and every retained bundle in the corpus: a record whose key was computed
    // by the pre-change hashing, with no sidecars, is still a reuse hit.
    #[test]
    fn retained_captures_from_before_this_change_are_still_reused() {
        let (root, source, _, plan, corpus) = drifting_plan("retained");
        let state = root.join("state");
        let rows = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(rows.first().unwrap().0, "captured");
        let item = id(inspect(&plan).unwrap().first().unwrap()).unwrap();
        let record_path = corpus.join(format!("{item}.capture"));
        let record: Capture = read(&record_path).unwrap();
        let legacy = git_carry::legacy::reusable_capture_key_with_policy(
            &source,
            git_carry::CapturePolicy::default(),
        )
        .unwrap();
        assert_eq!(record.key, legacy, "KeyParts::digest must be bit-identical");
        // Rewrite the record exactly as the pre-change agent would have written
        // it, and remove the sidecars it never wrote.
        fs::remove_file(corpus.join(format!("{}.parts", record.bundle))).unwrap();
        fs::remove_file(&record_path).unwrap();
        write(
            &record_path,
            &Capture {
                key: legacy,
                bundle: record.bundle.clone(),
                digest: record.digest,
                identity: record.identity,
            },
        )
        .unwrap();
        let rows = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(rows.first().unwrap().0, "capture-reused-after-census");
        assert_eq!(rows.first().unwrap().3, 0);
        fs::remove_dir_all(root).unwrap();
    }

    // The R25 proof: pass 2 reads exactly the drifted seats, not the corpus.
    #[test]
    fn a_second_pass_after_drift_rereads_only_the_drifted_seats() {
        let (root, source, target, plan, corpus) = drifting_plan("rereads");
        let state = root.join("state");
        arm_drift(&source);
        let first = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(first.first().unwrap().0, "captured-with-drift");
        let second = receipts(&plan, &state, &corpus).unwrap();
        let (outcome, reason, drift, bytes_read) = second.first().unwrap();
        assert_eq!(*outcome, "capture-extended-from-drift");
        assert_eq!(*reason, None);
        assert!(drift.is_empty());
        assert_eq!(
            *bytes_read,
            (b"rewritten mid-pass".len() + b"new seat".len()) as u64,
            "pass 2 must read the drifted seats and nothing else"
        );
        let third = receipts(&plan, &state, &corpus).unwrap();
        assert_eq!(third.first().unwrap().0, "capture-reused-after-census");
        assert_eq!(third.first().unwrap().3, 0);
        // The extended bundle is complete: reused blobs and re-read seats alike.
        apply(&plan, &corpus, &root.join("applied"), "neo", 2, &|_| Ok(())).unwrap();
        assert_eq!(
            fs::read(target.join("tracked")).unwrap(),
            b"rewritten mid-pass"
        );
        assert_eq!(fs::read(target.join("appeared")).unwrap(), b"new seat");
        assert_eq!(fs::read(target.join("big")).unwrap(), vec![b'b'; 65_536]);
        assert_eq!(fs::read(target.join("small")).unwrap(), b"small untracked");
        fs::remove_dir_all(root).unwrap();
    }
}
