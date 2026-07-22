//! Held-window composition for one selected registered root.
//!
//! This module deliberately stops before planning. Matching remote A/B passes
//! remain non-atomic diagnostic evidence. A sibling catalog-bound observation
//! retains one internally closed immutable catalog revision, but still cannot
//! prove writer fencing, externally complete bootstrap, or currentness. Neither
//! artifact is an actionable payload projection; both expose no digest,
//! serialization, clone, or action conversion.

use anyhow::Result;
use opendal::Operator;
use std::fmt;
use std::path::{Path, PathBuf};
#[cfg(test)]
use tcfs_core::config::expand_tilde;
use tcfs_core::config::{
    RegisteredRootPlanContractFingerprintV1, RegisteredRootPlanContractV1, RegisteredRootV1Config,
    RootBindingV1Config, RootProfilePolicyV1, RootProfileSettingsFingerprintV1, RootProfileV1,
    RootSpecV1Config,
};
use tcfs_storage::ConditionalWriteSemanticsReceipt;

use crate::index_entry::PortableNamespaceRole;
use crate::registered_git_topology::{
    begin_strict_git_raw_topology_v1, HeldStrictGitRawTopologyV1, StrictGitRawTopologyBeginV1,
    StrictGitRawTopologyFinishV1, StrictGitRawTopologyIncompleteV1,
};
use crate::registered_local_snapshot::{
    begin_strict_local_snapshot_v1, RevalidatedStrictLocalSnapshotV1, StrictLocalNamespaceRoleV1,
    StrictLocalSnapshotFinishV1, StrictLocalSnapshotHoldReadV1, StrictLocalSnapshotIncompleteV1,
};
use crate::registered_reconcile::{
    read_and_bind_strict_primary_state_for_pending_root_v1, StrictPrimaryStateIncompleteV1,
    StrictPrimaryStateReadV1, StrictPrimaryStateSnapshotV1,
};
use crate::registered_remote_catalog::{
    read_semantically_bound_remote_catalog_corpus_v1, SemanticallyBoundRemoteCatalogCorpusV1,
    StrictSemanticallyBoundRemoteCatalogIncompleteV1, StrictSemanticallyBoundRemoteCatalogReadV1,
};
use crate::registered_remote_observation::{
    read_bound_remote_observation_two_pass_v1, MatchingTwoPassBoundRemoteEvidenceV1,
    RemoteNamespaceClaimOriginsV1, StrictBoundRemoteObservationIncompleteV1,
    StrictBoundRemoteObservationReadV1,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NamespaceSafetyClaimSourceV1 {
    LocalCurrent,
    StateBaseline,
    RemoteCurrent,
    RemoteHistorical,
    RemoteReservation,
}

/// Fixed-size result of validating the three already-bounded claim sources.
///
/// The composer deliberately does not retain a second owned union of paths.
/// Remote history and reservations are counted here as namespace-safety
/// evidence, but this summary is not a payload or action projection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct NamespaceSafetySummaryV1 {
    unique_claims: u64,
    unique_claim_bytes: u64,
    local_current_claims: u64,
    state_baseline_claims: u64,
    remote_current_claims: u64,
    remote_historical_claims: u64,
    remote_reservation_claims: u64,
}

impl NamespaceSafetySummaryV1 {
    #[cfg(test)]
    const fn source_claim_count(self, source: NamespaceSafetyClaimSourceV1) -> u64 {
        match source {
            NamespaceSafetyClaimSourceV1::LocalCurrent => self.local_current_claims,
            NamespaceSafetyClaimSourceV1::StateBaseline => self.state_baseline_claims,
            NamespaceSafetyClaimSourceV1::RemoteCurrent => self.remote_current_claims,
            NamespaceSafetyClaimSourceV1::RemoteHistorical => self.remote_historical_claims,
            NamespaceSafetyClaimSourceV1::RemoteReservation => self.remote_reservation_claims,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NamespaceSafetyClaimConflictKindV1 {
    FoldedSpellingAlias,
    FileDirectoryRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NamespaceSafetyClaimConflictV1 {
    kind: NamespaceSafetyClaimConflictKindV1,
    first_source: NamespaceSafetyClaimSourceV1,
    conflicting_source: NamespaceSafetyClaimSourceV1,
}

impl NamespaceSafetyClaimConflictV1 {
    pub(crate) const fn kind(self) -> NamespaceSafetyClaimConflictKindV1 {
        self.kind
    }

    pub(crate) const fn first_source(self) -> NamespaceSafetyClaimSourceV1 {
        self.first_source
    }

    pub(crate) const fn conflicting_source(self) -> NamespaceSafetyClaimSourceV1 {
        self.conflicting_source
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegisteredSourceRouteMismatchV1 {
    ConfiguredLocalRoot,
    ConfiguredStatePath,
    HeldLocalRoot,
    StatePath,
    StateLocalRoot,
    StateRemotePrefix,
    RemotePrefix,
    Profile,
    ProfileSettingsFingerprint,
    PlanContractFingerprint,
}

#[derive(Debug)]
pub(crate) enum StrictRegisteredRootSourcesIncompleteV1 {
    InvalidSelectedRoot,
    BindingMissing,
    RouteMismatch(RegisteredSourceRouteMismatchV1),
    Local(StrictLocalSnapshotIncompleteV1),
    State(StrictPrimaryStateIncompleteV1),
    Remote(StrictBoundRemoteObservationIncompleteV1),
    NamespaceSafetyConflict(NamespaceSafetyClaimConflictV1),
    NamespaceSafetyResourceLimit,
    RemoteClaimWithoutOrigin,
    GitTopology(StrictGitRawTopologyIncompleteV1),
    Catalog(StrictSemanticallyBoundRemoteCatalogIncompleteV1),
}

pub(crate) enum StrictRegisteredRootSourcesReadV1 {
    Observed(Box<AgentStaticNamespaceSafetyObservationV1>),
    GitRawLocalTopologyObserved(Box<GitRawLocalTopologyObservationV1>),
    Incomplete(StrictRegisteredRootSourcesIncompleteV1),
}

pub(crate) enum CatalogBoundRegisteredRootSourcesReadV1 {
    AgentStatic(Box<CatalogBoundAgentStaticSourceObservationV1>),
    GitRaw(Box<CatalogBoundGitRawSourceObservationV1>),
    Incomplete(StrictRegisteredRootSourcesIncompleteV1),
}

/// Opaque proof that the daemon-owned selector authenticated one exact
/// root-registry entry and its runtime-canonical host binding.
///
/// This module intentionally provides no production constructor yet. The
/// eventual daemon integration must move or expose construction behind the
/// selector that owns authorization, overlap, ownership, ACL, and state-fence
/// checks; raw config plus path arguments are not an authority capability.
pub(crate) struct ValidatedSelectedRegisteredRootRouteV1 {
    root_id: String,
    selected: RegisteredRootV1Config,
    canonical_local_root: PathBuf,
    canonical_state_path: PathBuf,
}

/// Opaque remote identity projected from the daemon-authenticated selected
/// root route.
///
/// Catalog bytes may describe themselves, but they are never authority to
/// choose which registered root the daemon selected. Production construction
/// therefore remains private to this module and requires the selector-owned
/// route capability above.
pub(crate) struct ValidatedSelectedRegisteredRootRemoteContextV1 {
    root_id: String,
    spec: RootSpecV1Config,
    spec_identity_fingerprint: String,
    profile_settings_fingerprint: RootProfileSettingsFingerprintV1,
    plan_contract_fingerprint: RegisteredRootPlanContractFingerprintV1,
}

impl ValidatedSelectedRegisteredRootRemoteContextV1 {
    fn from_selected_route(selected_route: &ValidatedSelectedRegisteredRootRouteV1) -> Self {
        let root_id = selected_route.root_id.clone();
        let spec = selected_route.selected.spec.clone();
        Self {
            spec_identity_fingerprint: spec.identity_fingerprint(&root_id),
            profile_settings_fingerprint: spec.profile.policy().settings_fingerprint(),
            plan_contract_fingerprint: RegisteredRootPlanContractV1::strict_v1().fingerprint(),
            root_id,
            spec,
        }
    }

    pub(crate) fn root_id(&self) -> &str {
        &self.root_id
    }

    pub(crate) const fn spec(&self) -> &RootSpecV1Config {
        &self.spec
    }

    pub(crate) fn spec_identity_fingerprint(&self) -> &str {
        &self.spec_identity_fingerprint
    }

    pub(crate) const fn profile_settings_fingerprint(&self) -> RootProfileSettingsFingerprintV1 {
        self.profile_settings_fingerprint
    }

    pub(crate) const fn plan_contract_fingerprint(
        &self,
    ) -> RegisteredRootPlanContractFingerprintV1 {
        self.plan_contract_fingerprint
    }
}

impl ValidatedSelectedRegisteredRootRouteV1 {
    fn remote_context(&self) -> ValidatedSelectedRegisteredRootRemoteContextV1 {
        ValidatedSelectedRegisteredRootRemoteContextV1::from_selected_route(self)
    }
}

#[cfg(test)]
pub(crate) fn validated_selected_registered_root_remote_context_for_test_v1(
    root_id: &str,
    spec: &RootSpecV1Config,
) -> std::result::Result<
    ValidatedSelectedRegisteredRootRemoteContextV1,
    StrictRegisteredRootSourcesIncompleteV1,
> {
    let selected = RegisteredRootV1Config {
        spec: spec.clone(),
        binding: None,
    };
    if selected.validate_shape(root_id).is_err() {
        return Err(StrictRegisteredRootSourcesIncompleteV1::InvalidSelectedRoot);
    }
    Ok(ValidatedSelectedRegisteredRootRemoteContextV1 {
        root_id: root_id.to_owned(),
        spec: spec.clone(),
        spec_identity_fingerprint: spec.identity_fingerprint(root_id),
        profile_settings_fingerprint: spec.profile.policy().settings_fingerprint(),
        plan_contract_fingerprint: RegisteredRootPlanContractV1::strict_v1().fingerprint(),
    })
}

/// Opaque, non-authoritative observation of one AgentStatic selected root.
///
/// The retained inputs prove their own acquisition properties and keep the
/// local root descriptor alive. They do not prove a transactionally complete
/// remote universe. This type must not gain a plan digest or action projection.
pub(crate) struct AgentStaticNamespaceSafetyObservationV1 {
    root_id: String,
    spec: RootSpecV1Config,
    binding: RootBindingV1Config,
    spec_identity_fingerprint: String,
    binding_identity_fingerprint: String,
    profile_policy: RootProfilePolicyV1,
    plan_contract: RegisteredRootPlanContractV1,
    canonical_state_path: PathBuf,
    local: RevalidatedStrictLocalSnapshotV1,
    state: StrictPrimaryStateSnapshotV1,
    remote: MatchingTwoPassBoundRemoteEvidenceV1,
    namespace_safety_summary: NamespaceSafetySummaryV1,
}

impl AgentStaticNamespaceSafetyObservationV1 {
    pub(crate) fn root_id(&self) -> &str {
        &self.root_id
    }

    pub(crate) fn canonical_local_root(&self) -> &Path {
        self.local.canonical_local_root()
    }

    pub(crate) fn canonical_state_path(&self) -> &Path {
        &self.canonical_state_path
    }

    pub(crate) fn remote_prefix(&self) -> &str {
        &self.spec.remote_prefix
    }

    pub(crate) const fn namespace_safety_claim_count(&self) -> u64 {
        self.namespace_safety_summary.unique_claims
    }

    #[cfg(test)]
    const fn namespace_safety_source_claim_count(
        &self,
        source: NamespaceSafetyClaimSourceV1,
    ) -> u64 {
        self.namespace_safety_summary.source_claim_count(source)
    }
}

impl fmt::Debug for AgentStaticNamespaceSafetyObservationV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentStaticNamespaceSafetyObservationV1")
            .field("root_id", &self.root_id)
            .field("spec", &self.spec)
            .field("binding", &self.binding)
            .field("spec_identity_fingerprint", &self.spec_identity_fingerprint)
            .field(
                "binding_identity_fingerprint",
                &self.binding_identity_fingerprint,
            )
            .field("profile_policy", &self.profile_policy)
            .field("plan_contract", &self.plan_contract)
            .field("canonical_local_root", &self.local.canonical_local_root())
            .field("canonical_state_path", &self.canonical_state_path)
            .field("state_entry_count", &self.state.entries().len())
            .field("remote_prefix", &self.remote.remote_prefix())
            .field(
                "namespace_safety_claim_count",
                &self.namespace_safety_summary.unique_claims,
            )
            .finish_non_exhaustive()
    }
}

/// Opaque, non-authoritative observation of one GitRaw selected root.
///
/// The local Git config, HEAD, and sorted refs were captured through the held
/// root into process-owned bytes and derived identically twice without
/// reopening live config, HEAD, or ref contents. Inventory C and the held root
/// and `.git` identities are still revalidated. Object semantics, remote Git
/// semantics, catalog authority, bootstrap, high-water, and an execution-time
/// writer fence are absent. This type must not gain a plan digest or action
/// projection.
pub(crate) struct GitRawLocalTopologyObservationV1 {
    root_id: String,
    spec: RootSpecV1Config,
    binding: RootBindingV1Config,
    spec_identity_fingerprint: String,
    binding_identity_fingerprint: String,
    profile_policy: RootProfilePolicyV1,
    plan_contract: RegisteredRootPlanContractV1,
    canonical_state_path: PathBuf,
    local: RevalidatedStrictLocalSnapshotV1,
    state: StrictPrimaryStateSnapshotV1,
    remote: MatchingTwoPassBoundRemoteEvidenceV1,
    git_topology: HeldStrictGitRawTopologyV1,
    namespace_safety_summary: NamespaceSafetySummaryV1,
}

impl GitRawLocalTopologyObservationV1 {
    pub(crate) fn root_id(&self) -> &str {
        &self.root_id
    }

    pub(crate) fn canonical_local_root(&self) -> &Path {
        self.local.canonical_local_root()
    }

    pub(crate) fn canonical_state_path(&self) -> &Path {
        &self.canonical_state_path
    }

    pub(crate) fn remote_prefix(&self) -> &str {
        &self.spec.remote_prefix
    }

    pub(crate) const fn namespace_safety_claim_count(&self) -> u64 {
        self.namespace_safety_summary.unique_claims
    }

    pub(crate) fn git_topology(&self) -> &HeldStrictGitRawTopologyV1 {
        &self.git_topology
    }
}

impl fmt::Debug for GitRawLocalTopologyObservationV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitRawLocalTopologyObservationV1")
            .field("root_id", &self.root_id)
            .field("spec", &self.spec)
            .field("binding", &self.binding)
            .field("spec_identity_fingerprint", &self.spec_identity_fingerprint)
            .field(
                "binding_identity_fingerprint",
                &self.binding_identity_fingerprint,
            )
            .field("profile_policy", &self.profile_policy)
            .field("plan_contract", &self.plan_contract)
            .field("canonical_local_root", &self.local.canonical_local_root())
            .field("canonical_state_path", &self.canonical_state_path)
            .field("state_entry_count", &self.state.entries().len())
            .field("remote_prefix", &self.remote.remote_prefix())
            .field("git_topology", &self.git_topology)
            .field(
                "namespace_safety_claim_count",
                &self.namespace_safety_summary.unique_claims,
            )
            .finish_non_exhaustive()
    }
}

/// Common held inputs for one catalog-bound selected-root observation.
///
/// The catalog corpus proves the internal closure of one observed immutable
/// revision only. It does not prove that every writer used the catalog, that
/// sequence one was bootstrapped from complete truth, or that the selected
/// HEAD is the latest non-replayed revision.
struct CatalogBoundRegisteredRootSourceBaseV1 {
    root_id: String,
    spec: RootSpecV1Config,
    binding: RootBindingV1Config,
    spec_identity_fingerprint: String,
    binding_identity_fingerprint: String,
    profile_policy: RootProfilePolicyV1,
    plan_contract: RegisteredRootPlanContractV1,
    canonical_state_path: PathBuf,
    local: RevalidatedStrictLocalSnapshotV1,
    state: StrictPrimaryStateSnapshotV1,
    remote: SemanticallyBoundRemoteCatalogCorpusV1,
    namespace_safety_summary: NamespaceSafetySummaryV1,
}

impl CatalogBoundRegisteredRootSourceBaseV1 {
    fn root_id(&self) -> &str {
        &self.root_id
    }

    fn canonical_local_root(&self) -> &Path {
        self.local.canonical_local_root()
    }

    fn canonical_state_path(&self) -> &Path {
        &self.canonical_state_path
    }

    fn remote_prefix(&self) -> &str {
        self.remote.remote_prefix()
    }

    const fn namespace_safety_claim_count(&self) -> u64 {
        self.namespace_safety_summary.unique_claims
    }

    #[cfg(test)]
    const fn namespace_safety_source_claim_count(
        &self,
        source: NamespaceSafetyClaimSourceV1,
    ) -> u64 {
        self.namespace_safety_summary.source_claim_count(source)
    }

    #[cfg(test)]
    const fn catalog_sequence(&self) -> u64 {
        self.remote.catalog_sequence().get()
    }
}

impl fmt::Debug for CatalogBoundRegisteredRootSourceBaseV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogBoundRegisteredRootSourceBaseV1")
            .field("root_id", &self.root_id)
            .field("spec", &self.spec)
            .field("binding", &self.binding)
            .field("spec_identity_fingerprint", &self.spec_identity_fingerprint)
            .field(
                "binding_identity_fingerprint",
                &self.binding_identity_fingerprint,
            )
            .field("profile_policy", &self.profile_policy)
            .field("plan_contract", &self.plan_contract)
            .field("canonical_local_root", &self.local.canonical_local_root())
            .field("canonical_state_path", &self.canonical_state_path)
            .field("state_entry_count", &self.state.entries().len())
            .field("remote", &self.remote)
            .field(
                "namespace_safety_claim_count",
                &self.namespace_safety_summary.unique_claims,
            )
            .finish_non_exhaustive()
    }
}

/// Digestless held-source observation for an AgentStatic root and one
/// internally closed catalog revision.
///
/// This remains non-authoritative until writer fencing, trusted bootstrap, and
/// monotonic currentness are independently established. It must not gain a
/// plan digest or action projection.
pub(crate) struct CatalogBoundAgentStaticSourceObservationV1 {
    base: CatalogBoundRegisteredRootSourceBaseV1,
}

impl CatalogBoundAgentStaticSourceObservationV1 {
    pub(crate) fn root_id(&self) -> &str {
        self.base.root_id()
    }

    pub(crate) fn canonical_local_root(&self) -> &Path {
        self.base.canonical_local_root()
    }

    pub(crate) fn canonical_state_path(&self) -> &Path {
        self.base.canonical_state_path()
    }

    pub(crate) fn remote_prefix(&self) -> &str {
        self.base.remote_prefix()
    }

    pub(crate) const fn namespace_safety_claim_count(&self) -> u64 {
        self.base.namespace_safety_claim_count()
    }

    #[cfg(test)]
    const fn namespace_safety_source_claim_count(
        &self,
        source: NamespaceSafetyClaimSourceV1,
    ) -> u64 {
        self.base.namespace_safety_source_claim_count(source)
    }

    #[cfg(test)]
    const fn catalog_sequence(&self) -> u64 {
        self.base.catalog_sequence()
    }
}

impl fmt::Debug for CatalogBoundAgentStaticSourceObservationV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogBoundAgentStaticSourceObservationV1")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// Digestless held-source observation for a GitRaw root and one internally
/// closed catalog revision.
///
/// The retained Git topology is the checkpoint-14 immutable raw-metadata
/// shadow. It does not prove object existence/kind, ancestry, fast-forward, or
/// a continuous native-Git writer fence. This type must not gain a plan digest
/// or action projection.
pub(crate) struct CatalogBoundGitRawSourceObservationV1 {
    base: CatalogBoundRegisteredRootSourceBaseV1,
    git_topology: HeldStrictGitRawTopologyV1,
}

impl CatalogBoundGitRawSourceObservationV1 {
    pub(crate) fn root_id(&self) -> &str {
        self.base.root_id()
    }

    pub(crate) fn canonical_local_root(&self) -> &Path {
        self.base.canonical_local_root()
    }

    pub(crate) fn canonical_state_path(&self) -> &Path {
        self.base.canonical_state_path()
    }

    pub(crate) fn remote_prefix(&self) -> &str {
        self.base.remote_prefix()
    }

    pub(crate) const fn namespace_safety_claim_count(&self) -> u64 {
        self.base.namespace_safety_claim_count()
    }

    pub(crate) fn git_topology(&self) -> &HeldStrictGitRawTopologyV1 {
        &self.git_topology
    }

    #[cfg(test)]
    const fn catalog_sequence(&self) -> u64 {
        self.base.catalog_sequence()
    }
}

impl fmt::Debug for CatalogBoundGitRawSourceObservationV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogBoundGitRawSourceObservationV1")
            .field("base", &self.base)
            .field("git_topology", &self.git_topology)
            .finish_non_exhaustive()
    }
}

struct ExpectedRegisteredSourceRouteV1<'a> {
    canonical_local_root: &'a Path,
    canonical_state_path: &'a Path,
    remote_prefix: &'a str,
    profile: RootProfileV1,
    profile_settings_fingerprint: RootProfileSettingsFingerprintV1,
    plan_contract_fingerprint: RegisteredRootPlanContractFingerprintV1,
}

struct ObservedRegisteredSourceRouteV1<'a> {
    held_local_root: &'a Path,
    state_path: &'a Path,
    state_local_root: &'a Path,
    state_remote_prefix: &'a str,
    remote_prefix: &'a str,
    profile: RootProfileV1,
    profile_settings_fingerprint: RootProfileSettingsFingerprintV1,
    plan_contract_fingerprint: RegisteredRootPlanContractFingerprintV1,
}

fn validate_observed_source_route_v1(
    expected: &ExpectedRegisteredSourceRouteV1<'_>,
    observed: &ObservedRegisteredSourceRouteV1<'_>,
) -> std::result::Result<(), RegisteredSourceRouteMismatchV1> {
    let checks = [
        (
            observed.held_local_root == expected.canonical_local_root,
            RegisteredSourceRouteMismatchV1::HeldLocalRoot,
        ),
        (
            observed.state_path == expected.canonical_state_path,
            RegisteredSourceRouteMismatchV1::StatePath,
        ),
        (
            observed.state_local_root == expected.canonical_local_root,
            RegisteredSourceRouteMismatchV1::StateLocalRoot,
        ),
        (
            observed.state_remote_prefix == expected.remote_prefix,
            RegisteredSourceRouteMismatchV1::StateRemotePrefix,
        ),
        (
            observed.remote_prefix == expected.remote_prefix,
            RegisteredSourceRouteMismatchV1::RemotePrefix,
        ),
        (
            observed.profile == expected.profile,
            RegisteredSourceRouteMismatchV1::Profile,
        ),
        (
            observed.profile_settings_fingerprint == expected.profile_settings_fingerprint,
            RegisteredSourceRouteMismatchV1::ProfileSettingsFingerprint,
        ),
        (
            observed.plan_contract_fingerprint == expected.plan_contract_fingerprint,
            RegisteredSourceRouteMismatchV1::PlanContractFingerprint,
        ),
    ];
    checks
        .into_iter()
        .find_map(|(matches, mismatch)| (!matches).then_some(mismatch))
        .map_or(Ok(()), Err)
}

fn compare_namespace_safety_claim_v1<'a>(
    first: &mut Option<(&'a str, PortableNamespaceRole, NamespaceSafetyClaimSourceV1)>,
    source: NamespaceSafetyClaimSourceV1,
    exact_path: &'a str,
    role: PortableNamespaceRole,
) -> std::result::Result<(), NamespaceSafetyClaimConflictV1> {
    if let Some((first_exact_path, first_role, first_source)) = *first {
        let kind = if first_exact_path != exact_path {
            Some(NamespaceSafetyClaimConflictKindV1::FoldedSpellingAlias)
        } else if first_role != role {
            Some(NamespaceSafetyClaimConflictKindV1::FileDirectoryRole)
        } else {
            None
        };
        if let Some(kind) = kind {
            return Err(NamespaceSafetyClaimConflictV1 {
                kind,
                first_source,
                conflicting_source: source,
            });
        }
        return Ok(());
    }
    *first = Some((exact_path, role, source));
    Ok(())
}

fn compare_remote_namespace_safety_claim_v1<'a>(
    first: &mut Option<(&'a str, PortableNamespaceRole, NamespaceSafetyClaimSourceV1)>,
    exact_path: &'a str,
    role: PortableNamespaceRole,
    origins: RemoteNamespaceClaimOriginsV1,
) -> std::result::Result<(), NamespaceSafetyClaimConflictV1> {
    for (present, source) in [
        (
            origins.current(),
            NamespaceSafetyClaimSourceV1::RemoteCurrent,
        ),
        (
            origins.historical(),
            NamespaceSafetyClaimSourceV1::RemoteHistorical,
        ),
        (
            origins.reservation(),
            NamespaceSafetyClaimSourceV1::RemoteReservation,
        ),
    ] {
        if present {
            compare_namespace_safety_claim_v1(first, source, exact_path, role)?;
        }
    }
    Ok(())
}

enum ComposeNamespaceSafetyErrorV1 {
    Conflict(NamespaceSafetyClaimConflictV1),
    ResourceLimit,
    RemoteClaimWithoutOrigin,
}

impl From<NamespaceSafetyClaimConflictV1> for ComposeNamespaceSafetyErrorV1 {
    fn from(conflict: NamespaceSafetyClaimConflictV1) -> Self {
        Self::Conflict(conflict)
    }
}

fn checked_summary_increment_v1(
    value: &mut u64,
    increment: u64,
) -> std::result::Result<(), ComposeNamespaceSafetyErrorV1> {
    *value = value
        .checked_add(increment)
        .ok_or(ComposeNamespaceSafetyErrorV1::ResourceLimit)?;
    Ok(())
}

fn compose_namespace_safety_summary_from_remote_claims_v1<'a>(
    local: &RevalidatedStrictLocalSnapshotV1,
    state: &StrictPrimaryStateSnapshotV1,
    mut remote_claims: impl Iterator<
        Item = (
            &'a str,
            &'a str,
            PortableNamespaceRole,
            RemoteNamespaceClaimOriginsV1,
        ),
    >,
) -> std::result::Result<NamespaceSafetySummaryV1, ComposeNamespaceSafetyErrorV1> {
    let mut local_claims = local.snapshot().namespace_claims();
    let mut state_claims = state.namespace_claims();
    let mut local_current = local_claims.next();
    let mut state_current = state_claims.next();
    let mut remote_current = remote_claims.next();
    let mut summary = NamespaceSafetySummaryV1::default();

    while local_current.is_some() || state_current.is_some() || remote_current.is_some() {
        let folded_path = [
            local_current.as_ref().map(|(path, _)| *path),
            state_current.as_ref().map(|(path, _)| *path),
            remote_current.as_ref().map(|(path, _, _, _)| *path),
        ]
        .into_iter()
        .flatten()
        .min()
        .expect("at least one ordered claim cursor is populated");
        let take_local = local_current
            .as_ref()
            .is_some_and(|(path, _)| *path == folded_path);
        let take_state = state_current
            .as_ref()
            .is_some_and(|(path, _)| *path == folded_path);
        let take_remote = remote_current
            .as_ref()
            .is_some_and(|(path, _, _, _)| *path == folded_path);
        let mut first = None;

        if take_local {
            let (_, claim) = local_current
                .as_ref()
                .expect("matching local cursor remains populated");
            let role = match claim.role() {
                StrictLocalNamespaceRoleV1::File => PortableNamespaceRole::File,
                StrictLocalNamespaceRoleV1::Directory => PortableNamespaceRole::Directory,
            };
            compare_namespace_safety_claim_v1(
                &mut first,
                NamespaceSafetyClaimSourceV1::LocalCurrent,
                claim.exact_path(),
                role,
            )?;
            checked_summary_increment_v1(&mut summary.local_current_claims, 1)?;
        }
        if take_state {
            let (_, claim) = state_current
                .as_ref()
                .expect("matching state cursor remains populated");
            compare_namespace_safety_claim_v1(
                &mut first,
                NamespaceSafetyClaimSourceV1::StateBaseline,
                claim.exact_path(),
                claim.role(),
            )?;
            checked_summary_increment_v1(&mut summary.state_baseline_claims, 1)?;
        }
        if take_remote {
            let (_, exact_path, role, origins) = remote_current
                .as_ref()
                .expect("matching remote cursor remains populated");
            if origins.is_empty() {
                return Err(ComposeNamespaceSafetyErrorV1::RemoteClaimWithoutOrigin);
            }
            compare_remote_namespace_safety_claim_v1(&mut first, exact_path, *role, *origins)?;
            checked_summary_increment_v1(
                &mut summary.remote_current_claims,
                u64::from(origins.current()),
            )?;
            checked_summary_increment_v1(
                &mut summary.remote_historical_claims,
                u64::from(origins.historical()),
            )?;
            checked_summary_increment_v1(
                &mut summary.remote_reservation_claims,
                u64::from(origins.reservation()),
            )?;
        }

        let (exact_path, _, _) =
            first.expect("every retained claim has at least one authenticated source");
        let unique_claim_bytes = u64::try_from(folded_path.len())
            .ok()
            .and_then(|folded_bytes| {
                u64::try_from(exact_path.len())
                    .ok()
                    .and_then(|exact_bytes| folded_bytes.checked_add(exact_bytes))
            })
            .ok_or(ComposeNamespaceSafetyErrorV1::ResourceLimit)?;
        checked_summary_increment_v1(&mut summary.unique_claims, 1)?;
        checked_summary_increment_v1(&mut summary.unique_claim_bytes, unique_claim_bytes)?;
        if take_local {
            local_current = local_claims.next();
        }
        if take_state {
            state_current = state_claims.next();
        }
        if take_remote {
            remote_current = remote_claims.next();
        }
    }
    Ok(summary)
}

fn compose_namespace_safety_summary_v1(
    local: &RevalidatedStrictLocalSnapshotV1,
    state: &StrictPrimaryStateSnapshotV1,
    remote: &MatchingTwoPassBoundRemoteEvidenceV1,
) -> std::result::Result<NamespaceSafetySummaryV1, ComposeNamespaceSafetyErrorV1> {
    compose_namespace_safety_summary_from_remote_claims_v1(
        local,
        state,
        remote.namespace_safety_claims(),
    )
}

fn compose_catalog_namespace_safety_summary_v1(
    local: &RevalidatedStrictLocalSnapshotV1,
    state: &StrictPrimaryStateSnapshotV1,
    remote: &SemanticallyBoundRemoteCatalogCorpusV1,
) -> std::result::Result<NamespaceSafetySummaryV1, ComposeNamespaceSafetyErrorV1> {
    compose_namespace_safety_summary_from_remote_claims_v1(
        local,
        state,
        remote.namespace_safety_claims(),
    )
}

#[cfg(test)]
fn canonicalize_test_route_path_v1(path: &Path, allow_missing_leaf: bool) -> Option<PathBuf> {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return Some(canonical);
    }
    if !allow_missing_leaf {
        return None;
    }
    let parent = std::fs::canonicalize(path.parent()?).ok()?;
    Some(parent.join(path.file_name()?))
}

#[cfg(test)]
fn configured_path_matches_canonical_v1(
    configured: &Path,
    canonical: &Path,
    allow_missing_leaf: bool,
) -> bool {
    canonical.is_absolute()
        && canonicalize_test_route_path_v1(canonical, allow_missing_leaf).as_deref()
            == Some(canonical)
        && canonicalize_test_route_path_v1(&expand_tilde(configured), allow_missing_leaf).as_deref()
            == Some(canonical)
}

#[cfg(test)]
fn validated_selection_for_test_v1(
    root_id: &str,
    selected: &RegisteredRootV1Config,
    canonical_local_root: &Path,
    canonical_state_path: &Path,
) -> std::result::Result<
    ValidatedSelectedRegisteredRootRouteV1,
    StrictRegisteredRootSourcesIncompleteV1,
> {
    if selected.validate_shape(root_id).is_err() {
        return Err(StrictRegisteredRootSourcesIncompleteV1::InvalidSelectedRoot);
    }
    let Some(binding) = selected.binding.as_ref() else {
        return Err(StrictRegisteredRootSourcesIncompleteV1::BindingMissing);
    };
    if !configured_path_matches_canonical_v1(&binding.local_root, canonical_local_root, false) {
        return Err(StrictRegisteredRootSourcesIncompleteV1::RouteMismatch(
            RegisteredSourceRouteMismatchV1::ConfiguredLocalRoot,
        ));
    }
    if !configured_path_matches_canonical_v1(&binding.state_path, canonical_state_path, true) {
        return Err(StrictRegisteredRootSourcesIncompleteV1::RouteMismatch(
            RegisteredSourceRouteMismatchV1::ConfiguredStatePath,
        ));
    }
    Ok(ValidatedSelectedRegisteredRootRouteV1 {
        root_id: root_id.to_owned(),
        selected: selected.clone(),
        canonical_local_root: canonical_local_root.to_owned(),
        canonical_state_path: canonical_state_path.to_owned(),
    })
}

#[cfg(test)]
async fn observe_selected_registered_root_sources_for_test_v1(
    root_id: &str,
    selected: &RegisteredRootV1Config,
    canonical_local_root: &Path,
    canonical_state_path: &Path,
    op: &Operator,
) -> Result<StrictRegisteredRootSourcesReadV1> {
    let selected = match validated_selection_for_test_v1(
        root_id,
        selected,
        canonical_local_root,
        canonical_state_path,
    ) {
        Ok(selected) => selected,
        Err(incomplete) => {
            return Ok(StrictRegisteredRootSourcesReadV1::Incomplete(incomplete));
        }
    };
    observe_validated_registered_root_sources_v1(selected, op).await
}

/// Observe one daemon-validated selected-root route through one held local
/// acquisition window.
async fn observe_validated_registered_root_sources_v1(
    selected_route: ValidatedSelectedRegisteredRootRouteV1,
    op: &Operator,
) -> Result<StrictRegisteredRootSourcesReadV1> {
    let ValidatedSelectedRegisteredRootRouteV1 {
        root_id,
        selected,
        canonical_local_root,
        canonical_state_path,
    } = selected_route;
    let binding = selected
        .binding
        .as_ref()
        .expect("validated selected-root capability always carries a binding");
    let profile_policy = selected.spec.profile.policy();
    let plan_contract = RegisteredRootPlanContractV1::strict_v1();
    let plan_contract_fingerprint = plan_contract.fingerprint();
    let spec_identity_fingerprint = selected.spec.identity_fingerprint(&root_id);
    let binding_identity_fingerprint =
        match binding.binding_fingerprint(&canonical_local_root, &canonical_state_path) {
            Ok(fingerprint) => fingerprint,
            Err(_) => {
                return Ok(StrictRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::InvalidSelectedRoot,
                ));
            }
        };

    let mut pending =
        match begin_strict_local_snapshot_v1(&canonical_local_root, selected.spec.profile)? {
            StrictLocalSnapshotHoldReadV1::Pending(pending) => pending,
            StrictLocalSnapshotHoldReadV1::Incomplete(incomplete) => {
                return Ok(StrictRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::Local(incomplete),
                ));
            }
        };
    let pending_git = if selected.spec.profile == RootProfileV1::GitRawV1 {
        match begin_strict_git_raw_topology_v1(&mut pending) {
            StrictGitRawTopologyBeginV1::Pending(git) => Some(*git),
            StrictGitRawTopologyBeginV1::Incomplete(incomplete) => {
                return Ok(StrictRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::GitTopology(incomplete),
                ));
            }
        }
    } else {
        None
    };
    let state = match read_and_bind_strict_primary_state_for_pending_root_v1(
        &canonical_state_path,
        &pending,
        &selected.spec.remote_prefix,
    )? {
        StrictPrimaryStateReadV1::Complete(state) => state,
        StrictPrimaryStateReadV1::Incomplete(incomplete) => {
            return Ok(StrictRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::State(incomplete),
            ));
        }
    };
    let remote =
        match read_bound_remote_observation_two_pass_v1(op, &selected.spec.remote_prefix).await? {
            StrictBoundRemoteObservationReadV1::Matched(remote) => remote,
            StrictBoundRemoteObservationReadV1::Incomplete(incomplete) => {
                return Ok(StrictRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::Remote(incomplete),
                ));
            }
        };
    let local = match pending.revalidate_inventory_c()? {
        StrictLocalSnapshotFinishV1::Complete(local) => local,
        StrictLocalSnapshotFinishV1::Incomplete(incomplete) => {
            return Ok(StrictRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::Local(incomplete),
            ));
        }
    };
    let (git_topology, local) = match pending_git {
        Some(git) => match git.revalidate_after_external_reads(local) {
            StrictGitRawTopologyFinishV1::Held { topology, local } => (Some(topology), local),
            StrictGitRawTopologyFinishV1::Incomplete(incomplete) => {
                return Ok(StrictRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::GitTopology(incomplete),
                ));
            }
        },
        None => (None, local),
    };
    if let Some(git) = &git_topology {
        if let Err(incomplete) = git.revalidate_capabilities() {
            return Ok(StrictRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::GitTopology(incomplete),
            ));
        }
    }

    let expected_route = ExpectedRegisteredSourceRouteV1 {
        canonical_local_root: &canonical_local_root,
        canonical_state_path: &canonical_state_path,
        remote_prefix: &selected.spec.remote_prefix,
        profile: selected.spec.profile,
        profile_settings_fingerprint: profile_policy.settings_fingerprint(),
        plan_contract_fingerprint,
    };
    let observed_route = ObservedRegisteredSourceRouteV1 {
        held_local_root: local.canonical_local_root(),
        state_path: state.selected_state_path(),
        state_local_root: state.canonical_local_root(),
        state_remote_prefix: state.remote_prefix(),
        remote_prefix: remote.remote_prefix(),
        profile: local.snapshot().profile(),
        profile_settings_fingerprint: local.snapshot().profile_settings_fingerprint(),
        plan_contract_fingerprint: local.snapshot().plan_contract_fingerprint(),
    };
    if let Err(mismatch) = validate_observed_source_route_v1(&expected_route, &observed_route) {
        return Ok(StrictRegisteredRootSourcesReadV1::Incomplete(
            StrictRegisteredRootSourcesIncompleteV1::RouteMismatch(mismatch),
        ));
    }

    let namespace_safety_summary =
        match compose_namespace_safety_summary_v1(&local, &state, &remote) {
            Ok(summary) => summary,
            Err(ComposeNamespaceSafetyErrorV1::Conflict(conflict)) => {
                return Ok(StrictRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::NamespaceSafetyConflict(conflict),
                ));
            }
            Err(ComposeNamespaceSafetyErrorV1::ResourceLimit) => {
                return Ok(StrictRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::NamespaceSafetyResourceLimit,
                ));
            }
            Err(ComposeNamespaceSafetyErrorV1::RemoteClaimWithoutOrigin) => {
                return Ok(StrictRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::RemoteClaimWithoutOrigin,
                ));
            }
        };

    match selected.spec.profile {
        RootProfileV1::AgentStaticV1 => {
            debug_assert!(git_topology.is_none());
            Ok(StrictRegisteredRootSourcesReadV1::Observed(Box::new(
                AgentStaticNamespaceSafetyObservationV1 {
                    root_id,
                    spec: selected.spec.clone(),
                    binding: binding.clone(),
                    spec_identity_fingerprint,
                    binding_identity_fingerprint,
                    profile_policy,
                    plan_contract,
                    canonical_state_path,
                    local,
                    state,
                    remote,
                    namespace_safety_summary,
                },
            )))
        }
        RootProfileV1::GitRawV1 => {
            let git_topology =
                git_topology.expect("GitRaw acquisition retains exact topology evidence");
            Ok(
                StrictRegisteredRootSourcesReadV1::GitRawLocalTopologyObserved(Box::new(
                    GitRawLocalTopologyObservationV1 {
                        root_id,
                        spec: selected.spec.clone(),
                        binding: binding.clone(),
                        spec_identity_fingerprint,
                        binding_identity_fingerprint,
                        profile_policy,
                        plan_contract,
                        canonical_state_path,
                        local,
                        state,
                        remote,
                        git_topology,
                        namespace_safety_summary,
                    },
                )),
            )
        }
    }
}

#[cfg(test)]
async fn observe_catalog_bound_selected_registered_root_sources_for_test_v1(
    root_id: &str,
    selected: &RegisteredRootV1Config,
    canonical_local_root: &Path,
    canonical_state_path: &Path,
    op: &Operator,
    receipt: &ConditionalWriteSemanticsReceipt,
) -> Result<CatalogBoundRegisteredRootSourcesReadV1> {
    let selected = match validated_selection_for_test_v1(
        root_id,
        selected,
        canonical_local_root,
        canonical_state_path,
    ) {
        Ok(selected) => selected,
        Err(incomplete) => {
            return Ok(CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                incomplete,
            ));
        }
    };
    observe_catalog_bound_validated_registered_root_sources_v1(selected, op, receipt).await
}

/// Observe one daemon-validated selected-root route and one internally closed
/// immutable catalog revision through the same held local acquisition window.
///
/// Receipt acquisition is deliberately outside this function. The catalog
/// reader verifies that the non-Clone receipt belongs to this exact accessor
/// and prefix. Even a successful result remains digestless because writer
/// fencing, trusted bootstrap, and monotonic currentness are not established.
async fn observe_catalog_bound_validated_registered_root_sources_v1(
    selected_route: ValidatedSelectedRegisteredRootRouteV1,
    op: &Operator,
    receipt: &ConditionalWriteSemanticsReceipt,
) -> Result<CatalogBoundRegisteredRootSourcesReadV1> {
    let remote_context = selected_route.remote_context();
    let ValidatedSelectedRegisteredRootRouteV1 {
        root_id,
        selected,
        canonical_local_root,
        canonical_state_path,
    } = selected_route;
    let binding = selected
        .binding
        .as_ref()
        .expect("validated selected-root capability always carries a binding");
    let profile_policy = selected.spec.profile.policy();
    let plan_contract = RegisteredRootPlanContractV1::strict_v1();
    let plan_contract_fingerprint = plan_contract.fingerprint();
    let spec_identity_fingerprint = selected.spec.identity_fingerprint(&root_id);
    let binding_identity_fingerprint =
        match binding.binding_fingerprint(&canonical_local_root, &canonical_state_path) {
            Ok(fingerprint) => fingerprint,
            Err(_) => {
                return Ok(CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::InvalidSelectedRoot,
                ));
            }
        };

    let mut pending =
        match begin_strict_local_snapshot_v1(&canonical_local_root, selected.spec.profile)? {
            StrictLocalSnapshotHoldReadV1::Pending(pending) => pending,
            StrictLocalSnapshotHoldReadV1::Incomplete(incomplete) => {
                return Ok(CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::Local(incomplete),
                ));
            }
        };
    let pending_git = if selected.spec.profile == RootProfileV1::GitRawV1 {
        match begin_strict_git_raw_topology_v1(&mut pending) {
            StrictGitRawTopologyBeginV1::Pending(git) => Some(*git),
            StrictGitRawTopologyBeginV1::Incomplete(incomplete) => {
                return Ok(CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::GitTopology(incomplete),
                ));
            }
        }
    } else {
        None
    };
    let state = match read_and_bind_strict_primary_state_for_pending_root_v1(
        &canonical_state_path,
        &pending,
        &selected.spec.remote_prefix,
    )? {
        StrictPrimaryStateReadV1::Complete(state) => state,
        StrictPrimaryStateReadV1::Incomplete(incomplete) => {
            return Ok(CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::State(incomplete),
            ));
        }
    };
    let remote =
        match read_semantically_bound_remote_catalog_corpus_v1(op, &remote_context, receipt).await?
        {
            StrictSemanticallyBoundRemoteCatalogReadV1::Verified(remote) => *remote,
            StrictSemanticallyBoundRemoteCatalogReadV1::Incomplete(incomplete) => {
                return Ok(CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::Catalog(incomplete),
                ));
            }
        };
    let local = match pending.revalidate_inventory_c()? {
        StrictLocalSnapshotFinishV1::Complete(local) => local,
        StrictLocalSnapshotFinishV1::Incomplete(incomplete) => {
            return Ok(CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::Local(incomplete),
            ));
        }
    };
    let (git_topology, local) = match pending_git {
        Some(git) => match git.revalidate_after_external_reads(local) {
            StrictGitRawTopologyFinishV1::Held { topology, local } => (Some(topology), local),
            StrictGitRawTopologyFinishV1::Incomplete(incomplete) => {
                return Ok(CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::GitTopology(incomplete),
                ));
            }
        },
        None => (None, local),
    };
    if let Some(git) = &git_topology {
        if let Err(incomplete) = git.revalidate_capabilities() {
            return Ok(CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::GitTopology(incomplete),
            ));
        }
    }

    let expected_route = ExpectedRegisteredSourceRouteV1 {
        canonical_local_root: &canonical_local_root,
        canonical_state_path: &canonical_state_path,
        remote_prefix: &selected.spec.remote_prefix,
        profile: selected.spec.profile,
        profile_settings_fingerprint: profile_policy.settings_fingerprint(),
        plan_contract_fingerprint,
    };
    let observed_route = ObservedRegisteredSourceRouteV1 {
        held_local_root: local.canonical_local_root(),
        state_path: state.selected_state_path(),
        state_local_root: state.canonical_local_root(),
        state_remote_prefix: state.remote_prefix(),
        remote_prefix: remote.remote_prefix(),
        profile: local.snapshot().profile(),
        profile_settings_fingerprint: local.snapshot().profile_settings_fingerprint(),
        plan_contract_fingerprint: local.snapshot().plan_contract_fingerprint(),
    };
    if let Err(mismatch) = validate_observed_source_route_v1(&expected_route, &observed_route) {
        return Ok(CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
            StrictRegisteredRootSourcesIncompleteV1::RouteMismatch(mismatch),
        ));
    }

    let namespace_safety_summary =
        match compose_catalog_namespace_safety_summary_v1(&local, &state, &remote) {
            Ok(summary) => summary,
            Err(ComposeNamespaceSafetyErrorV1::Conflict(conflict)) => {
                return Ok(CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::NamespaceSafetyConflict(conflict),
                ));
            }
            Err(ComposeNamespaceSafetyErrorV1::ResourceLimit) => {
                return Ok(CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::NamespaceSafetyResourceLimit,
                ));
            }
            Err(ComposeNamespaceSafetyErrorV1::RemoteClaimWithoutOrigin) => {
                return Ok(CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::RemoteClaimWithoutOrigin,
                ));
            }
        };

    let base = CatalogBoundRegisteredRootSourceBaseV1 {
        root_id,
        spec: selected.spec.clone(),
        binding: binding.clone(),
        spec_identity_fingerprint,
        binding_identity_fingerprint,
        profile_policy,
        plan_contract,
        canonical_state_path,
        local,
        state,
        remote,
        namespace_safety_summary,
    };
    match selected.spec.profile {
        RootProfileV1::AgentStaticV1 => {
            debug_assert!(git_topology.is_none());
            Ok(CatalogBoundRegisteredRootSourcesReadV1::AgentStatic(
                Box::new(CatalogBoundAgentStaticSourceObservationV1 { base }),
            ))
        }
        RootProfileV1::GitRawV1 => {
            let git_topology =
                git_topology.expect("GitRaw acquisition retains exact topology evidence");
            Ok(CatalogBoundRegisteredRootSourcesReadV1::GitRaw(Box::new(
                CatalogBoundGitRawSourceObservationV1 { base, git_topology },
            )))
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::registered_local_snapshot::StrictLocalSnapshotIncompleteKindV1;
    use crate::registered_remote_catalog::tests::{
        semantic_remote_catalog_fixture_for_test_v1,
        semantic_remote_catalog_fixture_with_first_read_write_for_test_v1,
        SemanticRemoteCatalogFixtureRowV1,
    };
    use crate::registered_remote_observation::tests::{
        matching_remote_fixture_operator_v1,
        matching_remote_fixture_operator_with_first_list_write_v1,
        missing_remote_index_fixture_operator_v1, RemoteNamespaceFixtureRowV1,
    };
    use std::fs;
    use std::num::NonZeroU64;
    use std::os::unix::fs::PermissionsExt;
    use tcfs_core::config::{RegisteredRootPolicy, RootLifecyclePolicyV1};

    static_assertions::assert_not_impl_any!(
        AgentStaticNamespaceSafetyObservationV1: Clone,
        serde::Serialize,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>,
        Into<crate::registered_local_snapshot::StrictLocalSnapshotDigestV1>,
        Into<crate::registered_reconcile::StrictPrimaryStateBytesDigestV1>
    );
    static_assertions::assert_not_impl_any!(
        GitRawLocalTopologyObservationV1: Clone,
        serde::Serialize,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>,
        Into<crate::registered_local_snapshot::StrictLocalSnapshotDigestV1>,
        Into<crate::registered_reconcile::StrictPrimaryStateBytesDigestV1>
    );
    static_assertions::assert_not_impl_any!(
        CatalogBoundAgentStaticSourceObservationV1: Clone,
        serde::Serialize,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>,
        Into<crate::registered_local_snapshot::StrictLocalSnapshotDigestV1>,
        Into<crate::registered_reconcile::StrictPrimaryStateBytesDigestV1>
    );
    static_assertions::assert_not_impl_any!(
        CatalogBoundGitRawSourceObservationV1: Clone,
        serde::Serialize,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>,
        Into<crate::registered_local_snapshot::StrictLocalSnapshotDigestV1>,
        Into<crate::registered_reconcile::StrictPrimaryStateBytesDigestV1>
    );
    static_assertions::assert_not_impl_any!(
        CatalogBoundRegisteredRootSourcesReadV1: Clone,
        serde::Serialize,
        Into<crate::reconcile::ReconcilePlan>,
        Into<Vec<crate::reconcile::ReconcileAction>>,
        Into<crate::registered_local_snapshot::StrictLocalSnapshotDigestV1>,
        Into<crate::registered_reconcile::StrictPrimaryStateBytesDigestV1>
    );
    static_assertions::assert_not_impl_any!(
        SemanticallyBoundRemoteCatalogCorpusV1: Clone,
        serde::Serialize
    );
    static_assertions::assert_not_impl_any!(
        MatchingTwoPassBoundRemoteEvidenceV1: Clone,
        serde::Serialize
    );
    static_assertions::assert_not_impl_any!(
        StrictPrimaryStateSnapshotV1: Clone,
        serde::Serialize
    );

    fn write_private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn empty_state_json() -> &'static [u8] {
        br#"{"last_nats_seq":0,"device_id":"sting","entries":{}}"#
    }

    fn selected_root(
        local_root: &Path,
        state_path: &Path,
        profile: RootProfileV1,
    ) -> RegisteredRootV1Config {
        RegisteredRootV1Config {
            spec: RootSpecV1Config {
                version: RootSpecV1Config::VERSION,
                remote_prefix: "roots".to_owned(),
                profile,
                generation: NonZeroU64::new(1).unwrap(),
            },
            binding: Some(RootBindingV1Config {
                version: RootBindingV1Config::VERSION,
                local_root: local_root.to_owned(),
                state_path: state_path.to_owned(),
                lifecycle_policy: RootLifecyclePolicyV1::InspectOnly,
                resolution_policy: RegisteredRootPolicy::InspectOnly,
            }),
        }
    }

    fn state_json(local_root: &Path, rel_paths: &[&str]) -> Vec<u8> {
        let mut entries = serde_json::Map::new();
        for rel_path in rel_paths {
            let cache_key = local_root.join(rel_path).to_string_lossy().into_owned();
            entries.insert(
                cache_key,
                serde_json::json!({
                    "blake3": "a".repeat(64),
                    "size": 4,
                    "mtime": 5,
                    "chunk_count": 1,
                    "remote_path": format!("roots/manifests/{}", "b".repeat(64)),
                    "last_synced": 6,
                    "vclock": { "clocks": { "sting": 1 } },
                    "device_id": "sting",
                    "status": "synced"
                }),
            );
        }
        serde_json::to_vec(&serde_json::json!({
            "last_nats_seq": 7,
            "device_id": "sting",
            "entries": entries,
        }))
        .unwrap()
    }

    async fn observe_fixture(
        local_files: &[&str],
        state_paths: &[&str],
        remote_rows: &[RemoteNamespaceFixtureRowV1],
    ) -> StrictRegisteredRootSourcesReadV1 {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        for rel_path in local_files {
            let path = root.join(rel_path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"payload").unwrap();
        }
        let root = fs::canonicalize(root).unwrap();
        let state_path = dir.path().join("state.json");
        write_private(&state_path, &state_json(&root, state_paths));
        let state_path = fs::canonicalize(state_path).unwrap();
        let selected = selected_root(&root, &state_path, RootProfileV1::AgentStaticV1);
        observe_selected_registered_root_sources_for_test_v1(
            "work",
            &selected,
            &root,
            &state_path,
            &matching_remote_fixture_operator_v1(remote_rows),
        )
        .await
        .unwrap()
    }

    async fn observe_catalog_fixture(
        profile: RootProfileV1,
        local_files: &[&str],
        state_paths: &[&str],
        remote_rows: &[SemanticRemoteCatalogFixtureRowV1],
    ) -> CatalogBoundRegisteredRootSourcesReadV1 {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        if profile == RootProfileV1::GitRawV1 {
            initialize_isolated_git_repository(&root, dir.path());
        }
        for rel_path in local_files {
            let path = root.join(rel_path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"payload").unwrap();
        }
        let root = fs::canonicalize(root).unwrap();
        let state_path = dir.path().join("state.json");
        write_private(&state_path, &state_json(&root, state_paths));
        let state_path = fs::canonicalize(state_path).unwrap();
        let selected = selected_root(&root, &state_path, profile);
        let remote =
            semantic_remote_catalog_fixture_for_test_v1("work", &selected.spec, remote_rows).await;
        observe_catalog_bound_selected_registered_root_sources_for_test_v1(
            "work",
            &selected,
            &root,
            &state_path,
            remote.operator(),
            remote.receipt(),
        )
        .await
        .unwrap()
    }

    fn initialize_isolated_git_repository(root: &Path, sandbox: &Path) {
        let home = sandbox.join("git-home");
        let xdg_config_home = sandbox.join("git-xdg-config");
        let template_dir = sandbox.join("git-empty-template");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&xdg_config_home).unwrap();
        fs::create_dir(&template_dir).unwrap();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["init", "--quiet", "--initial-branch=main"])
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", xdg_config_home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TEMPLATE_DIR", template_dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn observed(
        read: StrictRegisteredRootSourcesReadV1,
    ) -> Box<AgentStaticNamespaceSafetyObservationV1> {
        match read {
            StrictRegisteredRootSourcesReadV1::Observed(observed) => observed,
            StrictRegisteredRootSourcesReadV1::GitRawLocalTopologyObserved(observed) => {
                panic!("expected AgentStatic sources, got GitRaw sources: {observed:?}")
            }
            StrictRegisteredRootSourcesReadV1::Incomplete(incomplete) => {
                panic!("expected observed sources, got {incomplete:?}")
            }
        }
    }

    fn conflict(read: StrictRegisteredRootSourcesReadV1) -> NamespaceSafetyClaimConflictV1 {
        match read {
            StrictRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::NamespaceSafetyConflict(conflict),
            ) => conflict,
            StrictRegisteredRootSourcesReadV1::Observed(_) => {
                panic!("expected a namespace-safety conflict, got observed sources")
            }
            StrictRegisteredRootSourcesReadV1::GitRawLocalTopologyObserved(_) => {
                panic!("expected a namespace-safety conflict, got GitRaw sources")
            }
            StrictRegisteredRootSourcesReadV1::Incomplete(other) => {
                panic!("expected a namespace-safety conflict, got {other:?}")
            }
        }
    }

    fn catalog_agent_observed(
        read: CatalogBoundRegisteredRootSourcesReadV1,
    ) -> Box<CatalogBoundAgentStaticSourceObservationV1> {
        match read {
            CatalogBoundRegisteredRootSourcesReadV1::AgentStatic(observed) => observed,
            CatalogBoundRegisteredRootSourcesReadV1::GitRaw(observed) => {
                panic!("expected catalog-bound AgentStatic sources, got GitRaw: {observed:?}")
            }
            CatalogBoundRegisteredRootSourcesReadV1::Incomplete(incomplete) => {
                panic!("expected catalog-bound sources, got {incomplete:?}")
            }
        }
    }

    fn catalog_conflict(
        read: CatalogBoundRegisteredRootSourcesReadV1,
    ) -> NamespaceSafetyClaimConflictV1 {
        match read {
            CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::NamespaceSafetyConflict(conflict),
            ) => conflict,
            CatalogBoundRegisteredRootSourcesReadV1::AgentStatic(_) => {
                panic!("expected a catalog-bound namespace conflict, got AgentStatic sources")
            }
            CatalogBoundRegisteredRootSourcesReadV1::GitRaw(_) => {
                panic!("expected a catalog-bound namespace conflict, got GitRaw sources")
            }
            CatalogBoundRegisteredRootSourcesReadV1::Incomplete(other) => {
                panic!("expected a catalog-bound namespace conflict, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn agent_static_exact_route_observes_empty_state_and_remote_without_plan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let state_path = dir.path().join("state.json");
        write_private(&state_path, empty_state_json());
        let state_path = fs::canonicalize(state_path).unwrap();
        let selected = selected_root(&root, &state_path, RootProfileV1::AgentStaticV1);

        let observed = match observe_selected_registered_root_sources_for_test_v1(
            "work",
            &selected,
            &root,
            &state_path,
            &matching_remote_fixture_operator_v1(&[]),
        )
        .await
        .unwrap()
        {
            StrictRegisteredRootSourcesReadV1::Observed(observed) => observed,
            StrictRegisteredRootSourcesReadV1::GitRawLocalTopologyObserved(observed) => {
                panic!("expected AgentStatic sources, got GitRaw sources: {observed:?}")
            }
            StrictRegisteredRootSourcesReadV1::Incomplete(incomplete) => {
                panic!("expected observed sources, got {incomplete:?}")
            }
        };
        assert_eq!(observed.root_id(), "work");
        assert_eq!(observed.canonical_local_root(), root);
        assert_eq!(observed.canonical_state_path(), state_path);
        assert_eq!(observed.remote_prefix(), "roots");
        assert_eq!(observed.namespace_safety_claim_count(), 0);
        assert_eq!(
            observed
                .namespace_safety_source_claim_count(NamespaceSafetyClaimSourceV1::LocalCurrent),
            0
        );
    }

    #[tokio::test]
    async fn catalog_bound_agent_static_retains_one_closed_empty_revision_without_plan() {
        let observed = catalog_agent_observed(
            observe_catalog_fixture(RootProfileV1::AgentStaticV1, &[], &[], &[]).await,
        );
        assert_eq!(observed.root_id(), "work");
        assert_eq!(observed.remote_prefix(), "roots");
        assert_eq!(observed.catalog_sequence(), 1);
        assert_eq!(observed.namespace_safety_claim_count(), 0);
        assert_eq!(
            observed
                .namespace_safety_source_claim_count(NamespaceSafetyClaimSourceV1::RemoteCurrent),
            0
        );
    }

    #[tokio::test]
    async fn catalog_bound_claims_join_local_state_and_remote_in_one_lattice() {
        let observed = catalog_agent_observed(
            observe_catalog_fixture(
                RootProfileV1::AgentStaticV1,
                &["same"],
                &["same"],
                &[SemanticRemoteCatalogFixtureRowV1::CurrentFile(
                    "same".to_owned(),
                )],
            )
            .await,
        );
        assert_eq!(observed.namespace_safety_claim_count(), 1);
        for source in [
            NamespaceSafetyClaimSourceV1::LocalCurrent,
            NamespaceSafetyClaimSourceV1::StateBaseline,
            NamespaceSafetyClaimSourceV1::RemoteCurrent,
        ] {
            assert_eq!(observed.namespace_safety_source_claim_count(source), 1);
        }
    }

    #[tokio::test]
    async fn catalog_bound_cross_source_alias_fails_closed() {
        let conflict = catalog_conflict(
            observe_catalog_fixture(
                RootProfileV1::AgentStaticV1,
                &["Readme"],
                &[],
                &[SemanticRemoteCatalogFixtureRowV1::CurrentFile(
                    "README".to_owned(),
                )],
            )
            .await,
        );
        assert_eq!(
            conflict.kind(),
            NamespaceSafetyClaimConflictKindV1::FoldedSpellingAlias
        );
        assert_eq!(
            conflict.first_source(),
            NamespaceSafetyClaimSourceV1::LocalCurrent
        );
        assert_eq!(
            conflict.conflicting_source(),
            NamespaceSafetyClaimSourceV1::RemoteCurrent
        );
    }

    #[tokio::test]
    async fn catalog_reads_remain_inside_local_inventory_a_c_window() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("guard"), b"before").unwrap();
        let root = fs::canonicalize(root).unwrap();
        let state_path = dir.path().join("state.json");
        write_private(&state_path, empty_state_json());
        let state_path = fs::canonicalize(state_path).unwrap();
        let selected = selected_root(&root, &state_path, RootProfileV1::AgentStaticV1);
        let remote = semantic_remote_catalog_fixture_with_first_read_write_for_test_v1(
            "work",
            &selected.spec,
            &[],
            root.join("guard"),
            b"after".to_vec(),
        )
        .await;

        match observe_catalog_bound_selected_registered_root_sources_for_test_v1(
            "work",
            &selected,
            &root,
            &state_path,
            remote.operator(),
            remote.receipt(),
        )
        .await
        .unwrap()
        {
            CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::Local(incomplete),
            ) if incomplete.kind() == StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead => {}
            other => {
                let _ = other;
                panic!("catalog-window local mutation must fail at inventory C");
            }
        }
    }

    #[tokio::test]
    async fn catalog_bound_git_raw_retains_the_exact_acquisition_shadow() {
        let observed = match observe_catalog_fixture(RootProfileV1::GitRawV1, &[], &[], &[]).await {
            CatalogBoundRegisteredRootSourcesReadV1::GitRaw(observed) => observed,
            CatalogBoundRegisteredRootSourcesReadV1::AgentStatic(_) => {
                panic!("expected catalog-bound GitRaw sources")
            }
            CatalogBoundRegisteredRootSourcesReadV1::Incomplete(incomplete) => {
                panic!("expected catalog-bound GitRaw sources, got {incomplete:?}")
            }
        };
        assert_eq!(observed.root_id(), "work");
        assert_eq!(observed.remote_prefix(), "roots");
        assert_eq!(observed.catalog_sequence(), 1);
        assert_eq!(
            observed.git_topology().head().symbolic_target(),
            Some("refs/heads/main")
        );
    }

    #[tokio::test]
    async fn catalog_bound_git_raw_rejects_head_change_inside_catalog_window() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        initialize_isolated_git_repository(&root, dir.path());
        let root = fs::canonicalize(root).unwrap();
        let state_path = dir.path().join("state.json");
        write_private(&state_path, empty_state_json());
        let state_path = fs::canonicalize(state_path).unwrap();
        let selected = selected_root(&root, &state_path, RootProfileV1::GitRawV1);
        let remote = semantic_remote_catalog_fixture_with_first_read_write_for_test_v1(
            "work",
            &selected.spec,
            &[],
            root.join(".git/HEAD"),
            b"ref: refs/heads/other\n".to_vec(),
        )
        .await;

        match observe_catalog_bound_selected_registered_root_sources_for_test_v1(
            "work",
            &selected,
            &root,
            &state_path,
            remote.operator(),
            remote.receipt(),
        )
        .await
        .unwrap()
        {
            CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::Local(incomplete),
            ) if incomplete.kind() == StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead => {}
            CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::GitTopology(incomplete),
            ) if incomplete.kind()
                == crate::registered_git_topology::StrictGitRawTopologyIncompleteKindV1::ReferenceSetChanged => {
            }
            other => {
                let _ = other;
                panic!("Git HEAD mutation during the catalog window must fail closed");
            }
        }
    }

    #[tokio::test]
    async fn catalog_bound_composition_rejects_a_receipt_for_another_accessor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let state_path = dir.path().join("state.json");
        write_private(&state_path, empty_state_json());
        let state_path = fs::canonicalize(state_path).unwrap();
        let selected = selected_root(&root, &state_path, RootProfileV1::AgentStaticV1);
        let remote = semantic_remote_catalog_fixture_for_test_v1("work", &selected.spec, &[]).await;
        let other = semantic_remote_catalog_fixture_for_test_v1("work", &selected.spec, &[]).await;

        assert!(matches!(
            observe_catalog_bound_selected_registered_root_sources_for_test_v1(
                "work",
                &selected,
                &root,
                &state_path,
                remote.operator(),
                other.receipt(),
            )
            .await
            .unwrap(),
            CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::Catalog(
                    StrictSemanticallyBoundRemoteCatalogIncompleteV1::Catalog(
                        crate::registered_remote_catalog::StrictRemoteCatalogIncompleteV1::StorageSemanticsUnverified
                    )
                )
            )
        ));
    }

    #[tokio::test]
    async fn catalog_bound_composition_rejects_catalog_route_context_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let state_path = dir.path().join("state.json");
        write_private(&state_path, empty_state_json());
        let state_path = fs::canonicalize(state_path).unwrap();
        let selected = selected_root(&root, &state_path, RootProfileV1::AgentStaticV1);
        let mut other_spec = selected.spec.clone();
        other_spec.generation = NonZeroU64::new(2).unwrap();
        let remote = semantic_remote_catalog_fixture_for_test_v1("work", &other_spec, &[]).await;

        assert!(matches!(
            observe_catalog_bound_selected_registered_root_sources_for_test_v1(
                "work",
                &selected,
                &root,
                &state_path,
                remote.operator(),
                remote.receipt(),
            )
            .await
            .unwrap(),
            CatalogBoundRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::Catalog(
                    StrictSemanticallyBoundRemoteCatalogIncompleteV1::Catalog(
                        crate::registered_remote_catalog::StrictRemoteCatalogIncompleteV1::Invalid {
                            kind: crate::registered_remote_catalog::RemoteCatalogClosureObjectKindV1::Head,
                            reason: crate::registered_remote_catalog::InvalidRemoteCatalogReasonV1::Context,
                        }
                    )
                )
            )
        ));
    }

    #[tokio::test]
    async fn git_raw_retains_held_local_topology_without_plan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let status = std::process::Command::new("git")
            .args([
                "-C",
                root.to_str().unwrap(),
                "init",
                "--quiet",
                "--initial-branch=main",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let root = fs::canonicalize(root).unwrap();
        let state_path = dir.path().join("state.json");
        write_private(&state_path, empty_state_json());
        let state_path = fs::canonicalize(state_path).unwrap();
        let selected = selected_root(&root, &state_path, RootProfileV1::GitRawV1);

        let observed = match observe_selected_registered_root_sources_for_test_v1(
            "work",
            &selected,
            &root,
            &state_path,
            &matching_remote_fixture_operator_v1(&[]),
        )
        .await
        .unwrap()
        {
            StrictRegisteredRootSourcesReadV1::GitRawLocalTopologyObserved(observed) => observed,
            StrictRegisteredRootSourcesReadV1::Observed(_) => {
                panic!("expected GitRaw local topology, got AgentStatic sources")
            }
            StrictRegisteredRootSourcesReadV1::Incomplete(incomplete) => {
                panic!("expected GitRaw local topology, got {incomplete:?}")
            }
        };
        assert_eq!(observed.root_id(), "work");
        assert_eq!(observed.canonical_local_root(), root);
        assert_eq!(observed.canonical_state_path(), state_path);
        assert_eq!(observed.remote_prefix(), "roots");
        assert_eq!(
            observed.git_topology().head().symbolic_target(),
            Some("refs/heads/main")
        );
        assert_eq!(observed.git_topology().refs().len(), 0);
        assert!(observed.namespace_safety_claim_count() > 0);
    }

    #[tokio::test]
    async fn local_inventory_c_brackets_remote_reads_before_git_shadow_promotion() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let status = std::process::Command::new("git")
            .args([
                "-C",
                root.to_str().unwrap(),
                "init",
                "--quiet",
                "--initial-branch=main",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let root = fs::canonicalize(root).unwrap();
        let state_path = dir.path().join("state.json");
        write_private(&state_path, empty_state_json());
        let state_path = fs::canonicalize(state_path).unwrap();
        let selected = selected_root(&root, &state_path, RootProfileV1::GitRawV1);
        let remote = matching_remote_fixture_operator_with_first_list_write_v1(
            &[],
            root.join(".git/HEAD"),
            b"ref: refs/heads/other\n".to_vec(),
        );

        match observe_selected_registered_root_sources_for_test_v1(
            "work",
            &selected,
            &root,
            &state_path,
            &remote,
        )
        .await
        .unwrap()
        {
            StrictRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::Local(incomplete),
            ) if incomplete.kind() == StrictLocalSnapshotIncompleteKindV1::ChangedDuringRead => {}
            other => {
                let _ = other;
                panic!("remote-window Git mutation must fail at local inventory C");
            }
        }
    }

    #[tokio::test]
    async fn source_presence_lattice_uses_all_three_production_adapters() {
        let sources = [
            NamespaceSafetyClaimSourceV1::LocalCurrent,
            NamespaceSafetyClaimSourceV1::StateBaseline,
            NamespaceSafetyClaimSourceV1::RemoteCurrent,
        ];
        for mask in 0_u8..8 {
            let local = if mask & 0b001 == 0 {
                &[][..]
            } else {
                &["same"][..]
            };
            let state = if mask & 0b010 == 0 {
                &[][..]
            } else {
                &["same"][..]
            };
            let remote = if mask & 0b100 == 0 {
                Vec::new()
            } else {
                vec![RemoteNamespaceFixtureRowV1::CurrentFile("same".to_owned())]
            };
            let observed = observed(observe_fixture(local, state, &remote).await);
            assert_eq!(
                observed.namespace_safety_claim_count(),
                u64::from(mask != 0)
            );
            for (bit, source) in sources.into_iter().enumerate() {
                assert_eq!(
                    observed.namespace_safety_source_claim_count(source),
                    u64::from(mask & (1 << bit) != 0),
                    "source mask {mask:03b}"
                );
            }
        }
    }

    #[tokio::test]
    async fn every_cross_source_pair_rejects_real_alias_conflicts() {
        let cases = [
            (
                vec!["Readme"],
                vec!["README"],
                Vec::new(),
                NamespaceSafetyClaimSourceV1::LocalCurrent,
                NamespaceSafetyClaimSourceV1::StateBaseline,
            ),
            (
                vec!["Readme"],
                Vec::new(),
                vec![RemoteNamespaceFixtureRowV1::CurrentFile(
                    "README".to_owned(),
                )],
                NamespaceSafetyClaimSourceV1::LocalCurrent,
                NamespaceSafetyClaimSourceV1::RemoteCurrent,
            ),
            (
                Vec::new(),
                vec!["Readme"],
                vec![RemoteNamespaceFixtureRowV1::CurrentFile(
                    "README".to_owned(),
                )],
                NamespaceSafetyClaimSourceV1::StateBaseline,
                NamespaceSafetyClaimSourceV1::RemoteCurrent,
            ),
        ];
        for (local, state, remote, first, conflicting) in cases {
            let alias = conflict(observe_fixture(&local, &state, &remote).await);
            assert_eq!(
                alias.kind(),
                NamespaceSafetyClaimConflictKindV1::FoldedSpellingAlias
            );
            assert_eq!(alias.first_source(), first);
            assert_eq!(alias.conflicting_source(), conflicting);
        }
    }

    #[tokio::test]
    async fn historical_and_reservation_pairs_reject_all_conflict_geometries() {
        let remote_alias = |historical: bool| {
            if historical {
                RemoteNamespaceFixtureRowV1::DeletedFile("README".to_owned())
            } else {
                RemoteNamespaceFixtureRowV1::Reservation {
                    exact_path: "README".to_owned(),
                    role: PortableNamespaceRole::File,
                }
            }
        };
        let remote_directory = |historical: bool, path: &str| {
            if historical {
                RemoteNamespaceFixtureRowV1::DeletedDirectory(path.to_owned())
            } else {
                RemoteNamespaceFixtureRowV1::Reservation {
                    exact_path: path.to_owned(),
                    role: PortableNamespaceRole::Directory,
                }
            }
        };
        let remote_file = |historical: bool, path: &str| {
            if historical {
                RemoteNamespaceFixtureRowV1::DeletedFile(path.to_owned())
            } else {
                RemoteNamespaceFixtureRowV1::Reservation {
                    exact_path: path.to_owned(),
                    role: PortableNamespaceRole::File,
                }
            }
        };

        for (historical, remote_source) in [
            (true, NamespaceSafetyClaimSourceV1::RemoteHistorical),
            (false, NamespaceSafetyClaimSourceV1::RemoteReservation),
        ] {
            for (local, state, first_source) in [
                (
                    vec!["Readme"],
                    Vec::new(),
                    NamespaceSafetyClaimSourceV1::LocalCurrent,
                ),
                (
                    Vec::new(),
                    vec!["Readme"],
                    NamespaceSafetyClaimSourceV1::StateBaseline,
                ),
            ] {
                let alias =
                    conflict(observe_fixture(&local, &state, &[remote_alias(historical)]).await);
                assert_eq!(
                    alias.kind(),
                    NamespaceSafetyClaimConflictKindV1::FoldedSpellingAlias
                );
                assert_eq!(alias.first_source(), first_source);
                assert_eq!(alias.conflicting_source(), remote_source);
            }

            for (local, state, first_source) in [
                (
                    vec!["path"],
                    Vec::new(),
                    NamespaceSafetyClaimSourceV1::LocalCurrent,
                ),
                (
                    Vec::new(),
                    vec!["path"],
                    NamespaceSafetyClaimSourceV1::StateBaseline,
                ),
            ] {
                for remote in [
                    remote_directory(historical, "path"),
                    remote_file(historical, "path/child"),
                ] {
                    let role = conflict(
                        observe_fixture(&local, &state, std::slice::from_ref(&remote)).await,
                    );
                    assert_eq!(
                        role.kind(),
                        NamespaceSafetyClaimConflictKindV1::FileDirectoryRole
                    );
                    assert_eq!(role.first_source(), first_source);
                    assert_eq!(role.conflicting_source(), remote_source);
                }
            }

            for (local, state, first_source) in [
                (
                    vec!["path/child"],
                    Vec::new(),
                    NamespaceSafetyClaimSourceV1::LocalCurrent,
                ),
                (
                    Vec::new(),
                    vec!["path/child"],
                    NamespaceSafetyClaimSourceV1::StateBaseline,
                ),
            ] {
                let role = conflict(
                    observe_fixture(&local, &state, &[remote_file(historical, "path")]).await,
                );
                assert_eq!(
                    role.kind(),
                    NamespaceSafetyClaimConflictKindV1::FileDirectoryRole
                );
                assert_eq!(role.first_source(), first_source);
                assert_eq!(role.conflicting_source(), remote_source);
            }
        }
    }

    #[tokio::test]
    async fn remote_origin_pairs_fail_closed_inside_the_remote_reader() {
        use crate::registered_remote_observation::RemoteNamespaceClaimConflictV1;

        let cases = [
            (
                vec![
                    RemoteNamespaceFixtureRowV1::CurrentFile("Readme".to_owned()),
                    RemoteNamespaceFixtureRowV1::DeletedFile("README".to_owned()),
                ],
                RemoteNamespaceClaimConflictV1::FoldedSpellingAlias,
            ),
            (
                vec![
                    RemoteNamespaceFixtureRowV1::CurrentFile("path".to_owned()),
                    RemoteNamespaceFixtureRowV1::Reservation {
                        exact_path: "path".to_owned(),
                        role: PortableNamespaceRole::Directory,
                    },
                ],
                RemoteNamespaceClaimConflictV1::FileDirectoryRole,
            ),
            (
                vec![
                    RemoteNamespaceFixtureRowV1::DeletedFile("parent".to_owned()),
                    RemoteNamespaceFixtureRowV1::Reservation {
                        exact_path: "parent/child".to_owned(),
                        role: PortableNamespaceRole::File,
                    },
                ],
                RemoteNamespaceClaimConflictV1::FileDirectoryRole,
            ),
            (
                vec![
                    RemoteNamespaceFixtureRowV1::DeletedFile("parent/child".to_owned()),
                    RemoteNamespaceFixtureRowV1::Reservation {
                        exact_path: "parent".to_owned(),
                        role: PortableNamespaceRole::File,
                    },
                ],
                RemoteNamespaceClaimConflictV1::FileDirectoryRole,
            ),
        ];
        for (rows, expected_reason) in cases {
            assert!(matches!(
                observe_fixture(&[], &[], &rows).await,
                StrictRegisteredRootSourcesReadV1::Incomplete(
                    StrictRegisteredRootSourcesIncompleteV1::Remote(
                        StrictBoundRemoteObservationIncompleteV1::Claim {
                            reason,
                            ..
                        }
                    )
                ) if reason == expected_reason
            ));
        }
    }

    #[tokio::test]
    async fn every_cross_source_pair_rejects_real_role_and_ancestor_conflicts() {
        let cases = [
            (
                vec!["path/child"],
                vec!["path"],
                Vec::new(),
                NamespaceSafetyClaimSourceV1::LocalCurrent,
                NamespaceSafetyClaimSourceV1::StateBaseline,
            ),
            (
                vec!["path"],
                Vec::new(),
                vec![RemoteNamespaceFixtureRowV1::CurrentDirectory(
                    "path".to_owned(),
                )],
                NamespaceSafetyClaimSourceV1::LocalCurrent,
                NamespaceSafetyClaimSourceV1::RemoteCurrent,
            ),
            (
                Vec::new(),
                vec!["path"],
                vec![RemoteNamespaceFixtureRowV1::CurrentDirectory(
                    "path".to_owned(),
                )],
                NamespaceSafetyClaimSourceV1::StateBaseline,
                NamespaceSafetyClaimSourceV1::RemoteCurrent,
            ),
            (
                vec!["path"],
                vec!["path/child"],
                Vec::new(),
                NamespaceSafetyClaimSourceV1::LocalCurrent,
                NamespaceSafetyClaimSourceV1::StateBaseline,
            ),
            (
                vec!["path"],
                Vec::new(),
                vec![RemoteNamespaceFixtureRowV1::CurrentFile(
                    "path/child".to_owned(),
                )],
                NamespaceSafetyClaimSourceV1::LocalCurrent,
                NamespaceSafetyClaimSourceV1::RemoteCurrent,
            ),
            (
                Vec::new(),
                vec!["path"],
                vec![RemoteNamespaceFixtureRowV1::CurrentFile(
                    "path/child".to_owned(),
                )],
                NamespaceSafetyClaimSourceV1::StateBaseline,
                NamespaceSafetyClaimSourceV1::RemoteCurrent,
            ),
        ];
        for (local, state, remote, first, conflicting) in cases {
            let role = conflict(observe_fixture(&local, &state, &remote).await);
            assert_eq!(
                role.kind(),
                NamespaceSafetyClaimConflictKindV1::FileDirectoryRole
            );
            assert_eq!(role.first_source(), first);
            assert_eq!(role.conflicting_source(), conflicting);
        }
    }

    #[tokio::test]
    async fn namespace_history_counts_authentic_remote_temporal_origins_without_actions() {
        let observed = observed(
            observe_fixture(
                &[],
                &[],
                &[
                    RemoteNamespaceFixtureRowV1::CurrentFile("path/child".to_owned()),
                    RemoteNamespaceFixtureRowV1::DeletedDirectory("path".to_owned()),
                    RemoteNamespaceFixtureRowV1::Reservation {
                        exact_path: "path".to_owned(),
                        role: PortableNamespaceRole::Directory,
                    },
                ],
            )
            .await,
        );
        assert_eq!(observed.namespace_safety_claim_count(), 2);
        assert_eq!(
            observed
                .namespace_safety_source_claim_count(NamespaceSafetyClaimSourceV1::RemoteCurrent),
            2
        );
        assert_eq!(
            observed.namespace_safety_source_claim_count(
                NamespaceSafetyClaimSourceV1::RemoteHistorical
            ),
            1
        );
        assert_eq!(
            observed.namespace_safety_source_claim_count(
                NamespaceSafetyClaimSourceV1::RemoteReservation
            ),
            1
        );
    }

    #[tokio::test]
    async fn fixed_ingress_exclusion_does_not_erase_remote_namespace_history() {
        let observed = observed(
            observe_fixture(
                &["home/.ssh/id_ed25519"],
                &[],
                &[
                    RemoteNamespaceFixtureRowV1::DeletedDirectory("home/.ssh".to_owned()),
                    RemoteNamespaceFixtureRowV1::Reservation {
                        exact_path: "home/.ssh".to_owned(),
                        role: PortableNamespaceRole::Directory,
                    },
                ],
            )
            .await,
        );
        assert_eq!(
            observed
                .namespace_safety_source_claim_count(NamespaceSafetyClaimSourceV1::LocalCurrent),
            1,
            "only the safe `home` ancestor remains; `.ssh` and its leaf are excluded"
        );
        assert_eq!(
            observed
                .namespace_safety_source_claim_count(NamespaceSafetyClaimSourceV1::RemoteCurrent),
            0
        );
        assert_eq!(
            observed.namespace_safety_source_claim_count(
                NamespaceSafetyClaimSourceV1::RemoteHistorical
            ),
            2
        );
        assert_eq!(
            observed.namespace_safety_source_claim_count(
                NamespaceSafetyClaimSourceV1::RemoteReservation
            ),
            2
        );
    }

    #[test]
    fn route_provenance_mismatch_is_typed_even_for_empty_inputs() {
        let profile = RootProfileV1::AgentStaticV1.policy();
        let git_profile = RootProfileV1::GitRawV1.policy();
        let contract = RegisteredRootPlanContractV1::strict_v1();
        let expected = ExpectedRegisteredSourceRouteV1 {
            canonical_local_root: Path::new("/root-a"),
            canonical_state_path: Path::new("/state-a.json"),
            remote_prefix: "roots-a",
            profile: RootProfileV1::AgentStaticV1,
            profile_settings_fingerprint: profile.settings_fingerprint(),
            plan_contract_fingerprint: contract.fingerprint(),
        };
        let base = || ObservedRegisteredSourceRouteV1 {
            held_local_root: Path::new("/root-a"),
            state_path: Path::new("/state-a.json"),
            state_local_root: Path::new("/root-a"),
            state_remote_prefix: "roots-a",
            remote_prefix: "roots-a",
            profile: RootProfileV1::AgentStaticV1,
            profile_settings_fingerprint: profile.settings_fingerprint(),
            plan_contract_fingerprint: contract.fingerprint(),
        };
        assert_eq!(
            validate_observed_source_route_v1(&expected, &base()),
            Ok(())
        );

        let cases = [
            (
                ObservedRegisteredSourceRouteV1 {
                    held_local_root: Path::new("/root-b"),
                    ..base()
                },
                RegisteredSourceRouteMismatchV1::HeldLocalRoot,
            ),
            (
                ObservedRegisteredSourceRouteV1 {
                    state_path: Path::new("/state-b.json"),
                    ..base()
                },
                RegisteredSourceRouteMismatchV1::StatePath,
            ),
            (
                ObservedRegisteredSourceRouteV1 {
                    state_local_root: Path::new("/root-b"),
                    ..base()
                },
                RegisteredSourceRouteMismatchV1::StateLocalRoot,
            ),
            (
                ObservedRegisteredSourceRouteV1 {
                    state_remote_prefix: "roots-b",
                    ..base()
                },
                RegisteredSourceRouteMismatchV1::StateRemotePrefix,
            ),
            (
                ObservedRegisteredSourceRouteV1 {
                    remote_prefix: "roots-b",
                    ..base()
                },
                RegisteredSourceRouteMismatchV1::RemotePrefix,
            ),
            (
                ObservedRegisteredSourceRouteV1 {
                    profile: RootProfileV1::GitRawV1,
                    ..base()
                },
                RegisteredSourceRouteMismatchV1::Profile,
            ),
            (
                ObservedRegisteredSourceRouteV1 {
                    profile_settings_fingerprint: git_profile.settings_fingerprint(),
                    ..base()
                },
                RegisteredSourceRouteMismatchV1::ProfileSettingsFingerprint,
            ),
        ];
        for (observed, expected_mismatch) in cases {
            assert_eq!(
                validate_observed_source_route_v1(&expected, &observed),
                Err(expected_mismatch)
            );
        }
    }

    #[tokio::test]
    async fn configured_route_mismatch_stops_before_acquisition() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let other = dir.path().join("other");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&other).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let other = fs::canonicalize(other).unwrap();
        let state_path = dir.path().join("state.json");
        write_private(&state_path, empty_state_json());
        let state_path = fs::canonicalize(state_path).unwrap();
        let selected = selected_root(&root, &state_path, RootProfileV1::AgentStaticV1);

        assert!(matches!(
            observe_selected_registered_root_sources_for_test_v1(
                "work",
                &selected,
                &other,
                &state_path,
                &matching_remote_fixture_operator_v1(&[]),
            )
            .await
            .unwrap(),
            StrictRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::RouteMismatch(
                    RegisteredSourceRouteMismatchV1::ConfiguredLocalRoot
                )
            )
        ));
    }

    #[tokio::test]
    async fn selected_route_shape_binding_and_state_failures_are_typed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let state_path = dir.path().join("state.json");
        write_private(&state_path, empty_state_json());
        let state_path = fs::canonicalize(state_path).unwrap();
        let selected = selected_root(&root, &state_path, RootProfileV1::AgentStaticV1);
        let op = matching_remote_fixture_operator_v1(&[]);

        assert!(matches!(
            observe_selected_registered_root_sources_for_test_v1(
                "INVALID",
                &selected,
                &root,
                &state_path,
                &op,
            )
            .await
            .unwrap(),
            StrictRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::InvalidSelectedRoot
            )
        ));

        let mut missing_binding = selected.clone();
        missing_binding.binding = None;
        assert!(matches!(
            observe_selected_registered_root_sources_for_test_v1(
                "work",
                &missing_binding,
                &root,
                &state_path,
                &op,
            )
            .await
            .unwrap(),
            StrictRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::BindingMissing
            )
        ));

        let other_state_path = dir.path().join("other-state.json");
        write_private(&other_state_path, empty_state_json());
        let other_state_path = fs::canonicalize(other_state_path).unwrap();
        assert!(matches!(
            observe_selected_registered_root_sources_for_test_v1(
                "work",
                &selected,
                &root,
                &other_state_path,
                &op,
            )
            .await
            .unwrap(),
            StrictRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::RouteMismatch(
                    RegisteredSourceRouteMismatchV1::ConfiguredStatePath
                )
            )
        ));

        let missing_state_path = dir.path().join("missing-state.json");
        let missing_state_selected =
            selected_root(&root, &missing_state_path, RootProfileV1::AgentStaticV1);
        assert!(matches!(
            observe_selected_registered_root_sources_for_test_v1(
                "work",
                &missing_state_selected,
                &root,
                &missing_state_path,
                &op,
            )
            .await
            .unwrap(),
            StrictRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::State(
                    StrictPrimaryStateIncompleteV1::PrimaryMissing
                )
            )
        ));
    }

    #[tokio::test]
    async fn local_and_remote_acquisition_failures_are_typed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("first"), b"same inode").unwrap();
        fs::hard_link(root.join("first"), root.join("second")).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let state_path = dir.path().join("state.json");
        write_private(&state_path, empty_state_json());
        let state_path = fs::canonicalize(state_path).unwrap();
        let selected = selected_root(&root, &state_path, RootProfileV1::AgentStaticV1);
        assert!(matches!(
            observe_selected_registered_root_sources_for_test_v1(
                "work",
                &selected,
                &root,
                &state_path,
                &matching_remote_fixture_operator_v1(&[]),
            )
            .await
            .unwrap(),
            StrictRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::Local(incomplete)
            ) if incomplete.kind() == StrictLocalSnapshotIncompleteKindV1::HardlinkRejected
        ));

        let clean_dir = tempfile::tempdir().unwrap();
        let clean_root = clean_dir.path().join("root");
        fs::create_dir(&clean_root).unwrap();
        let clean_root = fs::canonicalize(clean_root).unwrap();
        let clean_state_path = clean_dir.path().join("state.json");
        write_private(&clean_state_path, empty_state_json());
        let clean_state_path = fs::canonicalize(clean_state_path).unwrap();
        let clean_selected =
            selected_root(&clean_root, &clean_state_path, RootProfileV1::AgentStaticV1);
        assert!(matches!(
            observe_selected_registered_root_sources_for_test_v1(
                "work",
                &clean_selected,
                &clean_root,
                &clean_state_path,
                &missing_remote_index_fixture_operator_v1("ghost"),
            )
            .await
            .unwrap(),
            StrictRegisteredRootSourcesReadV1::Incomplete(
                StrictRegisteredRootSourcesIncompleteV1::Remote(
                    StrictBoundRemoteObservationIncompleteV1::ListedObjectMissing {
                        kind: crate::registered_remote_observation::BoundRemoteObjectKindV1::OrdinaryIndex,
                        ..
                    }
                )
            )
        ));
    }
}
