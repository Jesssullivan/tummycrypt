//! Strict, read-only local snapshots for registered-root planning.
//!
//! This module deliberately does not reuse the primary sync collector. A
//! registered-root plan needs one bounded, verified observation, not a best-effort
//! traversal that can follow renamed path components or update primary state.
//! The supported Linux implementation therefore:
//!
//! 1. opens the canonical configured root one component at a time without
//!    following symlinks;
//! 2. records a descriptor-relative identity inventory;
//! 3. captures every included leaf while checking its descriptor and parent
//!    identities before and after the read;
//! 4. records a second identity inventory and requires exact equality;
//! 5. keeps the configured-root descriptor alive while state and remote inputs
//!    are acquired;
//! 6. records inventory C through that same descriptor; and
//! 7. reopens the configured route and requires the original root identity.
//!
//! Acquisition identities make races fail closed but are intentionally absent
//! from the portable semantic digest. Descriptor, size, mtime, and ctime
//! brackets detect ordinary concurrent writers on the supported filesystem;
//! they are not a hostile-writer lease or a kernel filesystem snapshot. A
//! future executor must replan under its execution lock rather than treating
//! this read-only artifact as enduring authority.

use crate::blacklist::Blacklist;
use crate::index_entry::{portable_casefold_path, validate_namespace_logical_path};
use anyhow::Result;
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use tcfs_core::config::{
    RegisteredRootPlanContractFingerprintV1, RegisteredRootPlanContractV1,
    RootLocalSnapshotContractV1, RootProfileSettingsFingerprintV1, RootProfileV1,
};

const LOCAL_SNAPSHOT_DIGEST_DOMAIN_V1: &str = "tinyland.tcfs.registered-root-local-snapshot.b3v1";
const LOCAL_SNAPSHOT_ACQUISITION_FINGERPRINT_DOMAIN_V1: &str =
    "tinyland.tcfs.registered-root-local-snapshot-acquisition.b3v1";
const LOCAL_SNAPSHOT_SEMANTIC_ENCODING_FINGERPRINT_DOMAIN_V1: &str =
    "tinyland.tcfs.registered-root-local-snapshot-semantic-encoding.b3v1";
const LOCAL_SNAPSHOT_SEMANTIC_ENCODING_NAME_V1: &str =
    "portable-derived-directory-regular-symlink-v1";
const LOCAL_SNAPSHOT_CONTRACT_V1: RootLocalSnapshotContractV1 =
    RegisteredRootPlanContractV1::strict_v1().local_snapshot_contract();
const MAX_DEPTH_V1: usize = LOCAL_SNAPSHOT_CONTRACT_V1.max_depth() as usize;
const MAX_ENTRIES_V1: usize = LOCAL_SNAPSHOT_CONTRACT_V1.max_entries() as usize;
const MAX_RETAINED_PATH_BYTES_V1: usize =
    LOCAL_SNAPSHOT_CONTRACT_V1.max_retained_path_bytes() as usize;
const MAX_SYMLINK_TARGET_BYTES_V1: usize =
    LOCAL_SNAPSHOT_CONTRACT_V1.max_symlink_target_bytes() as usize;
const MAX_REGULAR_FILE_BYTES_V1: u64 = LOCAL_SNAPSHOT_CONTRACT_V1.max_regular_file_bytes();
const MAX_TOTAL_HASHED_BYTES_V1: u64 = LOCAL_SNAPSHOT_CONTRACT_V1.max_total_hashed_bytes();
const HASH_BUFFER_BYTES_V1: usize = LOCAL_SNAPSHOT_CONTRACT_V1.hash_buffer_bytes() as usize;

/// Portable BLAKE3 identity of one complete local semantic snapshot.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrictLocalSnapshotDigestV1([u8; 32]);

impl StrictLocalSnapshotDigestV1 {
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for StrictLocalSnapshotDigestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("b3v1:")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for StrictLocalSnapshotDigestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "StrictLocalSnapshotDigestV1({self})")
    }
}

/// Opaque identity of the acquisition proof and its resource bounds.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrictLocalSnapshotAcquisitionFingerprintV1([u8; 32]);

impl StrictLocalSnapshotAcquisitionFingerprintV1 {
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for StrictLocalSnapshotAcquisitionFingerprintV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("b3v1:")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for StrictLocalSnapshotAcquisitionFingerprintV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "StrictLocalSnapshotAcquisitionFingerprintV1({self})"
        )
    }
}

/// Portable identity of the semantic entry encoding, independent of the
/// platform-specific acquisition proof and resource limits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrictLocalSnapshotSemanticEncodingFingerprintV1([u8; 32]);

impl StrictLocalSnapshotSemanticEncodingFingerprintV1 {
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for StrictLocalSnapshotSemanticEncodingFingerprintV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("b3v1:")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for StrictLocalSnapshotSemanticEncodingFingerprintV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "StrictLocalSnapshotSemanticEncodingFingerprintV1({self})"
        )
    }
}

/// Immutable metadata and raw-byte identity of an included regular file.
#[derive(Clone, PartialEq, Eq)]
pub struct StrictLocalRegularFileV1 {
    raw_blake3: [u8; 32],
    size: u64,
    portable_mode: u32,
    mtime_seconds: i64,
    mtime_nanoseconds: u32,
}

impl StrictLocalRegularFileV1 {
    pub const fn raw_blake3(&self) -> &[u8; 32] {
        &self.raw_blake3
    }

    pub const fn size(&self) -> u64 {
        self.size
    }

    pub const fn portable_mode(&self) -> u32 {
        self.portable_mode
    }

    pub const fn mtime_seconds(&self) -> i64 {
        self.mtime_seconds
    }

    pub const fn mtime_nanoseconds(&self) -> u32 {
        self.mtime_nanoseconds
    }
}

impl fmt::Debug for StrictLocalRegularFileV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StrictLocalRegularFileV1")
            .field("raw_blake3", &blake3::Hash::from_bytes(self.raw_blake3))
            .field("size", &self.size)
            .field(
                "portable_mode",
                &format_args!("{:#05o}", self.portable_mode),
            )
            .field("mtime_seconds", &self.mtime_seconds)
            .field("mtime_nanoseconds", &self.mtime_nanoseconds)
            .finish()
    }
}

/// Exact, validated target of an included symbolic link.
///
/// `Debug` deliberately exposes only length and digest. The exact target is
/// available through the explicit accessor for planning, never accidentally
/// through diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub struct StrictLocalSymlinkV1 {
    target: String,
    target_blake3: [u8; 32],
}

impl StrictLocalSymlinkV1 {
    pub fn target(&self) -> &str {
        &self.target
    }

    pub const fn target_blake3(&self) -> &[u8; 32] {
        &self.target_blake3
    }
}

impl fmt::Debug for StrictLocalSymlinkV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StrictLocalSymlinkV1")
            .field("target_len", &self.target.len())
            .field(
                "target_blake3",
                &blake3::Hash::from_bytes(self.target_blake3),
            )
            .finish()
    }
}

/// One portable namespace role in a complete local snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StrictLocalEntryV1 {
    Directory,
    Regular(StrictLocalRegularFileV1),
    Symlink(StrictLocalSymlinkV1),
}

impl StrictLocalEntryV1 {
    pub const fn canonical_kind_name(&self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::Regular(_) => "regular",
            Self::Symlink(_) => "symlink",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StrictLocalNamespaceRoleV1 {
    File,
    Directory,
}

/// Acquisition-only namespace evidence retained for the future cross-input
/// composer. Ignored empty directories live here, not in the semantic digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StrictLocalNamespaceClaimV1 {
    exact_path: String,
    role: StrictLocalNamespaceRoleV1,
}

impl StrictLocalNamespaceClaimV1 {
    pub(crate) fn exact_path(&self) -> &str {
        &self.exact_path
    }

    pub(crate) const fn role(&self) -> StrictLocalNamespaceRoleV1 {
        self.role
    }
}

/// A complete, race-checked local semantic snapshot.
///
/// "Complete" is scoped to this one local acquisition. It is not a complete
/// registered-root plan and carries no state/remote composition proof.
#[derive(Clone, PartialEq, Eq)]
pub struct CompleteStrictLocalSnapshotV1 {
    profile: RootProfileV1,
    profile_settings_fingerprint: RootProfileSettingsFingerprintV1,
    plan_contract_fingerprint: RegisteredRootPlanContractFingerprintV1,
    acquisition_fingerprint: StrictLocalSnapshotAcquisitionFingerprintV1,
    semantic_encoding_fingerprint: StrictLocalSnapshotSemanticEncodingFingerprintV1,
    namespace_claims: BTreeMap<String, StrictLocalNamespaceClaimV1>,
    entries: BTreeMap<String, StrictLocalEntryV1>,
    digest: StrictLocalSnapshotDigestV1,
}

impl CompleteStrictLocalSnapshotV1 {
    pub const fn profile(&self) -> RootProfileV1 {
        self.profile
    }

    pub const fn profile_settings_fingerprint(&self) -> RootProfileSettingsFingerprintV1 {
        self.profile_settings_fingerprint
    }

    pub const fn acquisition_contract_name(&self) -> &'static str {
        LOCAL_SNAPSHOT_CONTRACT_V1.canonical_name()
    }

    pub const fn plan_contract_fingerprint(&self) -> RegisteredRootPlanContractFingerprintV1 {
        self.plan_contract_fingerprint
    }

    pub const fn acquisition_fingerprint(&self) -> StrictLocalSnapshotAcquisitionFingerprintV1 {
        self.acquisition_fingerprint
    }

    pub const fn semantic_encoding_name(&self) -> &'static str {
        LOCAL_SNAPSHOT_SEMANTIC_ENCODING_NAME_V1
    }

    pub const fn semantic_encoding_fingerprint(
        &self,
    ) -> StrictLocalSnapshotSemanticEncodingFingerprintV1 {
        self.semantic_encoding_fingerprint
    }

    pub fn entries(&self) -> impl ExactSizeIterator<Item = (&str, &StrictLocalEntryV1)> {
        self.entries
            .iter()
            .map(|(path, entry)| (path.as_str(), entry))
    }

    pub fn entry(&self, rel_path: &str) -> Option<&StrictLocalEntryV1> {
        self.entries.get(rel_path)
    }

    pub(crate) fn namespace_claims(
        &self,
    ) -> impl ExactSizeIterator<Item = (&str, &StrictLocalNamespaceClaimV1)> {
        self.namespace_claims
            .iter()
            .map(|(folded_path, claim)| (folded_path.as_str(), claim))
    }

    pub const fn digest(&self) -> StrictLocalSnapshotDigestV1 {
        self.digest
    }
}

impl fmt::Debug for CompleteStrictLocalSnapshotV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompleteStrictLocalSnapshotV1")
            .field("profile", &self.profile)
            .field(
                "profile_settings_fingerprint",
                &self.profile_settings_fingerprint,
            )
            .field("plan_contract_fingerprint", &self.plan_contract_fingerprint)
            .field("acquisition_fingerprint", &self.acquisition_fingerprint)
            .field(
                "semantic_encoding_fingerprint",
                &self.semantic_encoding_fingerprint,
            )
            .field("namespace_claim_count", &self.namespace_claims.len())
            .field("entries", &self.entries)
            .field("digest", &self.digest)
            .finish()
    }
}

/// Stable class of a fail-closed, digest-less acquisition result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrictLocalSnapshotIncompleteKindV1 {
    UnsupportedPlatform,
    InvalidConfiguredRoot,
    ConfiguredRootTrustRejected,
    FilesystemReadFailed,
    AcquisitionLimitExceeded,
    NamespaceRejected,
    GitLayoutRejected,
    HardlinkRejected,
    DuplicateIdentityRejected,
    UnsupportedObjectRejected,
    UnsafeModeRejected,
    SymlinkTargetRejected,
    MountBoundaryRejected,
    ChangedDuringRead,
}

/// Typed, non-sensitive explanation for an incomplete local snapshot.
#[derive(Clone, PartialEq, Eq)]
pub struct StrictLocalSnapshotIncompleteV1 {
    kind: StrictLocalSnapshotIncompleteKindV1,
    rel_path: Option<String>,
    operation: &'static str,
}

impl StrictLocalSnapshotIncompleteV1 {
    pub const fn kind(&self) -> StrictLocalSnapshotIncompleteKindV1 {
        self.kind
    }

    pub fn rel_path(&self) -> Option<&str> {
        self.rel_path.as_deref()
    }

    pub const fn operation(&self) -> &'static str {
        self.operation
    }
}

impl fmt::Debug for StrictLocalSnapshotIncompleteV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StrictLocalSnapshotIncompleteV1")
            .field("kind", &self.kind)
            .field("rel_path", &self.rel_path)
            .field("operation", &self.operation)
            .finish()
    }
}

/// A strict snapshot is either complete with a digest or incomplete without
/// one. Callers cannot manufacture a digest for an incomplete acquisition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StrictLocalSnapshotReadV1 {
    Complete(CompleteStrictLocalSnapshotV1),
    Incomplete(StrictLocalSnapshotIncompleteV1),
}

impl StrictLocalSnapshotReadV1 {
    pub fn complete(&self) -> Option<&CompleteStrictLocalSnapshotV1> {
        match self {
            Self::Complete(snapshot) => Some(snapshot),
            Self::Incomplete(_) => None,
        }
    }

    pub fn incomplete(&self) -> Option<&StrictLocalSnapshotIncompleteV1> {
        match self {
            Self::Complete(_) => None,
            Self::Incomplete(incomplete) => Some(incomplete),
        }
    }
}

/// One provisional local observation whose configured-root descriptor remains
/// live.
///
/// The provisional snapshot deliberately exposes no digest. Only
/// [`PendingStrictLocalSnapshotV1::revalidate_inventory_c`] can turn it into a
/// complete snapshot, after inventory C and the configured-route identity
/// check succeed.
pub(crate) struct PendingStrictLocalSnapshotV1 {
    #[cfg(target_os = "linux")]
    inner: Box<supported::PendingSupportedSnapshotV1>,
    #[cfg(not(target_os = "linux"))]
    unsupported: std::convert::Infallible,
}

impl PendingStrictLocalSnapshotV1 {
    /// Exact canonical spelling whose descriptor is held by this acquisition.
    pub(crate) fn canonical_local_root(&self) -> &Path {
        #[cfg(target_os = "linux")]
        {
            self.inner.canonical_local_root()
        }

        #[cfg(not(target_os = "linux"))]
        match self.unsupported {}
    }

    /// Revalidate the local acquisition only after all external reads finish.
    pub(crate) fn revalidate_inventory_c(self) -> Result<StrictLocalSnapshotFinishV1> {
        #[cfg(target_os = "linux")]
        {
            Ok(match supported::finish_supported(*self.inner) {
                Ok(snapshot) => {
                    StrictLocalSnapshotFinishV1::Complete(RevalidatedStrictLocalSnapshotV1 {
                        inner: Box::new(snapshot),
                    })
                }
                Err(failure) => StrictLocalSnapshotFinishV1::Incomplete(failure.into_public()),
            })
        }

        #[cfg(not(target_os = "linux"))]
        match self.unsupported {}
    }
}

/// Inventory-C-validated local input that still owns the original root
/// descriptor. Cross-input composition may inspect the complete snapshot while
/// retaining the capability; compatibility callers explicitly consume it.
pub(crate) struct RevalidatedStrictLocalSnapshotV1 {
    #[cfg(target_os = "linux")]
    inner: Box<supported::RevalidatedSupportedSnapshotV1>,
    #[cfg(not(target_os = "linux"))]
    unsupported: std::convert::Infallible,
}

impl RevalidatedStrictLocalSnapshotV1 {
    pub(crate) fn canonical_local_root(&self) -> &Path {
        #[cfg(target_os = "linux")]
        {
            self.inner.canonical_local_root()
        }

        #[cfg(not(target_os = "linux"))]
        match self.unsupported {}
    }

    pub(crate) fn snapshot(&self) -> &CompleteStrictLocalSnapshotV1 {
        #[cfg(target_os = "linux")]
        {
            self.inner.snapshot()
        }

        #[cfg(not(target_os = "linux"))]
        match self.unsupported {}
    }

    fn into_snapshot(self) -> CompleteStrictLocalSnapshotV1 {
        #[cfg(target_os = "linux")]
        {
            (*self.inner).into_snapshot()
        }

        #[cfg(not(target_os = "linux"))]
        match self.unsupported {}
    }
}

/// Starting a strict acquisition either yields a held root capability or a
/// typed, digest-less failure.
pub(crate) enum StrictLocalSnapshotHoldReadV1 {
    Pending(PendingStrictLocalSnapshotV1),
    Incomplete(StrictLocalSnapshotIncompleteV1),
}

pub(crate) enum StrictLocalSnapshotFinishV1 {
    Complete(RevalidatedStrictLocalSnapshotV1),
    Incomplete(StrictLocalSnapshotIncompleteV1),
}

pub(crate) fn begin_strict_local_snapshot_v1(
    canonical_local_root: &Path,
    profile: RootProfileV1,
) -> Result<StrictLocalSnapshotHoldReadV1> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (canonical_local_root, profile);
        return Ok(StrictLocalSnapshotHoldReadV1::Incomplete(
            CaptureFailure::root(
                StrictLocalSnapshotIncompleteKindV1::UnsupportedPlatform,
                "platform-check",
            )
            .into_public(),
        ));
    }

    #[cfg(target_os = "linux")]
    {
        Ok(
            match supported::begin_supported(canonical_local_root, profile) {
                Ok(held) => StrictLocalSnapshotHoldReadV1::Pending(PendingStrictLocalSnapshotV1 {
                    inner: Box::new(held),
                }),
                Err(failure) => StrictLocalSnapshotHoldReadV1::Incomplete(failure.into_public()),
            },
        )
    }
}

#[derive(Debug)]
struct CaptureFailure {
    kind: StrictLocalSnapshotIncompleteKindV1,
    rel_path: Option<String>,
    operation: &'static str,
}

impl CaptureFailure {
    fn root(kind: StrictLocalSnapshotIncompleteKindV1, operation: &'static str) -> Self {
        Self {
            kind,
            rel_path: None,
            operation,
        }
    }

    fn path(
        kind: StrictLocalSnapshotIncompleteKindV1,
        rel_path: impl Into<String>,
        operation: &'static str,
    ) -> Self {
        Self {
            kind,
            rel_path: Some(rel_path.into()),
            operation,
        }
    }

    fn into_public(self) -> StrictLocalSnapshotIncompleteV1 {
        StrictLocalSnapshotIncompleteV1 {
            kind: self.kind,
            rel_path: self.rel_path,
            operation: self.operation,
        }
    }
}

/// Capture one strict, read-only local snapshot.
///
/// Expected policy, platform, I/O, and race failures are returned as a typed
/// `Incomplete` value with no digest. The outer `Result` is retained for
/// composition with the registered-root planner and for future internal
/// invariant errors.
pub fn capture_strict_local_snapshot_v1(
    canonical_local_root: &Path,
    profile: RootProfileV1,
) -> Result<StrictLocalSnapshotReadV1> {
    match begin_strict_local_snapshot_v1(canonical_local_root, profile)? {
        StrictLocalSnapshotHoldReadV1::Pending(pending) => {
            match pending.revalidate_inventory_c()? {
                StrictLocalSnapshotFinishV1::Complete(revalidated) => Ok(
                    StrictLocalSnapshotReadV1::Complete(revalidated.into_snapshot()),
                ),
                StrictLocalSnapshotFinishV1::Incomplete(incomplete) => {
                    Ok(StrictLocalSnapshotReadV1::Incomplete(incomplete))
                }
            }
        }
        StrictLocalSnapshotHoldReadV1::Incomplete(incomplete) => {
            Ok(StrictLocalSnapshotReadV1::Incomplete(incomplete))
        }
    }
}

struct LengthFramedHasherV1 {
    hasher: blake3::Hasher,
}

impl LengthFramedHasherV1 {
    fn new(domain: &'static str) -> Self {
        Self {
            hasher: blake3::Hasher::new_derive_key(domain),
        }
    }

    fn field(&mut self, tag: &'static str, value: &[u8]) {
        self.hasher.update(&(tag.len() as u32).to_be_bytes());
        self.hasher.update(tag.as_bytes());
        self.hasher.update(&(value.len() as u64).to_be_bytes());
        self.hasher.update(value);
    }

    fn finish(self) -> [u8; 32] {
        *self.hasher.finalize().as_bytes()
    }
}

fn local_snapshot_acquisition_fingerprint_v1() -> StrictLocalSnapshotAcquisitionFingerprintV1 {
    let mut encoder = LengthFramedHasherV1::new(LOCAL_SNAPSHOT_ACQUISITION_FINGERPRINT_DOMAIN_V1);
    let plan_contract = RegisteredRootPlanContractV1::strict_v1();
    encoder.field(
        "contract_name",
        LOCAL_SNAPSHOT_CONTRACT_V1.canonical_name().as_bytes(),
    );
    encoder.field(
        "plan_contract_fingerprint",
        plan_contract.fingerprint().as_bytes(),
    );
    encoder.field(
        "mount_boundary_policy",
        LOCAL_SNAPSHOT_CONTRACT_V1
            .mount_boundary_policy()
            .canonical_name()
            .as_bytes(),
    );
    encoder.field(
        "identity_acquisition",
        b"descriptor-relative-no-follow-stable-identity-content-identity-v1",
    );
    encoder.field(
        "stability",
        b"inventory-a-leaf-bracket-inventory-b-initial-reopen-held-read-inventory-c-final-reopen-v1",
    );
    encoder.field("namespace", b"portable-nfc-casefold-file-directory-role-v1");
    encoder.field("regular", b"raw-blake3-exact-size-eof-mode-mtime-v1");
    encoder.field("symlink", b"exact-utf8-safe-relative-target-v1");
    encoder.field(
        "excluded_inventory",
        b"excluded-root-identity-pruned-subtree-v1",
    );
    encoder.field("max_depth", &(MAX_DEPTH_V1 as u64).to_be_bytes());
    encoder.field("max_entries", &(MAX_ENTRIES_V1 as u64).to_be_bytes());
    encoder.field(
        "max_retained_path_bytes",
        &(MAX_RETAINED_PATH_BYTES_V1 as u64).to_be_bytes(),
    );
    encoder.field(
        "max_symlink_target_bytes",
        &(MAX_SYMLINK_TARGET_BYTES_V1 as u64).to_be_bytes(),
    );
    encoder.field(
        "max_regular_file_bytes",
        &MAX_REGULAR_FILE_BYTES_V1.to_be_bytes(),
    );
    encoder.field(
        "max_total_hashed_bytes",
        &MAX_TOTAL_HASHED_BYTES_V1.to_be_bytes(),
    );
    encoder.field(
        "hash_buffer_bytes",
        &(HASH_BUFFER_BYTES_V1 as u64).to_be_bytes(),
    );
    StrictLocalSnapshotAcquisitionFingerprintV1(encoder.finish())
}

fn local_snapshot_semantic_encoding_fingerprint_v1(
) -> StrictLocalSnapshotSemanticEncodingFingerprintV1 {
    let mut encoder =
        LengthFramedHasherV1::new(LOCAL_SNAPSHOT_SEMANTIC_ENCODING_FINGERPRINT_DOMAIN_V1);
    encoder.field(
        "encoding_name",
        LOCAL_SNAPSHOT_SEMANTIC_ENCODING_NAME_V1.as_bytes(),
    );
    encoder.field(
        "path_encoding",
        b"utf8-nfc-portable-casefold-canonical-byte-order-v1",
    );
    encoder.field(
        "directory_encoding",
        b"derived-ancestors-of-included-leaves-v1",
    );
    encoder.field(
        "regular_encoding",
        b"raw-blake3-size-portable-mode-mtime-seconds-nanoseconds-v1",
    );
    encoder.field(
        "symlink_encoding",
        b"exact-utf8-target-and-raw-target-blake3-v1",
    );
    encoder.field("field_framing", b"u32-tag-length-u64-value-length-v1");
    StrictLocalSnapshotSemanticEncodingFingerprintV1(encoder.finish())
}

fn semantic_snapshot_digest_v1(
    profile: RootProfileV1,
    profile_settings_fingerprint: RootProfileSettingsFingerprintV1,
    semantic_encoding_fingerprint: StrictLocalSnapshotSemanticEncodingFingerprintV1,
    entries: &BTreeMap<String, StrictLocalEntryV1>,
) -> StrictLocalSnapshotDigestV1 {
    let mut encoder = LengthFramedHasherV1::new(LOCAL_SNAPSHOT_DIGEST_DOMAIN_V1);
    encoder.field(
        "semantic_encoding_name",
        LOCAL_SNAPSHOT_SEMANTIC_ENCODING_NAME_V1.as_bytes(),
    );
    encoder.field(
        "semantic_encoding_fingerprint",
        semantic_encoding_fingerprint.as_bytes(),
    );
    encoder.field("profile", profile.canonical_name().as_bytes());
    encoder.field(
        "profile_settings_fingerprint",
        profile_settings_fingerprint.as_bytes(),
    );
    encoder.field("entry_count", &(entries.len() as u64).to_be_bytes());
    for (path, entry) in entries {
        encoder.field("path", path.as_bytes());
        encoder.field("kind", entry.canonical_kind_name().as_bytes());
        match entry {
            StrictLocalEntryV1::Directory => {}
            StrictLocalEntryV1::Regular(regular) => {
                encoder.field("raw_blake3", regular.raw_blake3());
                encoder.field("size", &regular.size().to_be_bytes());
                encoder.field("portable_mode", &regular.portable_mode().to_be_bytes());
                encoder.field("mtime_seconds", &regular.mtime_seconds().to_be_bytes());
                encoder.field(
                    "mtime_nanoseconds",
                    &regular.mtime_nanoseconds().to_be_bytes(),
                );
            }
            StrictLocalEntryV1::Symlink(symlink) => {
                encoder.field("target", symlink.target().as_bytes());
                encoder.field("target_blake3", symlink.target_blake3());
            }
        }
    }
    StrictLocalSnapshotDigestV1(encoder.finish())
}

#[cfg(target_os = "linux")]
mod supported {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::{CStr, CString, OsString};
    use std::fs::File;
    use std::io::Read;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::{Component, Path, PathBuf};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct StableIdentity {
        dev: u64,
        ino: u64,
        mode: u32,
        nlink: u64,
        uid: u32,
        gid: u32,
        rdev: u64,
        size: i64,
        mtime_seconds: i64,
        mtime_nanoseconds: i64,
        ctime_seconds: i64,
        ctime_nanoseconds: i64,
    }

    impl StableIdentity {
        fn object_kind(self) -> ObjectKind {
            match self.mode & libc::S_IFMT {
                libc::S_IFDIR => ObjectKind::Directory,
                libc::S_IFREG => ObjectKind::Regular,
                libc::S_IFLNK => ObjectKind::Symlink,
                _ => ObjectKind::Special,
            }
        }

        fn dev_ino(self) -> (u64, u64) {
            (self.dev, self.ino)
        }

        fn has_special_permissions(self) -> bool {
            self.mode & 0o7000 != 0
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum ObjectKind {
        Directory,
        Regular,
        Symlink,
        Special,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Disposition {
        Included,
        Excluded,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct InventoryRecord {
        identity: StableIdentity,
        disposition: Disposition,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct IdentityInventory {
        root: StableIdentity,
        root_mount_id: u64,
        entries: BTreeMap<Vec<u8>, InventoryRecord>,
    }

    struct InventoryContext<'a> {
        profile: RootProfileV1,
        entries: BTreeMap<Vec<u8>, InventoryRecord>,
        expected: Option<&'a IdentityInventory>,
        observed_count: usize,
        retained_path_bytes: usize,
        directory_identities: BTreeSet<(u64, u64)>,
        included_leaf_identities: BTreeSet<(u64, u64)>,
        total_hashed_bytes: u64,
        saw_root_git_directory: bool,
    }

    struct InventoryDirectoryFrame {
        directory: File,
        stream: DirectoryStream,
        components: Vec<Vec<u8>>,
        parent_excluded: bool,
        depth: usize,
        expected_identity_after_walk: Option<StableIdentity>,
    }

    impl<'a> InventoryContext<'a> {
        fn collecting(profile: RootProfileV1, root: StableIdentity) -> Self {
            let mut directory_identities = BTreeSet::new();
            directory_identities.insert(root.dev_ino());
            Self {
                profile,
                entries: BTreeMap::new(),
                expected: None,
                observed_count: 0,
                retained_path_bytes: 0,
                directory_identities,
                included_leaf_identities: BTreeSet::new(),
                total_hashed_bytes: 0,
                saw_root_git_directory: false,
            }
        }

        fn verifying(
            profile: RootProfileV1,
            root: StableIdentity,
            expected: &'a IdentityInventory,
        ) -> Self {
            let mut context = Self::collecting(profile, root);
            context.expected = Some(expected);
            context
        }

        fn retained_entry_count(&self) -> usize {
            if self.expected.is_some() {
                self.observed_count
            } else {
                self.entries.len()
            }
        }
    }

    pub(super) struct PendingSupportedSnapshotV1 {
        canonical_local_root: PathBuf,
        profile: RootProfileV1,
        root: File,
        root_identity: StableIdentity,
        root_mount_id: u64,
        inventory_a: IdentityInventory,
        namespace_claims: BTreeMap<String, StrictLocalNamespaceClaimV1>,
        entries: BTreeMap<String, StrictLocalEntryV1>,
    }

    impl PendingSupportedSnapshotV1 {
        pub(super) fn canonical_local_root(&self) -> &Path {
            &self.canonical_local_root
        }
    }

    pub(super) struct RevalidatedSupportedSnapshotV1 {
        canonical_local_root: PathBuf,
        _root: File,
        snapshot: CompleteStrictLocalSnapshotV1,
    }

    impl RevalidatedSupportedSnapshotV1 {
        pub(super) fn canonical_local_root(&self) -> &Path {
            &self.canonical_local_root
        }

        pub(super) fn snapshot(&self) -> &CompleteStrictLocalSnapshotV1 {
            &self.snapshot
        }

        pub(super) fn into_snapshot(self) -> CompleteStrictLocalSnapshotV1 {
            self.snapshot
        }
    }

    pub(super) fn begin_supported(
        canonical_local_root: &Path,
        profile: RootProfileV1,
    ) -> std::result::Result<PendingSupportedSnapshotV1, CaptureFailure> {
        validate_configured_root(canonical_local_root)?;
        crate::conflict_git::validate_trusted_configured_path(canonical_local_root).map_err(
            |_| {
                CaptureFailure::root(
                    StrictLocalSnapshotIncompleteKindV1::ConfiguredRootTrustRejected,
                    "validate-configured-root-trust",
                )
            },
        )?;

        let root = open_absolute_directory(canonical_local_root, "open-configured-root")?;
        let root_identity = fstat_identity(root.as_raw_fd(), None, "stat-configured-root")?;
        let root_mount_id = statx_mount_id(root.as_raw_fd(), None, "statx-configured-root")?;
        if root_identity.object_kind() != ObjectKind::Directory {
            return Err(CaptureFailure::root(
                StrictLocalSnapshotIncompleteKindV1::InvalidConfiguredRoot,
                "configured-root-not-directory",
            ));
        }

        let inventory_a = inventory_tree(&root, root_identity, root_mount_id, profile)?;
        let namespace_claims = portable_namespace_claims(&inventory_a.entries)?;
        run_test_hook(TestHookPoint::InventoryAToLeaf);
        let entries = capture_semantic_entries(&root, &inventory_a)?;
        run_test_hook(TestHookPoint::LeafToInventoryB);
        verify_inventory_tree(&root, root_identity, root_mount_id, profile, &inventory_a)?;

        run_test_hook(TestHookPoint::RootPathBeforeReopen);
        verify_configured_root_route(
            canonical_local_root,
            root_identity,
            root_mount_id,
            "revalidate-initial-configured-root-trust",
            "initial-reopen-configured-root",
            "initial-restat-configured-root",
            "initial-restatx-configured-root",
            "compare-initial-reopened-root-identity",
        )?;

        Ok(PendingSupportedSnapshotV1 {
            canonical_local_root: canonical_local_root.to_owned(),
            profile,
            root,
            root_identity,
            root_mount_id,
            inventory_a,
            namespace_claims,
            entries,
        })
    }

    pub(super) fn finish_supported(
        held: PendingSupportedSnapshotV1,
    ) -> std::result::Result<RevalidatedSupportedSnapshotV1, CaptureFailure> {
        verify_inventory_tree(
            &held.root,
            held.root_identity,
            held.root_mount_id,
            held.profile,
            &held.inventory_a,
        )?;

        verify_configured_root_route(
            &held.canonical_local_root,
            held.root_identity,
            held.root_mount_id,
            "revalidate-final-configured-root-trust",
            "final-reopen-configured-root",
            "final-restat-configured-root",
            "final-restatx-configured-root",
            "compare-final-reopened-root-identity",
        )?;

        let policy = held.profile.policy();
        let profile_settings_fingerprint = policy.settings_fingerprint();
        let plan_contract_fingerprint = RegisteredRootPlanContractV1::strict_v1().fingerprint();
        let acquisition_fingerprint = local_snapshot_acquisition_fingerprint_v1();
        let semantic_encoding_fingerprint = local_snapshot_semantic_encoding_fingerprint_v1();
        let digest = semantic_snapshot_digest_v1(
            held.profile,
            profile_settings_fingerprint,
            semantic_encoding_fingerprint,
            &held.entries,
        );
        Ok(RevalidatedSupportedSnapshotV1 {
            canonical_local_root: held.canonical_local_root,
            _root: held.root,
            snapshot: CompleteStrictLocalSnapshotV1 {
                profile: held.profile,
                profile_settings_fingerprint,
                plan_contract_fingerprint,
                acquisition_fingerprint,
                semantic_encoding_fingerprint,
                namespace_claims: held.namespace_claims,
                entries: held.entries,
                digest,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn verify_configured_root_route(
        canonical_local_root: &Path,
        expected_identity: StableIdentity,
        expected_mount_id: u64,
        trust_operation: &'static str,
        open_operation: &'static str,
        stat_operation: &'static str,
        statx_operation: &'static str,
        compare_operation: &'static str,
    ) -> std::result::Result<(), CaptureFailure> {
        validate_configured_root(canonical_local_root)?;
        crate::conflict_git::validate_trusted_configured_path(canonical_local_root).map_err(
            |_| {
                CaptureFailure::root(
                    StrictLocalSnapshotIncompleteKindV1::ConfiguredRootTrustRejected,
                    trust_operation,
                )
            },
        )?;
        let reopened = open_absolute_directory(canonical_local_root, open_operation)?;
        let reopened_identity = fstat_identity(reopened.as_raw_fd(), None, stat_operation)?;
        let reopened_mount_id = statx_mount_id(reopened.as_raw_fd(), None, statx_operation)?;
        if reopened_identity != expected_identity || reopened_mount_id != expected_mount_id {
            return Err(CaptureFailure::root(
                StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead,
                compare_operation,
            ));
        }
        Ok(())
    }

    fn validate_configured_root(path: &Path) -> std::result::Result<(), CaptureFailure> {
        if !path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::CurDir | Component::ParentDir | Component::Prefix(_)
                )
            })
        {
            return Err(CaptureFailure::root(
                StrictLocalSnapshotIncompleteKindV1::InvalidConfiguredRoot,
                "require-absolute-normal-components",
            ));
        }
        let canonical = path.canonicalize().map_err(|_| {
            CaptureFailure::root(
                StrictLocalSnapshotIncompleteKindV1::InvalidConfiguredRoot,
                "canonicalize-configured-root",
            )
        })?;
        if canonical.as_os_str().as_bytes() != path.as_os_str().as_bytes() {
            return Err(CaptureFailure::root(
                StrictLocalSnapshotIncompleteKindV1::InvalidConfiguredRoot,
                "require-exact-canonical-root-spelling",
            ));
        }
        Ok(())
    }

    fn inventory_tree(
        root: &File,
        expected_root: StableIdentity,
        expected_root_mount_id: u64,
        profile: RootProfileV1,
    ) -> std::result::Result<IdentityInventory, CaptureFailure> {
        let observed_root = fstat_identity(root.as_raw_fd(), None, "stat-inventory-root")?;
        if observed_root != expected_root {
            return Err(CaptureFailure::root(
                StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead,
                "compare-inventory-root",
            ));
        }
        let observed_mount_id = statx_mount_id(root.as_raw_fd(), None, "statx-inventory-root")?;
        if observed_mount_id != expected_root_mount_id {
            return Err(CaptureFailure::root(
                StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead,
                "compare-inventory-root-mount-identity",
            ));
        }
        let mut context = InventoryContext::collecting(profile, observed_root);
        inventory_directory(root, &[], false, 0, &mut context)?;
        let after_root = fstat_identity(root.as_raw_fd(), None, "restat-inventory-root")?;
        let after_mount_id = statx_mount_id(root.as_raw_fd(), None, "restatx-inventory-root")?;
        if after_root != observed_root || after_mount_id != observed_mount_id {
            return Err(CaptureFailure::root(
                StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead,
                "compare-inventory-root-after-walk",
            ));
        }
        if profile == RootProfileV1::GitRawV1 && !context.saw_root_git_directory {
            return Err(CaptureFailure::root(
                StrictLocalSnapshotIncompleteKindV1::GitLayoutRejected,
                "require-root-dot-git-directory",
            ));
        }
        Ok(IdentityInventory {
            root: observed_root,
            root_mount_id: observed_mount_id,
            entries: context.entries,
        })
    }

    fn verify_inventory_tree(
        root: &File,
        expected_root: StableIdentity,
        expected_root_mount_id: u64,
        profile: RootProfileV1,
        expected: &IdentityInventory,
    ) -> std::result::Result<(), CaptureFailure> {
        let observed_root = fstat_identity(root.as_raw_fd(), None, "stat-verify-root")?;
        let observed_mount_id = statx_mount_id(root.as_raw_fd(), None, "statx-verify-root")?;
        if observed_root != expected_root
            || observed_mount_id != expected_root_mount_id
            || observed_root != expected.root
            || observed_mount_id != expected.root_mount_id
        {
            return Err(CaptureFailure::root(
                StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead,
                "compare-verify-root-before-walk",
            ));
        }
        let mut context = InventoryContext::verifying(profile, observed_root, expected);
        inventory_directory(root, &[], false, 0, &mut context)?;
        let after_root = fstat_identity(root.as_raw_fd(), None, "restat-verify-root")?;
        let after_mount_id = statx_mount_id(root.as_raw_fd(), None, "restatx-verify-root")?;
        if after_root != observed_root || after_mount_id != observed_mount_id {
            return Err(CaptureFailure::root(
                StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead,
                "compare-verify-root-after-walk",
            ));
        }
        if profile == RootProfileV1::GitRawV1 && !context.saw_root_git_directory {
            return Err(CaptureFailure::root(
                StrictLocalSnapshotIncompleteKindV1::GitLayoutRejected,
                "reverify-root-dot-git-directory",
            ));
        }
        if context.observed_count != expected.entries.len() {
            return Err(CaptureFailure::root(
                StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead,
                "compare-verify-entry-count",
            ));
        }
        Ok(())
    }

    fn inventory_directory(
        directory: &File,
        parent_components: &[Vec<u8>],
        parent_excluded: bool,
        depth: usize,
        context: &mut InventoryContext<'_>,
    ) -> std::result::Result<(), CaptureFailure> {
        if depth > MAX_DEPTH_V1 {
            return Err(CaptureFailure::path(
                StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                display_rel_components(parent_components),
                "maximum-directory-depth",
            ));
        }
        let root_directory = directory.try_clone().map_err(|_| {
            CaptureFailure::path(
                StrictLocalSnapshotIncompleteKindV1::FilesystemReadFailed,
                display_rel_components(parent_components),
                "clone-inventory-root-descriptor",
            )
        })?;
        let root_stream = open_directory_stream(root_directory.as_raw_fd(), parent_components)?;
        let mut stack = vec![InventoryDirectoryFrame {
            directory: root_directory,
            stream: root_stream,
            components: parent_components.to_vec(),
            parent_excluded,
            depth,
            expected_identity_after_walk: None,
        }];

        while !stack.is_empty() {
            let name = {
                let frame = stack
                    .last_mut()
                    .expect("nonempty inventory directory frame stack");
                read_next_raw_name(
                    &mut frame.stream,
                    &frame.components,
                    MAX_ENTRIES_V1.saturating_sub(context.retained_entry_count()),
                    MAX_RETAINED_PATH_BYTES_V1.saturating_sub(context.retained_path_bytes),
                )?
            };
            let Some(name) = name else {
                let mut completed = stack
                    .pop()
                    .expect("nonempty inventory directory frame stack");
                completed.stream.close().map_err(|_| {
                    CaptureFailure::path(
                        StrictLocalSnapshotIncompleteKindV1::FilesystemReadFailed,
                        display_rel_components(&completed.components),
                        "closedir",
                    )
                })?;
                if let Some(expected) = completed.expected_identity_after_walk {
                    let rel_bytes = join_rel_components(&completed.components);
                    let after = fstat_identity(
                        completed.directory.as_raw_fd(),
                        Some(&rel_bytes),
                        "restat-directory",
                    )?;
                    if after != expected {
                        return Err(CaptureFailure::path(
                            StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead,
                            display_rel_bytes(&rel_bytes),
                            "compare-directory-after-inventory",
                        ));
                    }
                }
                continue;
            };

            let (directory_fd, parent_components, parent_excluded, depth) = {
                let frame = stack
                    .last()
                    .expect("nonempty inventory directory frame stack");
                (
                    frame.directory.as_raw_fd(),
                    frame.components.clone(),
                    frame.parent_excluded,
                    frame.depth,
                )
            };
            let parent_is_root = parent_components.is_empty();
            let mut components = parent_components;
            components.push(name.clone());
            if components.len() > MAX_DEPTH_V1 {
                return Err(CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                    display_rel_components(&components),
                    "maximum-path-component-depth",
                ));
            }
            let rel_bytes = join_rel_components(&components);
            let rel_display = display_rel_bytes(&rel_bytes);
            context.retained_path_bytes = context
                .retained_path_bytes
                .checked_add(rel_bytes.len())
                .ok_or_else(|| {
                    CaptureFailure::path(
                        StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                        rel_display.clone(),
                        "retained-path-byte-overflow",
                    )
                })?;
            if context.retained_path_bytes > MAX_RETAINED_PATH_BYTES_V1 {
                return Err(CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                    rel_display,
                    "maximum-retained-path-bytes",
                ));
            }
            if context.retained_entry_count() >= MAX_ENTRIES_V1 {
                return Err(CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                    display_rel_bytes(&rel_bytes),
                    "maximum-entry-count",
                ));
            }

            let c_name = c_string_name(&name, &rel_bytes)?;
            // The constrained O_PATH open must be the first metadata operation
            // on a descendant name. An unconstrained stat lookup could enter a
            // mount inserted at that name (and trigger an automount) before
            // NO_XDEV had a chance to reject the boundary.
            let identity_handle = openat_descendant_identity(
                directory_fd,
                &c_name,
                Some(&rel_bytes),
                "open-inventory-entry",
            )?;
            let identity = fstat_identity(
                identity_handle.as_raw_fd(),
                Some(&rel_bytes),
                "stat-inventory-entry",
            )?;
            let exact_dot_git = name.as_slice() == b".git";
            let ascii_dot_git = name.eq_ignore_ascii_case(b".git");
            if ascii_dot_git && !exact_dot_git && !parent_excluded {
                return Err(CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::NamespaceRejected,
                    display_rel_bytes(&rel_bytes),
                    "noncanonical-dot-git-alias",
                ));
            }

            let is_root_dot_git = parent_is_root && exact_dot_git;
            let nested_dot_git = !parent_is_root && exact_dot_git;
            if context.profile == RootProfileV1::GitRawV1 && is_root_dot_git {
                if identity.object_kind() != ObjectKind::Directory {
                    return Err(CaptureFailure::path(
                        StrictLocalSnapshotIncompleteKindV1::GitLayoutRejected,
                        display_rel_bytes(&rel_bytes),
                        "root-dot-git-must-be-real-directory",
                    ));
                }
                context.saw_root_git_directory = true;
            }
            if context.profile == RootProfileV1::GitRawV1 && nested_dot_git {
                return Err(CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::GitLayoutRejected,
                    display_rel_bytes(&rel_bytes),
                    "nested-dot-git-rejected",
                ));
            }

            let rel_os_path = raw_rel_path(&components);
            let profile_excluded = context.profile == RootProfileV1::AgentStaticV1 && exact_dot_git;
            let fixed_excluded = Blacklist::default()
                .check_fixed_ingress_path_components(&rel_os_path)
                .is_some();
            let excluded = parent_excluded || profile_excluded || fixed_excluded;
            let disposition = if excluded {
                Disposition::Excluded
            } else {
                Disposition::Included
            };

            if !excluded
                && identity.object_kind() == ObjectKind::Regular
                && identity.has_special_permissions()
            {
                return Err(CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::UnsafeModeRejected,
                    display_rel_bytes(&rel_bytes),
                    "setuid-setgid-sticky-mode",
                ));
            }
            match identity.object_kind() {
                ObjectKind::Directory => {
                    if !context.directory_identities.insert(identity.dev_ino()) {
                        return Err(CaptureFailure::path(
                            StrictLocalSnapshotIncompleteKindV1::DuplicateIdentityRejected,
                            display_rel_bytes(&rel_bytes),
                            "repeated-directory-identity",
                        ));
                    }
                }
                ObjectKind::Regular | ObjectKind::Symlink if !excluded => {
                    if identity.nlink != 1 {
                        return Err(CaptureFailure::path(
                            StrictLocalSnapshotIncompleteKindV1::HardlinkRejected,
                            display_rel_bytes(&rel_bytes),
                            "included-leaf-link-count",
                        ));
                    }
                    if identity.object_kind() == ObjectKind::Regular {
                        let size = u64::try_from(identity.size).map_err(|_| {
                            CaptureFailure::path(
                                StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                                display_rel_bytes(&rel_bytes),
                                "negative-regular-file-size-before-hash",
                            )
                        })?;
                        if size > MAX_REGULAR_FILE_BYTES_V1 {
                            return Err(CaptureFailure::path(
                                StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                                display_rel_bytes(&rel_bytes),
                                "maximum-regular-file-bytes",
                            ));
                        }
                        context.total_hashed_bytes = context
                            .total_hashed_bytes
                            .checked_add(size)
                            .ok_or_else(|| {
                            CaptureFailure::path(
                                StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                                display_rel_bytes(&rel_bytes),
                                "total-hashed-byte-overflow",
                            )
                        })?;
                        if context.total_hashed_bytes > MAX_TOTAL_HASHED_BYTES_V1 {
                            return Err(CaptureFailure::path(
                                StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                                display_rel_bytes(&rel_bytes),
                                "maximum-total-hashed-bytes",
                            ));
                        }
                    }
                    if !context.included_leaf_identities.insert(identity.dev_ino()) {
                        return Err(CaptureFailure::path(
                            StrictLocalSnapshotIncompleteKindV1::DuplicateIdentityRejected,
                            display_rel_bytes(&rel_bytes),
                            "duplicate-included-leaf-identity",
                        ));
                    }
                }
                ObjectKind::Special if !excluded => {
                    return Err(CaptureFailure::path(
                        StrictLocalSnapshotIncompleteKindV1::UnsupportedObjectRejected,
                        display_rel_bytes(&rel_bytes),
                        "included-special-object",
                    ));
                }
                ObjectKind::Regular | ObjectKind::Symlink | ObjectKind::Special => {}
            }

            let record = InventoryRecord {
                identity,
                disposition,
            };
            if let Some(expected) = context.expected {
                if expected.entries.get(&rel_bytes) != Some(&record) {
                    return Err(CaptureFailure::path(
                        StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead,
                        display_rel_bytes(&rel_bytes),
                        "compare-verify-inventory-entry",
                    ));
                }
                context.observed_count =
                    context.observed_count.checked_add(1).ok_or_else(|| {
                        CaptureFailure::path(
                            StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                            display_rel_bytes(&rel_bytes),
                            "verify-entry-count-overflow",
                        )
                    })?;
            } else if context.entries.insert(rel_bytes.clone(), record).is_some() {
                return Err(CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::DuplicateIdentityRejected,
                    display_rel_bytes(&rel_bytes),
                    "duplicate-inventory-path",
                ));
            }

            if identity.object_kind() == ObjectKind::Directory && !excluded {
                let child = openat_descendant_file(
                    directory_fd,
                    &c_name,
                    true,
                    Some(&rel_bytes),
                    "open-inventory-directory",
                )?;
                let opened_identity =
                    fstat_identity(child.as_raw_fd(), Some(&rel_bytes), "stat-opened-directory")?;
                if opened_identity != identity {
                    return Err(CaptureFailure::path(
                        StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead,
                        display_rel_bytes(&rel_bytes),
                        "compare-opened-directory-identity",
                    ));
                }
                let child_depth = depth.checked_add(1).ok_or_else(|| {
                    CaptureFailure::path(
                        StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                        display_rel_bytes(&rel_bytes),
                        "directory-depth-overflow",
                    )
                })?;
                if child_depth > MAX_DEPTH_V1 {
                    return Err(CaptureFailure::path(
                        StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                        display_rel_bytes(&rel_bytes),
                        "maximum-directory-depth",
                    ));
                }
                let child_stream = open_directory_stream(child.as_raw_fd(), &components)?;
                stack.push(InventoryDirectoryFrame {
                    directory: child,
                    stream: child_stream,
                    components,
                    parent_excluded: excluded,
                    depth: child_depth,
                    expected_identity_after_walk: Some(identity),
                });
            }
        }
        Ok(())
    }

    fn portable_namespace_claims(
        entries: &BTreeMap<Vec<u8>, InventoryRecord>,
    ) -> std::result::Result<BTreeMap<String, StrictLocalNamespaceClaimV1>, CaptureFailure> {
        let mut claims: BTreeMap<String, StrictLocalNamespaceClaimV1> = BTreeMap::new();
        for (raw_path, record) in entries {
            if record.disposition == Disposition::Excluded {
                continue;
            }
            let rel_path = std::str::from_utf8(raw_path).map_err(|_| {
                CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::NamespaceRejected,
                    display_rel_bytes(raw_path),
                    "non-utf8-path",
                )
            })?;
            validate_namespace_logical_path(rel_path).map_err(|_| {
                CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::NamespaceRejected,
                    rel_path,
                    "noncanonical-logical-path",
                )
            })?;
            let leaf_role = match record.identity.object_kind() {
                ObjectKind::Directory => StrictLocalNamespaceRoleV1::Directory,
                ObjectKind::Regular | ObjectKind::Symlink => StrictLocalNamespaceRoleV1::File,
                ObjectKind::Special => continue,
            };
            let components: Vec<&str> = rel_path.split('/').collect();
            for end in 1..=components.len() {
                let exact = components[..end].join("/");
                let role = if end == components.len() {
                    leaf_role
                } else {
                    StrictLocalNamespaceRoleV1::Directory
                };
                let folded = portable_casefold_path(&exact).map_err(|_| {
                    CaptureFailure::path(
                        StrictLocalSnapshotIncompleteKindV1::NamespaceRejected,
                        rel_path,
                        "portable-casefold-path",
                    )
                })?;
                if let Some(prior) = claims.get(&folded) {
                    if prior.exact_path != exact || prior.role != role {
                        return Err(CaptureFailure::path(
                            StrictLocalSnapshotIncompleteKindV1::NamespaceRejected,
                            rel_path,
                            "portable-casefold-or-role-collision",
                        ));
                    }
                } else {
                    claims.insert(
                        folded,
                        StrictLocalNamespaceClaimV1 {
                            exact_path: exact,
                            role,
                        },
                    );
                }
            }
        }
        Ok(claims)
    }

    fn capture_semantic_entries(
        root: &File,
        inventory: &IdentityInventory,
    ) -> std::result::Result<BTreeMap<String, StrictLocalEntryV1>, CaptureFailure> {
        let mut entries = BTreeMap::new();
        let mut required_directories = BTreeSet::new();
        for (raw_path, record) in &inventory.entries {
            if record.disposition == Disposition::Excluded {
                continue;
            }
            let rel_path = std::str::from_utf8(raw_path).map_err(|_| {
                CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::NamespaceRejected,
                    display_rel_bytes(raw_path),
                    "non-utf8-semantic-path",
                )
            })?;
            let entry = match record.identity.object_kind() {
                ObjectKind::Directory => continue,
                ObjectKind::Regular => StrictLocalEntryV1::Regular(capture_regular_file(
                    root,
                    inventory,
                    raw_path,
                    record.identity,
                )?),
                ObjectKind::Symlink => StrictLocalEntryV1::Symlink(capture_symlink(
                    root,
                    inventory,
                    raw_path,
                    record.identity,
                )?),
                ObjectKind::Special => {
                    return Err(CaptureFailure::path(
                        StrictLocalSnapshotIncompleteKindV1::UnsupportedObjectRejected,
                        rel_path,
                        "special-object-reached-semantic-capture",
                    ));
                }
            };
            for (separator, _) in rel_path.match_indices('/') {
                required_directories.insert(rel_path[..separator].to_owned());
            }
            entries.insert(rel_path.to_owned(), entry);
        }
        for directory in required_directories {
            let has_included_directory =
                inventory
                    .entries
                    .get(directory.as_bytes())
                    .is_some_and(|record| {
                        record.disposition == Disposition::Included
                            && record.identity.object_kind() == ObjectKind::Directory
                    });
            if !has_included_directory {
                return Err(CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead,
                    directory,
                    "missing-included-semantic-ancestor",
                ));
            }
            entries.insert(directory, StrictLocalEntryV1::Directory);
        }
        Ok(entries)
    }

    fn capture_regular_file(
        root: &File,
        inventory: &IdentityInventory,
        rel_path: &[u8],
        expected: StableIdentity,
    ) -> std::result::Result<StrictLocalRegularFileV1, CaptureFailure> {
        if expected.size < 0 {
            return Err(CaptureFailure::path(
                StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead,
                display_rel_bytes(rel_path),
                "negative-regular-file-size",
            ));
        }
        let (parent, leaf_name, expected_parent) =
            open_inventory_parent(root, inventory, rel_path)?;
        let before_parent =
            fstat_identity(parent.as_raw_fd(), Some(rel_path), "stat-regular-parent")?;
        if before_parent != expected_parent {
            return Err(changed(rel_path, "compare-regular-parent-before"));
        }
        let c_name = c_string_name(&leaf_name, rel_path)?;
        // Preserve the inventory's no-crossing proof for both the initial
        // binding and the post-read pathname revalidation. Never lstat a
        // descendant name outside an openat2(NO_XDEV) descriptor.
        let probe = openat_descendant_identity(
            parent.as_raw_fd(),
            &c_name,
            Some(rel_path),
            "open-regular-identity-probe",
        )?;
        let before_probe = fstat_identity(
            probe.as_raw_fd(),
            Some(rel_path),
            "stat-regular-identity-probe",
        )?;
        if before_probe != expected {
            return Err(changed(rel_path, "compare-regular-identity-probe"));
        }
        run_test_hook(TestHookPoint::RegularAfterInitialProbe);
        let mut file = openat_descendant_file(
            parent.as_raw_fd(),
            &c_name,
            false,
            Some(rel_path),
            "open-regular-file",
        )?;
        let before_fd = fstat_identity(file.as_raw_fd(), Some(rel_path), "stat-regular-fd")?;
        if before_fd != expected || before_fd.object_kind() != ObjectKind::Regular {
            return Err(changed(rel_path, "compare-opened-regular-identity"));
        }

        let expected_size = expected.size as u64;
        let mut remaining = expected_size;
        let mut buffer = vec![0u8; HASH_BUFFER_BYTES_V1];
        let mut hasher = blake3::Hasher::new();
        let mut ran_hash_hook = false;
        while remaining > 0 {
            let requested = usize::try_from(remaining.min(buffer.len() as u64))
                .expect("bounded hash read length fits usize");
            let count = file.read(&mut buffer[..requested]).map_err(|_| {
                CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::FilesystemReadFailed,
                    display_rel_bytes(rel_path),
                    "read-regular-file",
                )
            })?;
            if count == 0 {
                return Err(changed(rel_path, "regular-file-short-read"));
            }
            hasher.update(&buffer[..count]);
            remaining -= count as u64;
            if !ran_hash_hook {
                run_test_hook(TestHookPoint::RegularAfterFirstHashChunk);
                ran_hash_hook = true;
            }
        }
        let mut sentinel = [0u8; 1];
        let sentinel_count = file.read(&mut sentinel).map_err(|_| {
            CaptureFailure::path(
                StrictLocalSnapshotIncompleteKindV1::FilesystemReadFailed,
                display_rel_bytes(rel_path),
                "read-regular-eof-sentinel",
            )
        })?;
        if sentinel_count != 0 {
            return Err(changed(rel_path, "regular-file-grew-during-read"));
        }

        let after_fd = fstat_identity(file.as_raw_fd(), Some(rel_path), "restat-regular-fd")?;
        let after_probe = fstat_identity(
            probe.as_raw_fd(),
            Some(rel_path),
            "restat-regular-identity-probe",
        )?;
        let rebound_probe = openat_descendant_identity(
            parent.as_raw_fd(),
            &c_name,
            Some(rel_path),
            "reopen-regular-path-binding",
        )?;
        let rebound_identity = fstat_identity(
            rebound_probe.as_raw_fd(),
            Some(rel_path),
            "restat-reopened-regular-path-binding",
        )?;
        let after_parent =
            fstat_identity(parent.as_raw_fd(), Some(rel_path), "restat-regular-parent")?;
        if after_fd != expected
            || after_probe != expected
            || rebound_identity != expected
            || after_parent != expected_parent
        {
            return Err(changed(rel_path, "compare-regular-after-read"));
        }
        let mtime_nanoseconds = u32::try_from(expected.mtime_nanoseconds).map_err(|_| {
            CaptureFailure::path(
                StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead,
                display_rel_bytes(rel_path),
                "regular-mtime-nanoseconds-out-of-range",
            )
        })?;
        if mtime_nanoseconds >= 1_000_000_000 {
            return Err(CaptureFailure::path(
                StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead,
                display_rel_bytes(rel_path),
                "regular-mtime-nanoseconds-not-normalized",
            ));
        }
        Ok(StrictLocalRegularFileV1 {
            raw_blake3: *hasher.finalize().as_bytes(),
            size: expected_size,
            portable_mode: expected.mode & 0o777,
            mtime_seconds: expected.mtime_seconds,
            mtime_nanoseconds,
        })
    }

    fn capture_symlink(
        root: &File,
        inventory: &IdentityInventory,
        rel_path: &[u8],
        expected: StableIdentity,
    ) -> std::result::Result<StrictLocalSymlinkV1, CaptureFailure> {
        if expected.size < 0 || expected.size as usize > MAX_SYMLINK_TARGET_BYTES_V1 {
            return Err(CaptureFailure::path(
                StrictLocalSnapshotIncompleteKindV1::SymlinkTargetRejected,
                display_rel_bytes(rel_path),
                "symlink-target-size",
            ));
        }
        let (parent, leaf_name, expected_parent) =
            open_inventory_parent(root, inventory, rel_path)?;
        let before_parent =
            fstat_identity(parent.as_raw_fd(), Some(rel_path), "stat-symlink-parent")?;
        if before_parent != expected_parent {
            return Err(changed(rel_path, "compare-symlink-parent-before"));
        }
        let c_name = c_string_name(&leaf_name, rel_path)?;
        // O_PATH|O_NOFOLLOW pins the link object itself while NO_XDEV rejects
        // a newly inserted mount before any descendant metadata lookup.
        let probe = openat_descendant_identity(
            parent.as_raw_fd(),
            &c_name,
            Some(rel_path),
            "open-symlink-identity-probe",
        )?;
        let before_probe = fstat_identity(
            probe.as_raw_fd(),
            Some(rel_path),
            "stat-symlink-identity-probe",
        )?;
        if before_probe != expected || before_probe.object_kind() != ObjectKind::Symlink {
            return Err(changed(rel_path, "compare-symlink-identity-probe"));
        }

        let mut target_buffer = [0u8; MAX_SYMLINK_TARGET_BYTES_V1 + 1];
        let empty = CString::new("").expect("empty string contains no NUL");
        // SAFETY: the O_PATH descriptor pins the symlink itself; Linux
        // readlinkat with an empty path reads that pinned link and writes at
        // most the supplied initialized buffer length.
        let target_len = unsafe {
            libc::readlinkat(
                probe.as_raw_fd(),
                empty.as_ptr(),
                target_buffer.as_mut_ptr().cast(),
                target_buffer.len(),
            )
        };
        if target_len < 0 {
            return Err(CaptureFailure::path(
                StrictLocalSnapshotIncompleteKindV1::FilesystemReadFailed,
                display_rel_bytes(rel_path),
                "readlinkat",
            ));
        }
        let target_len = target_len as usize;
        if target_len > MAX_SYMLINK_TARGET_BYTES_V1 || target_len != expected.size as usize {
            return Err(changed(rel_path, "compare-symlink-target-length"));
        }
        let rebound_probe = openat_descendant_identity(
            parent.as_raw_fd(),
            &c_name,
            Some(rel_path),
            "reopen-symlink-path-binding",
        )?;
        let rebound_identity = fstat_identity(
            rebound_probe.as_raw_fd(),
            Some(rel_path),
            "restat-reopened-symlink-path-binding",
        )?;
        let after_parent =
            fstat_identity(parent.as_raw_fd(), Some(rel_path), "restat-symlink-parent")?;
        let after_probe = fstat_identity(
            probe.as_raw_fd(),
            Some(rel_path),
            "restat-symlink-identity-probe",
        )?;
        if rebound_identity != expected
            || after_probe != expected
            || after_parent != expected_parent
        {
            return Err(changed(rel_path, "compare-symlink-after-read"));
        }

        let target_bytes = &target_buffer[..target_len];
        let target = std::str::from_utf8(target_bytes).map_err(|_| {
            CaptureFailure::path(
                StrictLocalSnapshotIncompleteKindV1::SymlinkTargetRejected,
                display_rel_bytes(rel_path),
                "symlink-target-non-utf8",
            )
        })?;
        let rel_text = std::str::from_utf8(rel_path).map_err(|_| {
            CaptureFailure::path(
                StrictLocalSnapshotIncompleteKindV1::NamespaceRejected,
                display_rel_bytes(rel_path),
                "symlink-path-non-utf8",
            )
        })?;
        crate::engine::validate_indexed_symlink_target(Path::new(rel_text), target).map_err(
            |_| {
                CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::SymlinkTargetRejected,
                    rel_text,
                    "validate-symlink-target",
                )
            },
        )?;
        Ok(StrictLocalSymlinkV1 {
            target: target.to_owned(),
            target_blake3: *blake3::hash(target_bytes).as_bytes(),
        })
    }

    fn open_inventory_parent(
        root: &File,
        inventory: &IdentityInventory,
        rel_path: &[u8],
    ) -> std::result::Result<(File, Vec<u8>, StableIdentity), CaptureFailure> {
        let components: Vec<&[u8]> = rel_path.split(|byte| *byte == b'/').collect();
        let (leaf, parent_components) = components.split_last().ok_or_else(|| {
            CaptureFailure::root(
                StrictLocalSnapshotIncompleteKindV1::NamespaceRejected,
                "empty-inventory-path",
            )
        })?;
        let dot = CString::new(".").expect("dot contains no NUL");
        let mut current = openat_descendant_file(
            root.as_raw_fd(),
            &dot,
            true,
            None,
            "open-root-for-leaf-walk",
        )?;
        let mut walked = Vec::<Vec<u8>>::new();
        let mut expected_current = inventory.root;
        for component in parent_components {
            let observed =
                fstat_identity(current.as_raw_fd(), Some(rel_path), "stat-leaf-walk-parent")?;
            if observed != expected_current {
                return Err(changed(rel_path, "compare-leaf-walk-parent"));
            }
            let c_name = c_string_name(component, rel_path)?;
            current = openat_descendant_file(
                current.as_raw_fd(),
                &c_name,
                true,
                Some(rel_path),
                "open-leaf-walk-component",
            )?;
            run_test_hook(TestHookPoint::ParentWalkAfterComponentOpen);
            walked.push(component.to_vec());
            let walked_key = join_rel_components(&walked);
            expected_current = inventory
                .entries
                .get(&walked_key)
                .filter(|record| record.identity.object_kind() == ObjectKind::Directory)
                .map(|record| record.identity)
                .ok_or_else(|| changed(rel_path, "missing-inventory-parent"))?;
            let opened = fstat_identity(
                current.as_raw_fd(),
                Some(rel_path),
                "stat-opened-leaf-parent",
            )?;
            if opened != expected_current {
                return Err(changed(rel_path, "compare-opened-leaf-parent"));
            }
        }
        Ok((current, leaf.to_vec(), expected_current))
    }

    fn changed(rel_path: &[u8], operation: &'static str) -> CaptureFailure {
        CaptureFailure::path(
            StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead,
            display_rel_bytes(rel_path),
            operation,
        )
    }

    fn open_absolute_directory(
        path: &Path,
        operation: &'static str,
    ) -> std::result::Result<File, CaptureFailure> {
        let slash = CString::new("/").expect("slash contains no NUL");
        let mut current =
            openat_root_file(libc::AT_FDCWD, &slash, true, None, "open-filesystem-root")?;
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    let name_bytes = name.as_bytes();
                    let c_name = CString::new(name_bytes).map_err(|_| {
                        CaptureFailure::root(
                            StrictLocalSnapshotIncompleteKindV1::InvalidConfiguredRoot,
                            "configured-root-component-nul",
                        )
                    })?;
                    current =
                        openat_root_file(current.as_raw_fd(), &c_name, true, None, operation)?;
                }
                Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                    return Err(CaptureFailure::root(
                        StrictLocalSnapshotIncompleteKindV1::InvalidConfiguredRoot,
                        "configured-root-component-shape",
                    ));
                }
            }
        }
        Ok(current)
    }

    fn openat_root_file(
        parent_fd: RawFd,
        name: &CStr,
        directory: bool,
        rel_path: Option<&[u8]>,
        operation: &'static str,
    ) -> std::result::Result<File, CaptureFailure> {
        let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        if directory {
            flags |= libc::O_DIRECTORY;
        }
        // SAFETY: `parent_fd` is an open directory descriptor (or AT_FDCWD for
        // the absolute slash open), and `name` is NUL terminated. On success
        // the returned descriptor has exactly one `File` owner.
        let fd = unsafe { libc::openat(parent_fd, name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io_failure(rel_path, operation));
        }
        // SAFETY: `fd` is freshly returned by openat and uniquely owned.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn openat_descendant_file(
        parent_fd: RawFd,
        name: &CStr,
        directory: bool,
        rel_path: Option<&[u8]>,
        operation: &'static str,
    ) -> std::result::Result<File, CaptureFailure> {
        let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        if directory {
            flags |= libc::O_DIRECTORY;
        }
        openat2_resolved_file(parent_fd, name, flags, rel_path, operation)
    }

    pub(super) fn openat_descendant_identity(
        parent_fd: RawFd,
        name: &CStr,
        rel_path: Option<&[u8]>,
        operation: &'static str,
    ) -> std::result::Result<File, CaptureFailure> {
        openat2_resolved_file(
            parent_fd,
            name,
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            rel_path,
            operation,
        )
    }

    fn openat2_resolved_file(
        parent_fd: RawFd,
        name: &CStr,
        flags: libc::c_int,
        rel_path: Option<&[u8]>,
        operation: &'static str,
    ) -> std::result::Result<File, CaptureFailure> {
        // `libc::open_how` is non-exhaustive. Zero-initialize the kernel ABI
        // structure so future tail fields remain disabled, then set only the
        // fields defined by this contract.
        // SAFETY: all-zero is the kernel-defined disabled state for every
        // open_how field; the three public fields are populated immediately.
        let mut how = unsafe { MaybeUninit::<libc::open_how>::zeroed().assume_init() };
        how.flags = flags as u64;
        how.mode = 0;
        how.resolve = libc::RESOLVE_NO_XDEV
            | libc::RESOLVE_NO_MAGICLINKS
            | libc::RESOLVE_NO_SYMLINKS
            | libc::RESOLVE_BENEATH;
        // SAFETY: openat2 receives a valid parent descriptor, NUL-terminated
        // single-component path, and correctly sized open_how-compatible
        // structure. Every successful descriptor gets exactly one File owner.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                parent_fd,
                name.as_ptr(),
                &how as *const libc::open_how,
                std::mem::size_of::<libc::open_how>(),
            )
        };
        if fd < 0 {
            let errno = std::io::Error::last_os_error().raw_os_error();
            if matches!(
                errno,
                Some(code) if matches!(code, libc::ENOSYS | libc::EINVAL | libc::E2BIG)
            ) {
                return Err(match rel_path {
                    Some(path) => CaptureFailure::path(
                        StrictLocalSnapshotIncompleteKindV1::UnsupportedPlatform,
                        display_rel_bytes(path),
                        operation,
                    ),
                    None => CaptureFailure::root(
                        StrictLocalSnapshotIncompleteKindV1::UnsupportedPlatform,
                        operation,
                    ),
                });
            }
            return Err(io_failure(rel_path, operation));
        }
        let fd = RawFd::try_from(fd).map_err(|_| io_failure(rel_path, operation))?;
        // SAFETY: `fd` is freshly returned by openat2 and uniquely owned.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn statx_mount_id(
        fd: RawFd,
        rel_path: Option<&[u8]>,
        operation: &'static str,
    ) -> std::result::Result<u64, CaptureFailure> {
        let empty = CString::new("").expect("empty string contains no NUL");
        let mut statx = MaybeUninit::<libc::statx>::zeroed();
        // SAFETY: fd is valid and AT_EMPTY_PATH requests metadata for that
        // descriptor. `statx` points to writable storage.
        let result = unsafe {
            libc::statx(
                fd,
                empty.as_ptr(),
                libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
                libc::STATX_MNT_ID,
                statx.as_mut_ptr(),
            )
        };
        if result != 0 {
            let errno = std::io::Error::last_os_error().raw_os_error();
            if matches!(
                errno,
                Some(code) if matches!(code, libc::ENOSYS | libc::EINVAL)
            ) {
                return Err(match rel_path {
                    Some(path) => CaptureFailure::path(
                        StrictLocalSnapshotIncompleteKindV1::UnsupportedPlatform,
                        display_rel_bytes(path),
                        operation,
                    ),
                    None => CaptureFailure::root(
                        StrictLocalSnapshotIncompleteKindV1::UnsupportedPlatform,
                        operation,
                    ),
                });
            }
            return Err(io_failure(rel_path, operation));
        }
        // SAFETY: statx succeeded and initialized the output.
        let statx = unsafe { statx.assume_init() };
        if statx.stx_mask & libc::STATX_MNT_ID == 0 {
            return Err(match rel_path {
                Some(path) => CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::UnsupportedPlatform,
                    display_rel_bytes(path),
                    operation,
                ),
                None => CaptureFailure::root(
                    StrictLocalSnapshotIncompleteKindV1::UnsupportedPlatform,
                    operation,
                ),
            });
        }
        Ok(statx.stx_mnt_id)
    }

    fn fstat_identity(
        fd: RawFd,
        rel_path: Option<&[u8]>,
        operation: &'static str,
    ) -> std::result::Result<StableIdentity, CaptureFailure> {
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `stat` points to writable storage and `fd` is open.
        if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
            return Err(io_failure(rel_path, operation));
        }
        // SAFETY: fstat succeeded and initialized the structure.
        Ok(stable_identity(unsafe { stat.assume_init() }))
    }

    fn stable_identity(stat: libc::stat) -> StableIdentity {
        StableIdentity {
            dev: stat.st_dev,
            ino: stat.st_ino,
            mode: stat.st_mode,
            nlink: stat.st_nlink,
            uid: stat.st_uid,
            gid: stat.st_gid,
            rdev: stat.st_rdev,
            size: stat.st_size,
            mtime_seconds: stat.st_mtime,
            mtime_nanoseconds: stat.st_mtime_nsec,
            ctime_seconds: stat.st_ctime,
            ctime_nanoseconds: stat.st_ctime_nsec,
        }
    }

    fn open_directory_stream(
        directory_fd: RawFd,
        parent_components: &[Vec<u8>],
    ) -> std::result::Result<DirectoryStream, CaptureFailure> {
        let dot = CString::new(".").expect("dot contains no NUL");
        let stream_file = openat_descendant_file(
            directory_fd,
            &dot,
            true,
            None,
            "open-independent-directory-stream",
        )?;
        let stream_fd = stream_file.into_raw_fd();
        // SAFETY: fdopendir takes ownership of this dedicated descriptor.
        let directory = unsafe { libc::fdopendir(stream_fd) };
        if directory.is_null() {
            // SAFETY: fdopendir failed and did not take ownership.
            unsafe {
                libc::close(stream_fd);
            }
            return Err(CaptureFailure::path(
                StrictLocalSnapshotIncompleteKindV1::FilesystemReadFailed,
                display_rel_components(parent_components),
                "fdopendir",
            ));
        }
        Ok(DirectoryStream(directory))
    }

    fn read_next_raw_name(
        stream: &mut DirectoryStream,
        parent_components: &[Vec<u8>],
        remaining_entries: usize,
        remaining_path_bytes: usize,
    ) -> std::result::Result<Option<Vec<u8>>, CaptureFailure> {
        let parent_path_len = parent_components
            .iter()
            .try_fold(0usize, |total, component| {
                total.checked_add(component.len())
            })
            .and_then(|component_bytes| {
                component_bytes.checked_add(parent_components.len().saturating_sub(1))
            })
            .ok_or_else(|| {
                CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                    display_rel_components(parent_components),
                    "directory-path-length-overflow",
                )
            })?;
        let child_prefix_len = if parent_components.is_empty() {
            0
        } else {
            parent_path_len.checked_add(1).ok_or_else(|| {
                CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                    display_rel_components(parent_components),
                    "child-path-prefix-overflow",
                )
            })?
        };
        loop {
            clear_errno();
            // SAFETY: `stream.0` is a valid DIR pointer until Drop.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                if read_errno() != 0 {
                    return Err(CaptureFailure::path(
                        StrictLocalSnapshotIncompleteKindV1::FilesystemReadFailed,
                        display_rel_components(parent_components),
                        "readdir",
                    ));
                }
                return Ok(None);
            }
            // SAFETY: readdir returned a valid dirent with NUL-terminated d_name
            // until the next call on this stream. Copy bytes immediately.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            if remaining_entries == 0 {
                return Err(CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                    display_rel_components(parent_components),
                    "maximum-entry-count-before-name-allocation",
                ));
            }
            let child_path_len = child_prefix_len.checked_add(name.len()).ok_or_else(|| {
                CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                    display_rel_components(parent_components),
                    "child-path-length-overflow",
                )
            })?;
            if child_path_len > remaining_path_bytes {
                return Err(CaptureFailure::path(
                    StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded,
                    display_rel_components(parent_components),
                    "maximum-retained-path-bytes-before-name-allocation",
                ));
            }
            return Ok(Some(name.to_vec()));
        }
    }

    struct DirectoryStream(*mut libc::DIR);

    impl DirectoryStream {
        fn close(&mut self) -> std::io::Result<()> {
            if self.0.is_null() {
                return Ok(());
            }
            // SAFETY: this object uniquely owns the DIR pointer.
            let result = unsafe { libc::closedir(self.0) };
            self.0 = std::ptr::null_mut();
            if result == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        }
    }

    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: this object uniquely owns the DIR pointer.
                unsafe {
                    libc::closedir(self.0);
                }
                self.0 = std::ptr::null_mut();
            }
        }
    }

    fn errno_pointer() -> *mut libc::c_int {
        // SAFETY: libc returns the current thread's errno location.
        unsafe { libc::__errno_location() }
    }

    fn clear_errno() {
        // SAFETY: errno_pointer returns writable thread-local errno storage.
        unsafe {
            *errno_pointer() = 0;
        }
    }

    fn read_errno() -> libc::c_int {
        // SAFETY: errno_pointer returns readable thread-local errno storage.
        unsafe { *errno_pointer() }
    }

    fn io_failure(rel_path: Option<&[u8]>, operation: &'static str) -> CaptureFailure {
        let errno = std::io::Error::last_os_error().raw_os_error();
        let kind = match errno {
            Some(libc::EXDEV) => StrictLocalSnapshotIncompleteKindV1::MountBoundaryRejected,
            Some(libc::ENOENT | libc::ESTALE | libc::ELOOP | libc::EAGAIN) => {
                StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead
            }
            _ => StrictLocalSnapshotIncompleteKindV1::FilesystemReadFailed,
        };
        match rel_path {
            Some(path) => CaptureFailure::path(kind, display_rel_bytes(path), operation),
            None => CaptureFailure::root(kind, operation),
        }
    }

    fn c_string_name(name: &[u8], rel_path: &[u8]) -> std::result::Result<CString, CaptureFailure> {
        CString::new(name).map_err(|_| {
            CaptureFailure::path(
                StrictLocalSnapshotIncompleteKindV1::NamespaceRejected,
                display_rel_bytes(rel_path),
                "path-component-nul",
            )
        })
    }

    fn join_rel_components(components: &[Vec<u8>]) -> Vec<u8> {
        let capacity = components
            .iter()
            .map(Vec::len)
            .sum::<usize>()
            .saturating_add(components.len().saturating_sub(1));
        let mut result = Vec::with_capacity(capacity);
        for (index, component) in components.iter().enumerate() {
            if index != 0 {
                result.push(b'/');
            }
            result.extend_from_slice(component);
        }
        result
    }

    fn raw_rel_path(components: &[Vec<u8>]) -> PathBuf {
        let mut path = PathBuf::new();
        for component in components {
            path.push(OsString::from_vec(component.clone()));
        }
        path
    }

    fn display_rel_components(components: &[Vec<u8>]) -> String {
        display_rel_bytes(&join_rel_components(components))
    }

    fn display_rel_bytes(path: &[u8]) -> String {
        if path.is_empty() {
            ".".to_owned()
        } else {
            String::from_utf8_lossy(path).into_owned()
        }
    }

    #[cfg(test)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum TestHookPoint {
        InventoryAToLeaf,
        LeafToInventoryB,
        RootPathBeforeReopen,
        RegularAfterInitialProbe,
        RegularAfterFirstHashChunk,
        ParentWalkAfterComponentOpen,
    }

    #[cfg(test)]
    type TestHook = Box<dyn FnMut(TestHookPoint)>;

    #[cfg(test)]
    thread_local! {
        static TEST_HOOK: std::cell::RefCell<Option<TestHook>> =
            std::cell::RefCell::new(None);
    }

    #[cfg(test)]
    pub(super) struct TestHookGuard;

    #[cfg(test)]
    impl Drop for TestHookGuard {
        fn drop(&mut self) {
            TEST_HOOK.with(|slot| {
                slot.borrow_mut().take();
            });
        }
    }

    #[cfg(test)]
    pub(super) fn install_test_hook(hook: impl FnMut(TestHookPoint) + 'static) -> TestHookGuard {
        TEST_HOOK.with(|slot| {
            assert!(slot.borrow().is_none(), "test hook already installed");
            *slot.borrow_mut() = Some(Box::new(hook));
        });
        TestHookGuard
    }

    #[cfg(test)]
    fn run_test_hook(point: TestHookPoint) {
        TEST_HOOK.with(|slot| {
            if let Some(hook) = slot.borrow_mut().as_mut() {
                hook(point);
            }
        });
    }

    #[cfg(not(test))]
    #[derive(Clone, Copy)]
    enum TestHookPoint {
        InventoryAToLeaf,
        LeafToInventoryB,
        RootPathBeforeReopen,
        RegularAfterInitialProbe,
        RegularAfterFirstHashChunk,
        ParentWalkAfterComponentOpen,
    }

    #[cfg(not(test))]
    fn run_test_hook(_point: TestHookPoint) {}
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::supported::{install_test_hook, openat_descendant_identity, TestHookPoint};
    use super::*;
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::ffi::CString;
    use std::fs;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    fn canonical_root() -> (tempfile::TempDir, PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        fs::create_dir(&root).unwrap();
        (temporary, root.canonicalize().unwrap())
    }

    fn complete(root: &Path, profile: RootProfileV1) -> CompleteStrictLocalSnapshotV1 {
        match capture_strict_local_snapshot_v1(root, profile).unwrap() {
            StrictLocalSnapshotReadV1::Complete(snapshot) => snapshot,
            StrictLocalSnapshotReadV1::Incomplete(incomplete) => {
                panic!("expected complete snapshot, got {incomplete:?}")
            }
        }
    }

    fn pending(root: &Path, profile: RootProfileV1) -> PendingStrictLocalSnapshotV1 {
        match begin_strict_local_snapshot_v1(root, profile).unwrap() {
            StrictLocalSnapshotHoldReadV1::Pending(pending) => pending,
            StrictLocalSnapshotHoldReadV1::Incomplete(incomplete) => {
                panic!("expected pending snapshot, got {incomplete:?}")
            }
        }
    }

    fn finish_incomplete(pending: PendingStrictLocalSnapshotV1) -> StrictLocalSnapshotIncompleteV1 {
        match pending.revalidate_inventory_c().unwrap() {
            StrictLocalSnapshotFinishV1::Complete(complete) => {
                panic!(
                    "expected incomplete snapshot, got {:?}",
                    complete.snapshot()
                )
            }
            StrictLocalSnapshotFinishV1::Incomplete(incomplete) => incomplete,
        }
    }

    fn incomplete_kind(root: &Path, profile: RootProfileV1) -> StrictLocalSnapshotIncompleteKindV1 {
        match capture_strict_local_snapshot_v1(root, profile).unwrap() {
            StrictLocalSnapshotReadV1::Complete(snapshot) => {
                panic!("expected incomplete snapshot, got {snapshot:?}")
            }
            StrictLocalSnapshotReadV1::Incomplete(incomplete) => incomplete.kind(),
        }
    }

    #[test]
    fn stable_hidden_regular_directory_and_safe_links_have_golden_digest() {
        let (_temporary, root) = canonical_root();
        fs::create_dir(root.join(".hidden")).unwrap();
        fs::write(root.join(".hidden/payload"), b"portable bytes\n").unwrap();
        fs::set_permissions(
            root.join(".hidden/payload"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        let stable_timestamp = std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::new(1_700_000_000, 123_456_789);
        fs::File::open(root.join(".hidden/payload"))
            .unwrap()
            .set_times(
                fs::FileTimes::new()
                    .set_accessed(stable_timestamp)
                    .set_modified(stable_timestamp),
            )
            .unwrap();
        symlink(".hidden/payload", root.join("safe-link")).unwrap();
        symlink("missing-target", root.join("broken-link")).unwrap();

        let snapshot = complete(&root, RootProfileV1::AgentStaticV1);
        assert!(matches!(
            snapshot.entry(".hidden"),
            Some(StrictLocalEntryV1::Directory)
        ));
        assert!(matches!(
            snapshot.entry(".hidden/payload"),
            Some(StrictLocalEntryV1::Regular(_))
        ));
        assert!(matches!(
            snapshot.entry("safe-link"),
            Some(StrictLocalEntryV1::Symlink(link)) if link.target() == ".hidden/payload"
        ));
        assert!(matches!(
            snapshot.entry("broken-link"),
            Some(StrictLocalEntryV1::Symlink(link)) if link.target() == "missing-target"
        ));
        assert_eq!(
            snapshot.digest().to_string(),
            "b3v1:59aa9308444225642cb34a9cfff19ac45e4bc53f17b95fb522e39e3a83d61511"
        );
    }

    #[test]
    fn configured_root_requires_exact_canonical_raw_spelling() {
        let (_temporary, root) = canonical_root();
        assert!(matches!(
            capture_strict_local_snapshot_v1(&root, RootProfileV1::AgentStaticV1).unwrap(),
            StrictLocalSnapshotReadV1::Complete(_)
        ));

        let parent = root.parent().unwrap();
        let name = root.file_name().unwrap().to_str().unwrap();
        let spellings = [
            PathBuf::from(format!("{}//{name}", parent.display())),
            PathBuf::from(format!("{}/./{name}", parent.display())),
            PathBuf::from(format!("{}/", root.display())),
        ];
        for spelling in spellings {
            assert_eq!(
                incomplete_kind(&spelling, RootProfileV1::AgentStaticV1),
                StrictLocalSnapshotIncompleteKindV1::InvalidConfiguredRoot,
                "spelling must fail: {}",
                spelling.display()
            );
        }

        let alias = parent.join("root-alias");
        symlink(&root, &alias).unwrap();
        assert_eq!(
            incomplete_kind(&alias, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::InvalidConfiguredRoot
        );
    }

    #[test]
    fn profiles_apply_distinct_dot_git_contracts() {
        let (_temporary, root) = canonical_root();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
        fs::write(root.join("visible"), b"x").unwrap();

        let agent = complete(&root, RootProfileV1::AgentStaticV1);
        assert!(agent.entry(".git").is_none());
        assert!(agent.entry(".git/HEAD").is_none());
        assert!(agent.entry("visible").is_some());

        let git = complete(&root, RootProfileV1::GitRawV1);
        assert!(matches!(
            git.entry(".git"),
            Some(StrictLocalEntryV1::Directory)
        ));
        assert!(matches!(
            git.entry(".git/HEAD"),
            Some(StrictLocalEntryV1::Regular(_))
        ));
        assert_ne!(agent.digest(), git.digest());
    }

    #[test]
    fn git_raw_rejects_gitfile_and_nested_dot_git() {
        let (_temporary, root) = canonical_root();
        fs::write(root.join(".git"), b"gitdir: /elsewhere\n").unwrap();
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::GitRawV1),
            StrictLocalSnapshotIncompleteKindV1::GitLayoutRejected
        );

        fs::remove_file(root.join(".git")).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        fs::create_dir(root.join("nested/.git")).unwrap();
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::GitRawV1),
            StrictLocalSnapshotIncompleteKindV1::GitLayoutRejected
        );
    }

    #[test]
    fn included_hardlinks_are_rejected() {
        let (_temporary, root) = canonical_root();
        fs::write(root.join("first"), b"same inode").unwrap();
        fs::hard_link(root.join("first"), root.join("second")).unwrap();
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::HardlinkRejected
        );
    }

    #[test]
    fn special_permission_policy_applies_only_to_regular_files() {
        let (_temporary, root) = canonical_root();
        let executable = root.join("executable");
        fs::write(&executable, b"payload").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o4755)).unwrap();
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::UnsafeModeRejected
        );

        fs::remove_file(&executable).unwrap();
        let sticky = root.join("sticky");
        fs::create_dir(&sticky).unwrap();
        fs::set_permissions(&sticky, fs::Permissions::from_mode(0o1777)).unwrap();
        fs::write(sticky.join("payload"), b"ordinary").unwrap();
        let sticky_digest = complete(&root, RootProfileV1::AgentStaticV1).digest();
        fs::set_permissions(&sticky, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            sticky_digest,
            complete(&root, RootProfileV1::AgentStaticV1).digest()
        );
    }

    #[test]
    fn openat2_mount_proof_rejects_a_descendant_mount() {
        if !Path::new("/proc").is_dir() {
            return;
        }
        let root = fs::File::open("/").unwrap();
        let proc_name = CString::new("proc").unwrap();
        let error = openat_descendant_identity(
            root.as_raw_fd(),
            &proc_name,
            Some(b"proc"),
            "test-proc-mount-boundary",
        )
        .expect_err("procfs must be a descendant mount boundary");
        assert_eq!(
            error.kind,
            StrictLocalSnapshotIncompleteKindV1::MountBoundaryRejected
        );
    }

    #[test]
    fn regular_hash_work_limits_fail_before_content_reads() {
        let (_temporary, root) = canonical_root();
        fs::File::create(root.join("oversized"))
            .unwrap()
            .set_len(MAX_REGULAR_FILE_BYTES_V1 + 1)
            .unwrap();
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded
        );

        fs::remove_file(root.join("oversized")).unwrap();
        for index in 0..=(MAX_TOTAL_HASHED_BYTES_V1 / MAX_REGULAR_FILE_BYTES_V1) {
            fs::File::create(root.join(format!("sparse-{index}")))
                .unwrap()
                .set_len(MAX_REGULAR_FILE_BYTES_V1)
                .unwrap();
        }
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded
        );
    }

    #[test]
    fn fifo_and_socket_are_rejected_without_blocking() {
        let (_temporary, root) = canonical_root();
        let fifo = root.join("fifo");
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: the path is NUL terminated and points into the test root.
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::UnsupportedObjectRejected
        );

        fs::remove_file(&fifo).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(root.join("socket")).unwrap();
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::UnsupportedObjectRejected
        );
    }

    #[test]
    fn escaping_symlink_is_rejected() {
        let (_temporary, root) = canonical_root();
        fs::create_dir(root.join("nested")).unwrap();
        symlink("../../outside", root.join("nested/escape")).unwrap();
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::SymlinkTargetRejected
        );
    }

    #[test]
    fn portable_case_alias_is_rejected_when_filesystem_can_represent_it() {
        let (_temporary, root) = canonical_root();
        fs::write(root.join("Case"), b"one").unwrap();
        if fs::write(root.join("case"), b"two").is_err() {
            return;
        }
        let first = fs::metadata(root.join("Case")).unwrap();
        let second = fs::metadata(root.join("case")).unwrap();
        if first.dev() == second.dev() && first.ino() == second.ino() {
            return;
        }
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::NamespaceRejected
        );
    }

    #[test]
    fn component_depth_256_is_allowed_but_257_is_incomplete() {
        let (_temporary, root) = canonical_root();
        let mut deepest = root.clone();
        let mut depth_255 = None;
        for component_index in 0..MAX_DEPTH_V1 {
            deepest.push("d");
            fs::create_dir(&deepest).unwrap();
            if component_index + 1 == MAX_DEPTH_V1 - 1 {
                depth_255 = Some(deepest.clone());
            }
        }
        fs::write(depth_255.unwrap().join("boundary-file"), b"x").unwrap();
        let snapshot = complete(&root, RootProfileV1::AgentStaticV1);
        assert_eq!(snapshot.entries().len(), MAX_DEPTH_V1);

        fs::write(deepest.join("too-deep"), b"x").unwrap();
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::AcquisitionLimitExceeded
        );
    }

    #[test]
    fn mutation_between_inventory_a_and_leaf_capture_is_incomplete() {
        let (_temporary, root) = canonical_root();
        let payload = root.join("payload");
        fs::write(&payload, b"before").unwrap();
        let hook_payload = payload.clone();
        let _hook = install_test_hook(move |point| {
            if point == TestHookPoint::InventoryAToLeaf {
                fs::write(&hook_payload, b"after!").unwrap();
            }
        });
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead
        );
    }

    #[test]
    fn mutation_between_leaf_capture_and_inventory_b_is_incomplete() {
        let (_temporary, root) = canonical_root();
        let payload = root.join("payload");
        fs::write(&payload, b"before").unwrap();
        let hook_payload = payload.clone();
        let _hook = install_test_hook(move |point| {
            if point == TestHookPoint::LeafToInventoryB {
                fs::write(&hook_payload, b"after!").unwrap();
            }
        });
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead
        );
    }

    #[test]
    fn mutation_during_held_external_read_window_is_incomplete() {
        let (_temporary, root) = canonical_root();
        let payload = root.join("payload");
        fs::write(&payload, b"before").unwrap();
        let pending = pending(&root, RootProfileV1::AgentStaticV1);
        assert_eq!(pending.canonical_local_root(), root);

        fs::write(&payload, b"after!").unwrap();
        assert_eq!(
            finish_incomplete(pending).kind(),
            StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead
        );
    }

    #[test]
    fn configured_route_replacement_during_held_window_is_incomplete() {
        let (_temporary, root) = canonical_root();
        fs::write(root.join("payload"), b"stable").unwrap();
        let pending = pending(&root, RootProfileV1::AgentStaticV1);

        let original = root.with_extension("original");
        fs::rename(&root, &original).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("payload"), b"stable").unwrap();

        let incomplete = finish_incomplete(pending);
        assert_eq!(
            incomplete.kind(),
            StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead
        );
        assert_eq!(
            incomplete.operation(),
            // Renaming the original directory changes the identity observed
            // through the still-held descriptor, so inventory C rejects before
            // the final route reopen is even needed.
            "compare-verify-root-before-walk"
        );
    }

    #[test]
    fn pending_and_revalidated_capabilities_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PendingStrictLocalSnapshotV1>();
        assert_send_sync::<RevalidatedStrictLocalSnapshotV1>();
    }

    #[test]
    fn same_size_in_place_rewrite_during_hash_is_incomplete() {
        let (_temporary, root) = canonical_root();
        let payload = root.join("payload");
        let byte_count = HASH_BUFFER_BYTES_V1 * 2;
        fs::write(&payload, vec![b'a'; byte_count]).unwrap();
        let fired = Rc::new(Cell::new(false));
        let hook_fired = Rc::clone(&fired);
        let hook_payload = payload.clone();
        let _hook = install_test_hook(move |point| {
            if point == TestHookPoint::RegularAfterFirstHashChunk && !hook_fired.replace(true) {
                fs::write(&hook_payload, vec![b'b'; byte_count]).unwrap();
            }
        });
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead
        );
        assert!(fired.get());
    }

    #[test]
    fn leaf_replacement_after_initial_probe_is_incomplete() {
        let (_temporary, root) = canonical_root();
        let payload = root.join("payload");
        let parked = root.join("parked");
        fs::write(&payload, b"same-size").unwrap();
        let fired = Rc::new(Cell::new(false));
        let hook_fired = Rc::clone(&fired);
        let hook_payload = payload.clone();
        let hook_parked = parked.clone();
        let _hook = install_test_hook(move |point| {
            if point == TestHookPoint::RegularAfterInitialProbe && !hook_fired.replace(true) {
                fs::rename(&hook_payload, &hook_parked).unwrap();
                fs::write(&hook_payload, b"same-size").unwrap();
            }
        });
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead
        );
        assert!(fired.get());
    }

    #[test]
    fn parent_rename_during_descriptor_walk_is_incomplete() {
        let (_temporary, root) = canonical_root();
        let parent = root.join("parent");
        let moved = root.join("moved-parent");
        fs::create_dir(&parent).unwrap();
        fs::write(parent.join("payload"), b"payload").unwrap();
        let fired = Rc::new(Cell::new(false));
        let hook_fired = Rc::clone(&fired);
        let hook_parent = parent.clone();
        let hook_moved = moved.clone();
        let _hook = install_test_hook(move |point| {
            if point == TestHookPoint::ParentWalkAfterComponentOpen && !hook_fired.replace(true) {
                fs::rename(&hook_parent, &hook_moved).unwrap();
                fs::create_dir(&hook_parent).unwrap();
            }
        });
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead
        );
        assert!(fired.get());
    }

    #[test]
    fn configured_root_path_replacement_before_reopen_is_incomplete() {
        let (_temporary, root) = canonical_root();
        fs::write(root.join("payload"), b"before").unwrap();
        let original = root.with_extension("original");
        let hook_root = root.clone();
        let hook_original = original.clone();
        let _hook = install_test_hook(move |point| {
            if point == TestHookPoint::RootPathBeforeReopen {
                fs::rename(&hook_root, &hook_original).unwrap();
                fs::create_dir(&hook_root).unwrap();
                fs::write(hook_root.join("payload"), b"before").unwrap();
            }
        });
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead
        );
    }

    #[derive(Debug, PartialEq, Eq)]
    struct NoWriteRecord {
        kind: u32,
        mode: u32,
        size: u64,
        modified: Option<std::time::SystemTime>,
        bytes_or_target: Vec<u8>,
    }

    fn no_write_inventory(root: &Path) -> BTreeMap<PathBuf, NoWriteRecord> {
        fn walk(root: &Path, rel: &Path, output: &mut BTreeMap<PathBuf, NoWriteRecord>) {
            let directory = root.join(rel);
            let mut names: Vec<_> = fs::read_dir(&directory)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            for name in names {
                let child_rel = rel.join(name);
                let path = root.join(&child_rel);
                let metadata = fs::symlink_metadata(&path).unwrap();
                let file_type = metadata.file_type();
                let bytes_or_target = if file_type.is_file() {
                    fs::read(&path).unwrap()
                } else if file_type.is_symlink() {
                    fs::read_link(&path)
                        .unwrap()
                        .as_os_str()
                        .as_bytes()
                        .to_vec()
                } else {
                    Vec::new()
                };
                output.insert(
                    child_rel.clone(),
                    NoWriteRecord {
                        kind: metadata.mode() & libc::S_IFMT,
                        mode: metadata.mode(),
                        size: metadata.size(),
                        modified: metadata.modified().ok(),
                        bytes_or_target,
                    },
                );
                if file_type.is_dir() {
                    walk(root, &child_rel, output);
                }
            }
        }
        let mut result = BTreeMap::new();
        walk(root, Path::new(""), &mut result);
        result
    }

    #[test]
    fn capture_does_not_modify_tree_contents_or_metadata_contract() {
        let (_temporary, root) = canonical_root();
        fs::create_dir(root.join("dir")).unwrap();
        fs::write(root.join("dir/file"), b"payload").unwrap();
        symlink("dir/file", root.join("link")).unwrap();
        let before = no_write_inventory(&root);
        let _ = complete(&root, RootProfileV1::AgentStaticV1);
        let after = no_write_inventory(&root);
        assert_eq!(before, after);
    }

    #[test]
    fn symlink_debug_redacts_exact_target() {
        let target = "sensitive-but-valid-relative-target";
        let link = StrictLocalSymlinkV1 {
            target: target.to_owned(),
            target_blake3: *blake3::hash(target.as_bytes()).as_bytes(),
        };
        let debug = format!("{link:?}");
        assert!(!debug.contains(target));
        assert!(debug.contains("target_len"));
        assert!(debug.contains("target_blake3"));
    }

    #[test]
    fn semantic_digest_does_not_bind_acquisition_proof_bytes() {
        let (_temporary, root) = canonical_root();
        fs::write(root.join("visible"), b"read").unwrap();
        let snapshot = complete(&root, RootProfileV1::AgentStaticV1);
        let digest = snapshot.digest();
        let mut alternate_proof = snapshot.clone();
        alternate_proof.acquisition_fingerprint =
            StrictLocalSnapshotAcquisitionFingerprintV1([0x5a; 32]);
        assert_ne!(
            snapshot.acquisition_fingerprint(),
            alternate_proof.acquisition_fingerprint()
        );
        assert_eq!(digest, alternate_proof.digest());
    }

    #[test]
    fn fixed_deny_entries_are_inventory_only_and_semantically_omitted() {
        let (_temporary, root) = canonical_root();
        fs::write(root.join(".env"), b"not read").unwrap();
        fs::write(root.join("visible"), b"read").unwrap();
        let snapshot = complete(&root, RootProfileV1::AgentStaticV1);
        assert!(snapshot.entry(".env").is_none());
        assert!(snapshot.entry("visible").is_some());
    }

    #[test]
    fn adding_an_empty_directory_does_not_change_semantic_digest() {
        let (_temporary, root) = canonical_root();
        fs::write(root.join("visible"), b"read").unwrap();
        let before = complete(&root, RootProfileV1::AgentStaticV1).digest();
        fs::create_dir(root.join("empty")).unwrap();
        let with_empty = complete(&root, RootProfileV1::AgentStaticV1);
        assert!(with_empty.entry("empty").is_none());
        let (folded_path, empty_claim) = with_empty
            .namespace_claims()
            .find(|(_, claim)| claim.exact_path() == "empty")
            .expect("ignored empty directory must retain a namespace claim");
        assert_eq!(folded_path, "empty");
        assert_eq!(empty_claim.role(), StrictLocalNamespaceRoleV1::Directory);
        assert_eq!(before, with_empty.digest());
        fs::remove_dir(root.join("empty")).unwrap();
        assert_eq!(
            before,
            complete(&root, RootProfileV1::AgentStaticV1).digest()
        );
    }

    #[test]
    fn excluded_directory_payload_churn_is_ignored() {
        let (_temporary, root) = canonical_root();
        let denied = root.join(".ssh");
        fs::create_dir(&denied).unwrap();
        let payload = denied.join("id");
        fs::write(&payload, b"before").unwrap();
        fs::write(root.join("visible"), b"read").unwrap();
        let mutation = payload.clone();
        let _hook = install_test_hook(move |point| {
            if point == TestHookPoint::LeafToInventoryB {
                fs::write(&mutation, b"after!").unwrap();
            }
        });
        let snapshot = complete(&root, RootProfileV1::AgentStaticV1);
        assert!(snapshot.entry(".ssh").is_none());
        assert!(snapshot.entry(".ssh/id").is_none());
    }

    #[test]
    fn excluded_directory_replacement_is_detected() {
        let (_temporary, root) = canonical_root();
        let denied = root.join(".ssh");
        fs::create_dir(&denied).unwrap();
        fs::write(denied.join("id"), b"before").unwrap();
        let moved = root.with_extension("excluded-old");
        let hook_denied = denied.clone();
        let _hook = install_test_hook(move |point| {
            if point == TestHookPoint::LeafToInventoryB {
                fs::rename(&hook_denied, &moved).unwrap();
                fs::create_dir(&hook_denied).unwrap();
            }
        });
        assert_eq!(
            incomplete_kind(&root, RootProfileV1::AgentStaticV1),
            StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead
        );
    }
}
