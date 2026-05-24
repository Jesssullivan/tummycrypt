# TCFS Alpha/Beta QA Readiness - May 19, 2026

This is the current claim boundary for moving TCFS toward daily-driver use.
It is deliberately stricter than "the code path works": alpha and beta need
repeatable evidence, bounded failure modes, and clear limits on what testers are
allowed to trust.

Current execution board:
[TCFS Alpha Productionization Sprint - May 20, 2026](tcfs-alpha-productionization-sprint-2026-05-20.md).

## Current Posture

TCFS is ready for focused productionization QA, not for broad primary-filesystem
use.

- The macOS production Developer ID FileProvider lifecycle is proven on
  petting-zoo-mini. The current strongest public-asset packet is run
  `26218940950` against `v0.12.13-rc4`: signed HostApp root enumeration,
  exact hydrate, evict/rehydrate, mutation upload/readback, rename, and
  conflict/status without `fileprovider_testing_mode=true`. Product hardening
  remains open in `TIN-1547` for badge/progress/recovery assertions, a longer
  desktop soak, and first-run setup handoff.
- Linux is the strongest runtime for CLI/daemon/FUSE work. `TIN-1540` and
  `TIN-1422` are closed for the current alpha package-smoke boundary, and
  `TIN-131` / GitHub #280 are closed for the install/upgrade matrix. That does
  not yet claim FUSE/systemd/live-storage first-use for every package surface.
- Real-storage CI exists via `TIN-1421`, but live multi-host fleet acceptance
  remains `TIN-132`; CI does not replace named host evidence.
- Enrollment and invite flows are not a production trust boundary. `TIN-1424`
  is urgent/prod-blocker, and `TIN-1417` must land before self-enrollment or
  lost-device revocation is product-real.
- Production S3/storage posture remains `TIN-1546`. Alpha HTTPS/scoped
  credential posture is green on current-main storage canary evidence, but the
  beta-grade gate still needs large restore/load, socket/highwater behavior,
  transient recovery classification, and soak evidence.
  Use `tcfs storage canary --json` as the scoped read/write/delete/delete-verify
  probe in future packets; it is supporting evidence, not a full posture claim
  by itself.
- iOS remains proof-of-concept until `TIN-1548` proves a real Files.app lane
  with safe enrollment posture.

## Alpha QA Claim

Alpha QA is allowed only for trusted, named testers on operator-managed
infrastructure.

Alpha may exercise:

- release artifacts and source builds on disposable or shadow sync roots
- scoped project trees, repo canaries, and small daily-use folders
- macOS FileProvider lifecycle on the `v0.12.13-rc4` release asset, with
  main-ref and diagnostic-artifact reruns used as release-day regression
  evidence
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
| Release and first-use | Exact release artifacts install and upgrade for the current package-smoke boundary; first-run config remains manual | Repeatable install and upgrade matrix with no hand-authored config for the common path | `TIN-131`, `#280`, `TIN-1425` |
| macOS FileProvider | Published rc4 `.pkg` exact hydrate plus evict/rehydrate, mutation, rename, and conflict/status on the Developer ID surface; main-ref continuous rerun preferred for release-day viability | Badges/progress, recovery UX, and longer desktop soak | `TIN-1547`, `TIN-133` |
| Linux package smoke | `.deb`/`.rpm` install and upgrade package smoke is green for the current alpha boundary | Scheduled package smoke on a stable runner with archived transcripts and broader FUSE/systemd/live-storage first-use | `TIN-1422`, `TIN-1540` |
| Live fleet | Neo/honey acceptance is current, repeatable, and archived | Scheduled fleet acceptance with failure classification and dashboard history | `TIN-132`, `TIN-1421` |
| S3/storage posture | TLS/CA posture documented, health/read paths bounded, transient errors separated from missing objects, large-pack restore evidence captured | Production-like S3 endpoint, scoped credentials, latency/object-count budgets, rollback/restore evidence | `TIN-1546`, `TIN-720`, `#327` |
| Enrollment/security | Admin/operator-provisioned only; self-enrollment disabled for untrusted users | Real per-device identity, complete invite signature/MAC coverage, admin/session gating, safe bootstrap persistence, revocation evidence | `TIN-1417`, `TIN-1424` |
| Daily-driver primitives | Scoped roots only; no broad home ownership claim | Stable root identity, pin-as-hydrated, subscription selective sync, streaming large-file IO, xattrs, conflict UX, home-dir blacklist | `TIN-1416`, `TIN-1419`, `TIN-1420`, `TIN-1423`, `TIN-1556` |
| Desktop status and recovery | CLI/log-based inspection is acceptable for trusted operators | Cross-surface sync-state vocabulary, visible progress/errors/conflicts, and clean user-driven recovery | `TIN-1549` |
| Multitenancy | Out of scope | Tenant/vault identity, namespaced storage/NATS/authz/audit/metrics, migration story | `TIN-1418` |
| iOS | Proof-of-concept only | Files.app real-device/simulator acceptance, safe enrollment posture, TestFlight/provisioning decision | `TIN-1548`, `TIN-134` |

## This Week's Alpha Runway

Start each pass with the read-only gate classifier:

```bash
scripts/tcfs-alpha-gate-preflight.sh
```

1. Keep the `v0.12.13-rc4` exact public `.pkg` smoke packet from run
   `26218940950` as the current Mac alpha baseline, and keep a main-ref/package
   rerun ready for the next rc or FileProvider change.
2. Treat `TIN-1540`, `TIN-1422`, `TIN-131`, and GitHub #280 as closed for the
   current alpha claim. Re-run the Linux/package lanes on release-day if the
   release candidate changes.
3. Continue the alpha slice of `TIN-1547`: badge/progress/recovery assertions
   and longer desktop soak after the landed PR #412 rename/unsync safety cut.
4. Continue the `TIN-1546` storage mini-gate: large restore/load,
   socket/highwater, transient recovery, and soak evidence after the current
   HTTPS/scoped-credential canary.
5. Refresh `TIN-132` with a current two-host transcript when the release-day
   fleet claim needs renewal; do not infer named-host acceptance from CI alone.

## Evidence Required Per QA Run

Every alpha/beta evidence packet should include:

- exact artifact tag, commit, package name, and checksum when available
- host, OS version, architecture, and install path
- remote endpoint type, bucket/prefix, and whether TLS/custom CA was used
- `tcfs storage canary --json` output when the run claims scoped storage
  read/write/delete behavior
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
