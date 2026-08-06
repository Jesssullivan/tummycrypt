//! Versioned, config-independent ingress exclusions.
//!
//! Registered-root plans must bind the exact deny-set used when remote
//! payloads are admitted. Keeping that policy in `tcfs-core` gives profile
//! fingerprints and every consumer one dependency-neutral source of truth.

use std::fmt;
use std::path::{Component, Path};

/// Canonical encoding version for the fixed-ingress policy schema.
pub const FIXED_INGRESS_POLICY_SCHEMA_VERSION_V1: u32 = 1;

const FIXED_INGRESS_POLICY_FINGERPRINT_DOMAIN_V1: &str =
    "tinyland.tcfs.fixed-ingress-policy-schema.b3v1";
const FIXED_INGRESS_POLICY_NAME_V1: &str = "fixed-ingress-path-components-v1";
const FIXED_INGRESS_EVALUATION_ORDER_V1: &str =
    "path-wide-git-rules-then-component-major-security-rules-v1";

const SECURITY_DIRECTORY_NAMES_V1: &[&str] = &[".ssh", ".gnupg", "sops-nix"];
const SECURITY_EXACT_FILE_NAMES_V1: &[&str] = &[
    ".credentials.json",
    "auth.json",
    ".netrc",
    ".pgpass",
    "master.key",
];
const LIVE_DATABASE_SUFFIXES_V1: &[&str] = &[
    ".sqlite",
    ".sqlite3",
    ".sqlite-wal",
    ".sqlite-shm",
    ".db",
    ".db-wal",
    ".db-shm",
];

const GIT_WORKTREES_PATTERNS_V1: &[&str] = &[".git", "worktrees"];
const GIT_LOCK_PATTERNS_V1: &[&str] = &[".git", ".lock"];
const GIT_TCFS_UNDO_PATTERNS_V1: &[&str] = &[".git", "tcfs-undo"];
const ROTATION_PENDING_PATTERNS_V1: &[&str] = &[".rotate-pending"];
const ROTATION_STATE_PATTERNS_V1: &[&str] = &[".rotate-state.json"];
const ATOMIC_WRITE_TEMP_PARAMETERS_V1: &[&str] = &[
    ".tmp.",
    "target-starts-with-dot",
    "nonce-length-32",
    "nonce-ascii-hex",
];
const ATOMIC_WRITE_TEMP_NONCE_LENGTH_V1: usize = 32;
const DOTENV_PATTERNS_V1: &[&str] = &["exact:.env", "prefix:.env.", "suffix:.env"];

const GIT_WORKTREES_LABELS_V1: &[&str] = &[".git/worktrees admin area"];
const GIT_LOCK_LABELS_V1: &[&str] = &["git lockfile"];
const GIT_TCFS_UNDO_LABELS_V1: &[&str] = &[".git/tcfs-undo bundle"];
const ROTATION_PENDING_LABELS_V1: &[&str] = &["tcfs-rotation-pending-key"];
const ROTATION_STATE_LABELS_V1: &[&str] = &["tcfs-rotation-state"];
const ATOMIC_WRITE_TEMP_LABELS_V1: &[&str] = &["tcfs-atomic-write-temp"];
const DOTENV_LABELS_V1: &[&str] = &["dotenv"];
const LIVE_DATABASE_LABELS_V1: &[&str] = &["live-db"];

/// One rule in the canonical fixed-ingress evaluation order.
///
/// Reordering, adding, or changing a rule is a policy-schema change and must
/// produce a different [`FixedIngressPolicySchemaFingerprintV1`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FixedIngressRuleV1 {
    GitWorktreesAdmin,
    GitLockFile,
    GitTcfsUndo,
    SecurityDirectory,
    SecurityExactFile,
    MasterKeyRotationPending,
    MasterKeyRotationState,
    AtomicWriteTemporary,
    Dotenv,
    LiveDatabase,
}

impl FixedIngressRuleV1 {
    /// Stable rule name used by the schema fingerprint.
    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::GitWorktreesAdmin => "git-worktrees-admin-v1",
            Self::GitLockFile => "git-lock-file-v1",
            Self::GitTcfsUndo => "git-tcfs-undo-v1",
            Self::SecurityDirectory => "security-directory-v1",
            Self::SecurityExactFile => "security-exact-file-v1",
            Self::MasterKeyRotationPending => "master-key-rotation-pending-v1",
            Self::MasterKeyRotationState => "master-key-rotation-state-v1",
            Self::AtomicWriteTemporary => "atomic-write-temporary-v1",
            Self::Dotenv => "dotenv-v1",
            Self::LiveDatabase => "live-database-v1",
        }
    }

    fn schema(self) -> FixedIngressRuleSchemaV1 {
        match self {
            Self::GitWorktreesAdmin => FixedIngressRuleSchemaV1 {
                rule: self,
                matcher: "adjacent-components-ascii-case-insensitive-v1",
                patterns: GIT_WORKTREES_PATTERNS_V1,
                diagnostic_labels: GIT_WORKTREES_LABELS_V1,
            },
            Self::GitLockFile => FixedIngressRuleSchemaV1 {
                rule: self,
                matcher: "final-suffix-under-component-ascii-case-insensitive-v1",
                patterns: GIT_LOCK_PATTERNS_V1,
                diagnostic_labels: GIT_LOCK_LABELS_V1,
            },
            Self::GitTcfsUndo => FixedIngressRuleSchemaV1 {
                rule: self,
                matcher: "adjacent-components-ascii-case-insensitive-v1",
                patterns: GIT_TCFS_UNDO_PATTERNS_V1,
                diagnostic_labels: GIT_TCFS_UNDO_LABELS_V1,
            },
            Self::SecurityDirectory => FixedIngressRuleSchemaV1 {
                rule: self,
                matcher: "any-component-exact-ascii-case-insensitive-v1",
                patterns: SECURITY_DIRECTORY_NAMES_V1,
                diagnostic_labels: SECURITY_DIRECTORY_NAMES_V1,
            },
            Self::SecurityExactFile => FixedIngressRuleSchemaV1 {
                rule: self,
                matcher: "any-component-exact-ascii-case-insensitive-v1",
                patterns: SECURITY_EXACT_FILE_NAMES_V1,
                diagnostic_labels: SECURITY_EXACT_FILE_NAMES_V1,
            },
            Self::MasterKeyRotationPending => FixedIngressRuleSchemaV1 {
                rule: self,
                matcher: "any-component-suffix-ascii-case-insensitive-v1",
                patterns: ROTATION_PENDING_PATTERNS_V1,
                diagnostic_labels: ROTATION_PENDING_LABELS_V1,
            },
            Self::MasterKeyRotationState => FixedIngressRuleSchemaV1 {
                rule: self,
                matcher: "any-component-suffix-ascii-case-insensitive-v1",
                patterns: ROTATION_STATE_PATTERNS_V1,
                diagnostic_labels: ROTATION_STATE_LABELS_V1,
            },
            Self::AtomicWriteTemporary => FixedIngressRuleSchemaV1 {
                rule: self,
                matcher: "hidden-target-atomic-temp-shape-ascii-case-insensitive-v1",
                patterns: ATOMIC_WRITE_TEMP_PARAMETERS_V1,
                diagnostic_labels: ATOMIC_WRITE_TEMP_LABELS_V1,
            },
            Self::Dotenv => FixedIngressRuleSchemaV1 {
                rule: self,
                matcher: "dotenv-family-ascii-case-insensitive-v1",
                patterns: DOTENV_PATTERNS_V1,
                diagnostic_labels: DOTENV_LABELS_V1,
            },
            Self::LiveDatabase => FixedIngressRuleSchemaV1 {
                rule: self,
                matcher: "any-component-suffix-ascii-case-insensitive-v1",
                patterns: LIVE_DATABASE_SUFFIXES_V1,
                diagnostic_labels: LIVE_DATABASE_LABELS_V1,
            },
        }
    }
}

const FIXED_INGRESS_RULE_ORDER_V1: [FixedIngressRuleV1; 10] = [
    FixedIngressRuleV1::GitWorktreesAdmin,
    FixedIngressRuleV1::GitLockFile,
    FixedIngressRuleV1::GitTcfsUndo,
    FixedIngressRuleV1::SecurityDirectory,
    FixedIngressRuleV1::SecurityExactFile,
    FixedIngressRuleV1::MasterKeyRotationPending,
    FixedIngressRuleV1::MasterKeyRotationState,
    FixedIngressRuleV1::AtomicWriteTemporary,
    FixedIngressRuleV1::Dotenv,
    FixedIngressRuleV1::LiveDatabase,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FixedIngressRuleSchemaV1 {
    rule: FixedIngressRuleV1,
    matcher: &'static str,
    patterns: &'static [&'static str],
    diagnostic_labels: &'static [&'static str],
}

/// Typed result from the config-independent ingress deny-set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FixedIngressDenyReasonV1 {
    rule: FixedIngressRuleV1,
    label: &'static str,
}

impl FixedIngressDenyReasonV1 {
    /// The canonical rule that rejected the path.
    pub const fn rule(self) -> FixedIngressRuleV1 {
        self.rule
    }

    /// Stable diagnostic label associated with the matching rule.
    pub const fn label(self) -> &'static str {
        self.label
    }
}

/// Opaque BLAKE3 identity for the complete V1 policy schema.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FixedIngressPolicySchemaFingerprintV1([u8; 32]);

impl FixedIngressPolicySchemaFingerprintV1 {
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for FixedIngressPolicySchemaFingerprintV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("b3v1:")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for FixedIngressPolicySchemaFingerprintV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "FixedIngressPolicySchemaFingerprintV1({self})")
    }
}

/// Immutable V1 fixed-ingress policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FixedIngressPolicyV1;

impl FixedIngressPolicyV1 {
    pub const fn strict_v1() -> Self {
        Self
    }

    pub const fn canonical_name(self) -> &'static str {
        FIXED_INGRESS_POLICY_NAME_V1
    }

    /// Rules in their canonical evaluation and fingerprint order.
    pub const fn ordered_rules(self) -> &'static [FixedIngressRuleV1] {
        &FIXED_INGRESS_RULE_ORDER_V1
    }

    /// Fingerprint of matcher semantics, membership, labels, and rule order.
    pub fn schema_fingerprint(self) -> FixedIngressPolicySchemaFingerprintV1 {
        let schemas = FIXED_INGRESS_RULE_ORDER_V1.map(FixedIngressRuleV1::schema);
        fingerprint_fixed_ingress_schema_v1(&schemas)
    }

    /// Return the first V1 rule that rejects `path`.
    ///
    /// The three Git topology rules are path-wide and ordered first. Remaining
    /// security rules are then evaluated in canonical rule order for each path
    /// component, preserving the V1 first-match diagnostic contract.
    pub fn classify_path(self, path: &Path) -> Option<FixedIngressDenyReasonV1> {
        if let Some(denied) = self.ordered_rules()[..3]
            .iter()
            .find_map(|rule| classify_rule_v1(*rule, path))
        {
            return Some(denied);
        }
        for component in path.components() {
            let Some(name) = normal_component_v1(component) else {
                continue;
            };
            if let Some(denied) = self.ordered_rules()[3..]
                .iter()
                .find_map(|rule| classify_security_component_v1(*rule, name))
            {
                return Some(denied);
            }
        }
        None
    }
}

fn classify_rule_v1(rule: FixedIngressRuleV1, path: &Path) -> Option<FixedIngressDenyReasonV1> {
    let label = match rule {
        FixedIngressRuleV1::GitWorktreesAdmin => has_adjacent_components_v1(
            path,
            GIT_WORKTREES_PATTERNS_V1[0],
            GIT_WORKTREES_PATTERNS_V1[1],
        )
        .then_some(GIT_WORKTREES_LABELS_V1[0]),
        FixedIngressRuleV1::GitLockFile => {
            let name = path.file_name()?.to_str()?;
            (name.to_ascii_lowercase().ends_with(GIT_LOCK_PATTERNS_V1[1])
                && has_component_v1(path, GIT_LOCK_PATTERNS_V1[0]))
            .then_some(GIT_LOCK_LABELS_V1[0])
        }
        FixedIngressRuleV1::GitTcfsUndo => has_adjacent_components_v1(
            path,
            GIT_TCFS_UNDO_PATTERNS_V1[0],
            GIT_TCFS_UNDO_PATTERNS_V1[1],
        )
        .then_some(GIT_TCFS_UNDO_LABELS_V1[0]),
        FixedIngressRuleV1::SecurityDirectory
        | FixedIngressRuleV1::SecurityExactFile
        | FixedIngressRuleV1::MasterKeyRotationPending
        | FixedIngressRuleV1::MasterKeyRotationState
        | FixedIngressRuleV1::AtomicWriteTemporary
        | FixedIngressRuleV1::Dotenv
        | FixedIngressRuleV1::LiveDatabase => return None,
    }?;
    Some(FixedIngressDenyReasonV1 { rule, label })
}

fn classify_security_component_v1(
    rule: FixedIngressRuleV1,
    name: &str,
) -> Option<FixedIngressDenyReasonV1> {
    let folded = name.to_ascii_lowercase();
    let label = match rule {
        FixedIngressRuleV1::SecurityDirectory => {
            first_exact_name_v1(name, SECURITY_DIRECTORY_NAMES_V1)
        }
        FixedIngressRuleV1::SecurityExactFile => {
            first_exact_name_v1(name, SECURITY_EXACT_FILE_NAMES_V1)
        }
        FixedIngressRuleV1::MasterKeyRotationPending => folded
            .ends_with(ROTATION_PENDING_PATTERNS_V1[0])
            .then_some(ROTATION_PENDING_LABELS_V1[0]),
        FixedIngressRuleV1::MasterKeyRotationState => folded
            .ends_with(ROTATION_STATE_PATTERNS_V1[0])
            .then_some(ROTATION_STATE_LABELS_V1[0]),
        FixedIngressRuleV1::AtomicWriteTemporary => {
            is_atomic_write_temp_v1(name).then_some(ATOMIC_WRITE_TEMP_LABELS_V1[0])
        }
        FixedIngressRuleV1::Dotenv => {
            (folded == ".env" || folded.starts_with(".env.") || folded.ends_with(".env"))
                .then_some(DOTENV_LABELS_V1[0])
        }
        FixedIngressRuleV1::LiveDatabase => LIVE_DATABASE_SUFFIXES_V1
            .iter()
            .any(|suffix| folded.ends_with(suffix))
            .then_some(LIVE_DATABASE_LABELS_V1[0]),
        FixedIngressRuleV1::GitWorktreesAdmin
        | FixedIngressRuleV1::GitLockFile
        | FixedIngressRuleV1::GitTcfsUndo => return None,
    }?;
    Some(FixedIngressDenyReasonV1 { rule, label })
}

fn normal_component_v1<'a>(component: Component<'a>) -> Option<&'a str> {
    match component {
        Component::Normal(name) => name.to_str(),
        _ => None,
    }
}

fn has_component_v1(path: &Path, expected: &str) -> bool {
    path.components()
        .filter_map(normal_component_v1)
        .any(|name| name.eq_ignore_ascii_case(expected))
}

fn has_adjacent_components_v1(path: &Path, parent: &str, child: &str) -> bool {
    let mut previous_matches = false;
    for component in path.components() {
        let Some(name) = normal_component_v1(component) else {
            previous_matches = false;
            continue;
        };
        if previous_matches && name.eq_ignore_ascii_case(child) {
            return true;
        }
        previous_matches = name.eq_ignore_ascii_case(parent);
    }
    false
}

fn first_exact_name_v1(
    name: &str,
    expected_names: &'static [&'static str],
) -> Option<&'static str> {
    expected_names
        .iter()
        .find(|expected| name.eq_ignore_ascii_case(expected))
        .copied()
}

fn is_atomic_write_temp_v1(name: &str) -> bool {
    let folded = name.to_ascii_lowercase();
    let Some((target, nonce)) = folded.rsplit_once(ATOMIC_WRITE_TEMP_PARAMETERS_V1[0]) else {
        return false;
    };
    target.starts_with('.')
        && nonce.len() == ATOMIC_WRITE_TEMP_NONCE_LENGTH_V1
        && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn update_schema_string_v1(hasher: &mut blake3::Hasher, value: &str) {
    let len = u32::try_from(value.len()).expect("fixed-ingress schema string length must fit u32");
    hasher.update(&len.to_be_bytes());
    hasher.update(value.as_bytes());
}

fn fingerprint_fixed_ingress_schema_v1(
    schemas: &[FixedIngressRuleSchemaV1],
) -> FixedIngressPolicySchemaFingerprintV1 {
    let mut hasher = blake3::Hasher::new_derive_key(FIXED_INGRESS_POLICY_FINGERPRINT_DOMAIN_V1);
    hasher.update(&FIXED_INGRESS_POLICY_SCHEMA_VERSION_V1.to_be_bytes());
    update_schema_string_v1(&mut hasher, FIXED_INGRESS_POLICY_NAME_V1);
    update_schema_string_v1(&mut hasher, FIXED_INGRESS_EVALUATION_ORDER_V1);
    let rule_count =
        u32::try_from(schemas.len()).expect("fixed-ingress schema rule count must fit u32");
    hasher.update(&rule_count.to_be_bytes());
    for (ordinal, schema) in schemas.iter().enumerate() {
        let ordinal = u32::try_from(ordinal).expect("fixed-ingress rule ordinal must fit u32");
        hasher.update(&ordinal.to_be_bytes());
        update_schema_string_v1(&mut hasher, schema.rule.canonical_name());
        update_schema_string_v1(&mut hasher, schema.matcher);
        let pattern_count = u32::try_from(schema.patterns.len())
            .expect("fixed-ingress schema pattern count must fit u32");
        hasher.update(&pattern_count.to_be_bytes());
        for pattern in schema.patterns {
            update_schema_string_v1(&mut hasher, pattern);
        }
        let label_count = u32::try_from(schema.diagnostic_labels.len())
            .expect("fixed-ingress schema diagnostic-label count must fit u32");
        hasher.update(&label_count.to_be_bytes());
        for label in schema.diagnostic_labels {
            update_schema_string_v1(&mut hasher, label);
        }
    }
    FixedIngressPolicySchemaFingerprintV1(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_rules_are_closed_and_canonical() {
        assert_eq!(
            FixedIngressPolicyV1::strict_v1().ordered_rules(),
            &[
                FixedIngressRuleV1::GitWorktreesAdmin,
                FixedIngressRuleV1::GitLockFile,
                FixedIngressRuleV1::GitTcfsUndo,
                FixedIngressRuleV1::SecurityDirectory,
                FixedIngressRuleV1::SecurityExactFile,
                FixedIngressRuleV1::MasterKeyRotationPending,
                FixedIngressRuleV1::MasterKeyRotationState,
                FixedIngressRuleV1::AtomicWriteTemporary,
                FixedIngressRuleV1::Dotenv,
                FixedIngressRuleV1::LiveDatabase,
            ]
        );
    }

    #[test]
    fn every_fixed_ingress_rule_has_a_positive_case() {
        let policy = FixedIngressPolicyV1::strict_v1();
        let cases = [
            (
                "repo/.git/worktrees/wt/HEAD",
                FixedIngressRuleV1::GitWorktreesAdmin,
                ".git/worktrees admin area",
            ),
            (
                "repo/.git/refs/heads/main.lock",
                FixedIngressRuleV1::GitLockFile,
                "git lockfile",
            ),
            (
                "repo/.git/tcfs-undo/history.bundle",
                FixedIngressRuleV1::GitTcfsUndo,
                ".git/tcfs-undo bundle",
            ),
            (
                "home/.ssh/id_ed25519",
                FixedIngressRuleV1::SecurityDirectory,
                ".ssh",
            ),
            (
                "home/.config/auth.json",
                FixedIngressRuleV1::SecurityExactFile,
                "auth.json",
            ),
            (
                "home/.vault-key.rotate-pending",
                FixedIngressRuleV1::MasterKeyRotationPending,
                "tcfs-rotation-pending-key",
            ),
            (
                "home/.vault-key.rotate-state.json",
                FixedIngressRuleV1::MasterKeyRotationState,
                "tcfs-rotation-state",
            ),
            (
                "home/..vault-key.tmp.0123456789abcdef0123456789abcdef",
                FixedIngressRuleV1::AtomicWriteTemporary,
                "tcfs-atomic-write-temp",
            ),
            ("repo/service.env", FixedIngressRuleV1::Dotenv, "dotenv"),
            (
                "home/.codex/logs.sqlite-wal",
                FixedIngressRuleV1::LiveDatabase,
                "live-db",
            ),
        ];

        for (path, expected_rule, expected_label) in cases {
            let denied = policy
                .classify_path(Path::new(path))
                .unwrap_or_else(|| panic!("expected fixed-ingress denial for {path}"));
            assert_eq!(denied.rule(), expected_rule, "wrong rule for {path}");
            assert_eq!(denied.label(), expected_label, "wrong label for {path}");
        }
    }

    #[test]
    fn fixed_ingress_matching_is_ascii_case_insensitive() {
        let policy = FixedIngressPolicyV1::strict_v1();
        for path in [
            "repo/.GIT/WORKTREES/wt/HEAD",
            "repo/.GIT/INDEX.LOCK",
            "repo/.GIT/TCFS-UNDO/history.bundle",
            "home/.SSH/id_ed25519",
            "home/AUTH.JSON",
            "home/VAULT.ROTATE-PENDING",
            "home/VAULT.ROTATE-STATE.JSON",
            "home/.TARGET.TMP.ABCDEF0123456789ABCDEF0123456789",
            "repo/SERVICE.ENV",
            "home/STATE.SQLITE-WAL",
        ] {
            assert!(
                policy.classify_path(Path::new(path)).is_some(),
                "case alias must remain denied: {path}"
            );
        }
    }

    #[test]
    fn fixed_ingress_near_misses_remain_admissible() {
        let policy = FixedIngressPolicyV1::strict_v1();
        for path in [
            "repo/.git/worktree/wt/HEAD",
            "repo/git/worktrees/wt/HEAD",
            "repo/.git/index.locked",
            "repo/index.lock",
            "repo/.git/tcfs-undo-not/history.bundle",
            "home/.sshd/id_ed25519",
            "home/auth.json.bak",
            "home/vault.rotate-pending.bak",
            "home/vault.rotate-state.json.bak",
            "home/visible.tmp.0123456789abcdef0123456789abcdef",
            "home/.hidden.tmp.0123456789abcdef0123456789abcde",
            "home/.hidden.tmp.0123456789abcdef0123456789abcdeg",
            "repo/.envrc",
            "repo/service.environment",
            "home/state.sqlite4",
            "home/data.dbf",
        ] {
            assert_eq!(
                policy.classify_path(Path::new(path)),
                None,
                "near miss must remain admissible: {path}"
            );
        }
    }

    #[test]
    fn rule_order_is_observable_and_schema_bound() {
        let policy = FixedIngressPolicyV1::strict_v1();
        let denied = policy
            .classify_path(Path::new("home/.ssh/repo/.git/worktrees/wt/auth.json"))
            .expect("multi-match path must be denied");
        assert_eq!(denied.rule(), FixedIngressRuleV1::GitWorktreesAdmin);

        let denied = policy
            .classify_path(Path::new("home/auth.json/.ssh/id_ed25519"))
            .expect("component-order path must be denied");
        assert_eq!(denied.rule(), FixedIngressRuleV1::SecurityExactFile);

        let schemas = FIXED_INGRESS_RULE_ORDER_V1.map(FixedIngressRuleV1::schema);
        let mut reordered = schemas;
        reordered.swap(0, 1);
        assert_ne!(
            fingerprint_fixed_ingress_schema_v1(&schemas),
            fingerprint_fixed_ingress_schema_v1(&reordered)
        );
    }

    #[test]
    fn schema_fingerprint_binds_membership_and_matcher_semantics() {
        let schemas = FIXED_INGRESS_RULE_ORDER_V1.map(FixedIngressRuleV1::schema);
        assert_eq!(
            ATOMIC_WRITE_TEMP_PARAMETERS_V1[2],
            format!("nonce-length-{ATOMIC_WRITE_TEMP_NONCE_LENGTH_V1}")
        );

        let mut changed_membership = schemas;
        changed_membership[4].patterns = &["auth.json"];
        assert_ne!(
            fingerprint_fixed_ingress_schema_v1(&schemas),
            fingerprint_fixed_ingress_schema_v1(&changed_membership)
        );

        let mut changed_matcher = schemas;
        changed_matcher[4].matcher = "any-component-prefix-ascii-case-insensitive-v1";
        assert_ne!(
            fingerprint_fixed_ingress_schema_v1(&schemas),
            fingerprint_fixed_ingress_schema_v1(&changed_matcher)
        );

        let mut changed_label = schemas;
        changed_label[4].diagnostic_labels = &["redacted-auth-file"];
        assert_ne!(
            fingerprint_fixed_ingress_schema_v1(&schemas),
            fingerprint_fixed_ingress_schema_v1(&changed_label)
        );
    }

    #[test]
    fn fixed_ingress_schema_fingerprint_has_a_golden_identity() {
        let fingerprint = FixedIngressPolicyV1::strict_v1().schema_fingerprint();
        assert_eq!(fingerprint.as_bytes().len(), 32);
        assert_eq!(
            fingerprint.to_string(),
            "b3v1:155e561ee22414cad8a977a8f9e1f7ed6e4efe715fa12e66966057f3f6824a70"
        );
    }
}
