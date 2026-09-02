//! The bulkload refusal taxonomy.
//!
//! # Provenance
//!
//! The Python engine at `bulkload-refactor/.agents/skills/bulkload/scripts/
//! bulkload_lib/{scanner,executor,model}.py` does not carry machine-readable
//! refusal *codes*: it raises `BulkloadError("<prose>")` with several hundred
//! distinct human-readable messages. The names below are a faithful port of
//! the refusal *families* those messages fall into, grouped so that each
//! variant covers one class of prose refusal:
//!
//! | variant family        | representative Python prose                       |
//! |-----------------------|---------------------------------------------------|
//! | `SnapshotCustody*`    | "live snapshot custody is unavailable"             |
//! | `Digest*`             | "`AgentCaptureV4` catalog digest mismatch"           |
//! | `Git*`                | "Git authority changed during live snapshot"       |
//! | `Sqlite*`             | "`SQLite` `quick_check` failed"                        |
//! | `Path*`               | "control and format characters are forbidden ..."  |
//! | `Rollback*`           | "rollback end-state differs from its exact ..."    |
//! | `Budget*`             | "`SQLite` row capture budget exceeded"               |
//!
//! Every variant is a *refusal*: the operation declined to proceed and made no
//! partial mutation. Refusals are values, never panics (R33).

use core::fmt;

/// A bulkload refusal.
///
/// Ported from the Python engine's `BulkloadError` prose families -- see the
/// module docs for the mapping. Variants are non-exhaustive on purpose: the
/// M2/M3 lanes will grow this set as more of the engine is rebuilt.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkloadRefusal {
    // ---- custody / snapshot integrity ------------------------------------
    /// The live snapshot custody root could not be established or opened.
    SnapshotCustodyUnavailable,
    /// A payload resolved outside the custody root it claims to live under.
    SnapshotCustodyEscape,
    /// Declared snapshot roots overlap, alias, or are not unique.
    SnapshotRootsOverlap,
    /// The source tree changed after the immutable snapshot was taken.
    SourceChangedAfterSnapshot,
    /// A capture claimed ownership of a published snapshot it cannot prove.
    SnapshotOwnershipUnproven,

    // ---- digest / sealing -------------------------------------------------
    /// A content digest did not match the digest the plan was sealed against.
    DigestMismatch,
    /// A sealed object required by this stage is missing.
    SealedObjectMissing,
    /// A sealed object changed between sealing and apply.
    SealedObjectChanged,
    /// A receipt is not bound to the plan or push it claims.
    ReceiptBindingInvalid,

    // ---- schema / contract ------------------------------------------------
    /// The input is not the record type this stage accepts.
    SchemaMismatch,
    /// A required field is absent from an otherwise well-formed record.
    RequiredFieldMissing,
    /// A declared enum-like field carries a value outside its domain.
    FieldDomainViolation,
    /// Readiness or completeness disagrees with the recorded blockers.
    ContractSelfInconsistent,

    // ---- path handling ----------------------------------------------------
    /// A path that must be absolute is not.
    PathNotAbsolute,
    /// A path carries control or format characters, or an interior NUL.
    PathNotPortable,
    /// The path map is empty, detached from source authority, or unmapped.
    PathMapDetached,
    /// A path left the tree it was resolved against (traversal or symlink).
    PathEscapesRoot,

    // ---- git custody ------------------------------------------------------
    /// Git is not available on this host.
    GitUnavailable,
    /// A git authority path resolves outside the live snapshot root.
    GitAuthorityOutsideRoot,
    /// Git bytes or refs changed while the live snapshot was being taken.
    GitAuthorityChanged,
    /// A git inventory (index, refs, worktrees) is malformed.
    GitInventoryMalformed,
    /// The git destination already exists or is non-empty.
    GitDestinationOccupied,

    // ---- sqlite -----------------------------------------------------------
    /// `PRAGMA quick_check` or the foreign-key check failed.
    SqliteIntegrityCheckFailed,
    /// The database yielded a value type or magnitude the schema cannot carry.
    SqliteUnsupportedValue,
    /// Shared rows diverged between planning and apply.
    SqliteStateChanged,

    // ---- rollback / journal ------------------------------------------------
    /// The rollback snapshot required to undo this journal is missing.
    RollbackSnapshotMissing,
    /// Rollback did not reach the exact recorded before-state.
    RollbackEndStateDiverged,
    /// A journal already rolled back cannot be applied again.
    JournalAlreadyRolledBack,
    /// An existing journal belongs to a different transaction.
    JournalOwnershipConflict,

    // ---- budgets / transport ------------------------------------------------
    /// A capture, row, or spill budget was exceeded.
    BudgetExceeded,
    /// The transport authority differs from the captured authority.
    TransportAuthorityMismatch,
    /// The frame could not be encoded or decoded.
    FrameCodec,
    /// An underlying I/O operation refused; carries the OS errno when known.
    Io(Option<i32>),
}

impl BulkloadRefusal {
    /// The stable machine-readable code for this refusal.
    ///
    /// These strings are the wire/log identity of a refusal and must not change
    /// once a milestone ships.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match *self {
            Self::SnapshotCustodyUnavailable => "SNAPSHOT_CUSTODY_UNAVAILABLE",
            Self::SnapshotCustodyEscape => "SNAPSHOT_CUSTODY_ESCAPE",
            Self::SnapshotRootsOverlap => "SNAPSHOT_ROOTS_OVERLAP",
            Self::SourceChangedAfterSnapshot => "SOURCE_CHANGED_AFTER_SNAPSHOT",
            Self::SnapshotOwnershipUnproven => "SNAPSHOT_OWNERSHIP_UNPROVEN",
            Self::DigestMismatch => "DIGEST_MISMATCH",
            Self::SealedObjectMissing => "SEALED_OBJECT_MISSING",
            Self::SealedObjectChanged => "SEALED_OBJECT_CHANGED",
            Self::ReceiptBindingInvalid => "RECEIPT_BINDING_INVALID",
            Self::SchemaMismatch => "SCHEMA_MISMATCH",
            Self::RequiredFieldMissing => "REQUIRED_FIELD_MISSING",
            Self::FieldDomainViolation => "FIELD_DOMAIN_VIOLATION",
            Self::ContractSelfInconsistent => "CONTRACT_SELF_INCONSISTENT",
            Self::PathNotAbsolute => "PATH_NOT_ABSOLUTE",
            Self::PathNotPortable => "PATH_NOT_PORTABLE",
            Self::PathMapDetached => "PATH_MAP_DETACHED",
            Self::PathEscapesRoot => "PATH_ESCAPES_ROOT",
            Self::GitUnavailable => "GIT_UNAVAILABLE",
            Self::GitAuthorityOutsideRoot => "GIT_AUTHORITY_OUTSIDE_ROOT",
            Self::GitAuthorityChanged => "GIT_AUTHORITY_CHANGED",
            Self::GitInventoryMalformed => "GIT_INVENTORY_MALFORMED",
            Self::GitDestinationOccupied => "GIT_DESTINATION_OCCUPIED",
            Self::SqliteIntegrityCheckFailed => "SQLITE_INTEGRITY_CHECK_FAILED",
            Self::SqliteUnsupportedValue => "SQLITE_UNSUPPORTED_VALUE",
            Self::SqliteStateChanged => "SQLITE_STATE_CHANGED",
            Self::RollbackSnapshotMissing => "ROLLBACK_SNAPSHOT_MISSING",
            Self::RollbackEndStateDiverged => "ROLLBACK_END_STATE_DIVERGED",
            Self::JournalAlreadyRolledBack => "JOURNAL_ALREADY_ROLLED_BACK",
            Self::JournalOwnershipConflict => "JOURNAL_OWNERSHIP_CONFLICT",
            Self::BudgetExceeded => "BUDGET_EXCEEDED",
            Self::TransportAuthorityMismatch => "TRANSPORT_AUTHORITY_MISMATCH",
            Self::FrameCodec => "FRAME_CODEC",
            Self::Io(_) => "IO",
        }
    }
}

impl fmt::Display for BulkloadRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io(Some(errno)) => write!(f, "IO (errno {errno})"),
            _ => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for BulkloadRefusal {}

impl From<std::io::Error> for BulkloadRefusal {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.raw_os_error())
    }
}

impl From<postcard::Error> for BulkloadRefusal {
    fn from(_: postcard::Error) -> Self {
        Self::FrameCodec
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::BulkloadRefusal;

    /// Codes are the wire identity of a refusal: they must be unique and
    /// `SCREAMING_SNAKE_CASE`.
    #[test]
    fn codes_are_unique_and_well_formed() {
        let all = [
            BulkloadRefusal::SnapshotCustodyUnavailable,
            BulkloadRefusal::SnapshotCustodyEscape,
            BulkloadRefusal::SnapshotRootsOverlap,
            BulkloadRefusal::SourceChangedAfterSnapshot,
            BulkloadRefusal::SnapshotOwnershipUnproven,
            BulkloadRefusal::DigestMismatch,
            BulkloadRefusal::SealedObjectMissing,
            BulkloadRefusal::SealedObjectChanged,
            BulkloadRefusal::ReceiptBindingInvalid,
            BulkloadRefusal::SchemaMismatch,
            BulkloadRefusal::RequiredFieldMissing,
            BulkloadRefusal::FieldDomainViolation,
            BulkloadRefusal::ContractSelfInconsistent,
            BulkloadRefusal::PathNotAbsolute,
            BulkloadRefusal::PathNotPortable,
            BulkloadRefusal::PathMapDetached,
            BulkloadRefusal::PathEscapesRoot,
            BulkloadRefusal::GitUnavailable,
            BulkloadRefusal::GitAuthorityOutsideRoot,
            BulkloadRefusal::GitAuthorityChanged,
            BulkloadRefusal::GitInventoryMalformed,
            BulkloadRefusal::GitDestinationOccupied,
            BulkloadRefusal::SqliteIntegrityCheckFailed,
            BulkloadRefusal::SqliteUnsupportedValue,
            BulkloadRefusal::SqliteStateChanged,
            BulkloadRefusal::RollbackSnapshotMissing,
            BulkloadRefusal::RollbackEndStateDiverged,
            BulkloadRefusal::JournalAlreadyRolledBack,
            BulkloadRefusal::JournalOwnershipConflict,
            BulkloadRefusal::BudgetExceeded,
            BulkloadRefusal::TransportAuthorityMismatch,
            BulkloadRefusal::FrameCodec,
            BulkloadRefusal::Io(None),
        ];

        let mut codes: Vec<&'static str> = all.iter().map(BulkloadRefusal::code).collect();
        let total = codes.len();
        assert!(total >= 15, "taxonomy should carry at least 15 refusals");
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), total, "refusal codes must be unique");

        for code in codes {
            assert!(
                code.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                "code {code} is not SCREAMING_SNAKE_CASE"
            );
        }
    }

    #[test]
    fn io_refusal_carries_errno() {
        let refusal = BulkloadRefusal::from(std::io::Error::from_raw_os_error(2));
        assert_eq!(refusal, BulkloadRefusal::Io(Some(2)));
        assert_eq!(refusal.code(), "IO");
        assert_eq!(refusal.to_string(), "IO (errno 2)");
    }
}
