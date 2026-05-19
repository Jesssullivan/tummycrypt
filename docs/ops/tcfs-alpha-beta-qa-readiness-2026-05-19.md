# TCFS Alpha/Beta QA Readiness - May 19, 2026

This is the current claim boundary for moving TCFS toward daily-driver use.
It is deliberately stricter than "the code path works": alpha and beta need
repeatable evidence, bounded failure modes, and clear limits on what testers are
allowed to trust.

## Current Posture

TCFS is ready for focused productionization QA, not for broad primary-filesystem
use.

- The macOS production Developer ID FileProvider lifecycle is proven on
  petting-zoo-mini by run `26062554542`: install, domain rebuild, enumerate,
  exact hydrate, host evict/rehydrate, remote mutation, and conflict/status.
  Exact post-cut release-asset smoke and product hardening remain open in
  `TIN-1547`.
- Linux remains the strongest runtime for CLI/daemon/FUSE work, but package
  first-use is not fully proven. `TIN-1422` is blocked on `TIN-1540` until the
  Linux smoke backend is reachable from CI or a private runner.
- Real-storage CI exists via `TIN-1421`, but live multi-host fleet acceptance
  remains `TIN-132`; CI does not replace named host evidence.
- Enrollment and invite flows are not a production trust boundary. `TIN-1424`
  is urgent/prod-blocker, and `TIN-1417` must land before self-enrollment or
  lost-device revocation is product-real.
- Production S3/storage posture is not proven until `TIN-1546` covers TLS/CA
  posture, bounded health checks, request/read timeouts, transient error
  classification, and large-object restore evidence.
- iOS remains proof-of-concept until `TIN-1548` proves a real Files.app lane
  with safe enrollment posture.

## Alpha QA Claim

Alpha QA is allowed only for trusted, named testers on operator-managed
infrastructure.

Alpha may exercise:

- release artifacts and source builds on disposable or shadow sync roots
- scoped project trees, repo canaries, and small daily-use folders
- macOS FileProvider lifecycle after `TIN-1547` release-asset smoke
- Linux FUSE clean-name traversal and hydrate-on-open after `TIN-1422`
- live fleet sync against named hosts after `TIN-132`
- storage latency, object-count, retry, and failure-classification evidence

Alpha must not claim:

- primary home-directory takeover
- broad `~/git`, `~/Documents`, package-cache, or dotfile ownership
- self-service enrollment for arbitrary users
- meaningful lost-device revocation
- multitenant backend isolation
- iOS production readiness
- Windows/Explorer readiness

## Beta QA Claim

Beta QA starts only after TCFS can support trusted external users without
operator hand-holding for the common path.

Beta requires:

- first-run setup that writes valid config and reaches `tcfs status [ok]`
- package install and upgrade proof across the claimed platforms
- per-device cryptographic identity and device-local private keys
- invite/signature/admin gating that rejects tampered or failed bootstrap
- revocation semantics that deny new-content access after device removal
- production S3/storage posture with bounded health and request behavior
- FileProvider rename/unsync semantics that cannot silently destroy user data
- visible progress/status/recovery affordances on desktop surfaces
- selective-sync semantics users can understand and recover from

Beta still does not imply multitenancy unless the tenant model in `TIN-1418`
has shipped and been proven end to end.

## Gate Matrix

| Lane | Alpha gate | Beta gate | Tracker |
|---|---|---|---|
| Release and first-use | Exact release artifacts install, configure, `status [ok]`, and perform one real action on macOS, Homebrew, Linux `.deb`, container, and Nix surfaces | Repeatable install and upgrade matrix with no hand-authored config for the common path | `TIN-131`, `#280`, `TIN-1425` |
| macOS FileProvider | Post-cut `.pkg` exact hydrate plus evict/rehydrate, mutation, conflict/status on Developer ID surface | Rename, unsync-vs-delete, badges/progress, recovery UX, and longer desktop soak | `TIN-1547`, `TIN-133` |
| Linux package smoke | `.deb`/`.rpm` install against reachable SeaweedFS+NATS backend, hydrate fixture, evict, rehydrate | Scheduled package smoke on a stable runner with archived transcripts | `TIN-1422`, `TIN-1540` |
| Live fleet | Neo/honey acceptance is current, repeatable, and archived | Scheduled fleet acceptance with failure classification and dashboard history | `TIN-132`, `TIN-1421` |
| S3/storage posture | TLS/CA posture documented, health/read paths bounded, transient errors separated from missing objects, large-pack restore evidence captured | Production-like S3 endpoint, scoped credentials, latency/object-count budgets, rollback/restore evidence | `TIN-1546`, `TIN-720`, `#327` |
| Enrollment/security | Admin/operator-provisioned only; self-enrollment disabled for untrusted users | Real per-device identity, complete invite signature/MAC coverage, admin/session gating, safe bootstrap persistence, revocation evidence | `TIN-1417`, `TIN-1424` |
| Daily-driver primitives | Scoped roots only; no broad home ownership claim | Pin-as-hydrated, subscription selective sync, streaming large-file IO, xattrs, conflict UX, home-dir blacklist | `TIN-1416`, `TIN-1419`, `TIN-1420`, `TIN-1423` |
| Desktop status and recovery | CLI/log-based inspection is acceptable for trusted operators | Cross-surface sync-state vocabulary, visible progress/errors/conflicts, and clean user-driven recovery | `TIN-1549` |
| Multitenancy | Out of scope | Tenant/vault identity, namespaced storage/NATS/authz/audit/metrics, migration story | `TIN-1418` |
| iOS | Proof-of-concept only | Files.app real-device/simulator acceptance, safe enrollment posture, TestFlight/provisioning decision | `TIN-1548`, `TIN-134` |

## This Week's Alpha Runway

1. Merge the production-readiness sweep PR so the docs, evidence packet, and
   Linux smoke remote-spec fix are on main.
2. Clear `TIN-1540`, then rerun `TIN-1422` against a reachable backend using
   the corrected `seaweedfs://host:port/bucket/prefix` remote spec.
3. Complete the `TIN-131` first-use matrix for the rc artifact set:
   macOS `.pkg`, Homebrew, Linux `.deb`, container runtime, and Nix external
   profile.
4. Burn down the alpha slice of `TIN-1547`: exact release `.pkg` smoke,
   rename/unsync risk classification, and minimum visible status/recovery
   notes.
5. Start the `TIN-1546` storage mini-gate: bounded health/read timeouts,
   transient-error classification, and latency/object-count evidence for the
   large Git-pack restore path.
6. Refresh `TIN-132` with a current two-host transcript so live fleet evidence
   is not inferred from CI.

## Evidence Required Per QA Run

Every alpha/beta evidence packet should include:

- exact artifact tag, commit, package name, and checksum when available
- host, OS version, architecture, and install path
- remote endpoint type, bucket/prefix, and whether TLS/custom CA was used
- command transcript or workflow run ID
- first-byte hydrate, full hydrate, cache-hit read, and rehydrate timings
- object counts, byte counts, retry counts, and notable storage errors
- final claim statement: what this run proves and what it does not prove

## Stop Rules

Stop the alpha/beta claim and file or escalate a prod-blocker if any of these
occur:

- invite bootstrap accepts a tampered, unsigned, or failed payload
- user data can be deleted or renamed ambiguously by FileProvider unsync logic
- storage health, read, or exists calls can hang without a bounded timeout
- a transient storage/backend error is reported as a missing file/object
- package install leaves the user with no supported path to valid config
- iOS or desktop bootstrap persists raw production storage credentials
- a revoked device can continue receiving new-content keys without rotation or
  rewrap
