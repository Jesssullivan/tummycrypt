# Product Reality And Priority

As of May 9, 2026, `tummycrypt` is in a much better state operationally than
its remaining gaps might suggest.

The latest release is `v0.12.12`, and most release-facing surfaces now have
explicit proof paths. The important distinction is that `buildable`,
`packaged`, and `actually proven in user-facing flows` are still different
things.

Use this document as the short answer to:

- what is actually proven today
- what is still only buildable or manually explorable
- what should be prioritized next

## Current Product Posture

| Surface | Current truth | Source of proof |
| --- | --- | --- |
| Linux CLI + daemon | strongest and most routinely proven path; x86_64 FUSE lifecycle has current real-host evidence, while packaged systemd/mount first-use remains a separate gate | CI, release smoke, live host acceptance, archived Linux lifecycle evidence |
| Fleet sync / backend path | materially exercised on real hosts, but current live transcripts should be archived per run before treating them as release evidence | `neo-honey` live acceptance plus lab host matrix |
| Lazy traversal / hydration | core code and harnesses exist; Linux FUSE proves browse-before-download, exact `cat` hydration, mounted write/readback, cache clear/rehydrate, and recursive safe-unsync refusal/success on real host evidence; PZM proves macOS FileProvider enumerate, exact-content hydrate, evict, rehydrate, and mutation-through-CloudStorage under testing mode with the installed lab `SystemPolicyRule` profile; production Finder lifecycle evidence is still pending | `tcfs-vfs`/FUSE implementation, archived Linux evidence, PZM testing-mode smoke, and the lazy hydration demo runbook |
| macOS | experimental but real; current packages prove package/signing/storage/daemon startup, and PZM proves non-production lab FileProvider enumeration/hydration/evict/rehydrate plus mutation upload/readback and CLI conflict/exact-content preservation under Apple's testing-mode entitlement plus a managed SystemPolicyRule profile; production Finder enablement/conflict/status UX are still not release-grade | build + packaging + PZM smoke + local desktop evidence |
| iOS | proof-of-concept | Swift type-check and scaffold only |
| Windows | planned / skeleton | code exists, but there is no release-grade CLI, daemon, or Explorer flow |

## What Is Actually Proven

### 1. Release Artifact Proof

This is the narrowest and most important truth for public release claims.

| Surface | Status | Current reality |
| --- | --- | --- |
| Homebrew | current-tag pass | fresh install and upgrade proved on `v0.12.12`; current evidence is `docs/release/evidence/distribution-v01212-20260508T205913Z/` |
| macOS `.pkg` | partial pass | package install/signing/provisioning, daemon startup, and E2EE fixture gates have been proven in release/PZM lanes, but current production Developer ID clean-host Finder acceptance remains open; the PZM non-production testing-mode package proves FileProvider enumerate, hydrate, evict, rehydrate, mutation, and conflict/status content preservation on runs `25562087555`, `25565943781`, and `25569596910` |
| `.deb` | current-tag pass | support floor is Ubuntu 24.04+ / Debian 13+; Debian 12 is excluded unless a separate bookworm-targeted package is produced; `v0.12.12` repo-archived evidence proves Ubuntu 24.04 fresh/upgrade on arm64 and amd64 plus Debian 13 fresh install on arm64 and amd64 |
| `.rpm` | current-tag pass | RPM is daemon-only today; `v0.12.12` repo-archived evidence proves Fedora 42 x86_64 fresh install and sampled `0.12.2 -> 0.12.12` upgrade with CLI smoke skipped |
| container image | current-tag partial pass | `v0.12.12` evidence proves explicit amd64 pull, version, and worker process/metrics initialization before the no-config smoke exits on missing local NATS; the same evidence records that the tag lacks a native `linux/arm64/v8` manifest. The release workflow is configured for amd64 + arm64 publication on the next cut, but current-tag arm64 proof remains open until a new registry packet is archived |
| Nix install | current-tag pass | `v0.12.12` fresh install proved from the tagged flake into a temporary Darwin profile on `neo`; current evidence is `docs/release/evidence/distribution-v01212-20260508T205913Z/` |

Canonical runbook: [Distribution Smoke Matrix](distribution-smoke-matrix.md).
Install-to-first-use bridge:
[Packaged Install To First-Real-Use Acceptance](packaged-install-first-use.md).
Historical per-release evidence freeze for `v0.12.2`:
[v0.12.2 Evidence Matrix](../release/v0.12.2-evidence-matrix.md).
Current Homebrew/Nix distribution evidence for `v0.12.12`:
[distribution-v01212-20260508T205913Z](../release/evidence/distribution-v01212-20260508T205913Z/).
Current evidence index with GitHub Actions run links:
[Release Evidence Index](../release/evidence/README.md).

### 2. Continuous CI / Build Proof

Current CI proves:

- Rust workspace compile, format, clippy, and test suite
- sync feature tests in `tcfs-sync`
- wireup tests in `tcfs-e2e`
- Nix flake evaluation/build surfaces
- macOS FileProvider staticlib/header integration
- iOS simulator-oriented Swift type-check/build surface

Current CI does **not** prove:

- production Finder/FileProvider install-to-enable-to-conflict/status UX
- real iOS Files.app behavior on simulator or device
- macOS FileProvider Swift bundle build in the regular CI workflow
- Helm/Kubernetes rollout, OpenTofu apply, live NATS/SeaweedFS health, or
  worker deployment semantics
- accessibility behavior
- user-facing badges, progress, notifications, or recovery ergonomics

Primary workflow: `.github/workflows/ci.yml`.

### 3. Live Usage Proof

`tummycrypt` does now have real non-CI usage lanes. Treat them as live/manual
acceptance unless the specific run has a dated repo-archived transcript:

- [Neo-Honey Live Acceptance](neo-honey-acceptance.md): canonical live backend and two-device sync smoke
- [Lab Host Acceptance Matrix](lab-host-acceptance-matrix.md): real host acceptance using `honey`, `neo`, and `petting-zoo-mini`

That means the project is no longer relying only on unit tests and packaging.
Real operator flows and real-host sync flows are being exercised.

What this still does **not** mean:

- full desktop UX is release-proven
- production Finder/FileProvider mutation/conflict UX is continuously tested
- iOS is a truthful active release target
- every host/platform combination is equally mature

## What Is Still Only Buildable Or Manual

### Apple Desktop UX

macOS is no longer “missing” as a code path, but it is still not a release-grade
desktop surface in the same sense Linux is.

Now proven in the non-production PZM lab:

- a `v0.12.11` testing-mode package on `petting-zoo-mini`
- package install, signing/profile checks, shared-Keychain config, live S3/E2EE
  access, daemon startup, FileProvider registration, CloudStorage enumeration,
  host-app `requestDownload`, and exact-content hydration
- a `v0.12.12` testing-mode package with the installed
  `TCFS FileProvider Lab Gatekeeper Rules` profile
- smoke run `25562087555`: installed host policy probe, shared-Keychain config,
  E2EE, daemon startup, FileProvider registration, CloudStorage enumeration,
  `requestDownload`, `evict`, re-`requestDownload`, and exact 55-byte hydration

Resolved lab runtime-policy blocker:

- `v0.12.12` package/signing/storage/daemon stages pass on PZM
- the current Mac App Development certificate/profile pair is valid: codesign
  verification passes for the host and extension, embedded profiles decode, and
  `taskgated-helper` allows both host and extension entitlements
- package run `25456290021` proves the build-output host app reaches Swift
  `main()` in policy-probe mode and exits 0 after logging `main entered`,
  `domain created`, and `policyProbe: OK`, despite Gatekeeper assessment
  rejection
- `spctl` rejects both bundles, and `syspolicy_check` reports the installed app
  is not distribution-ready because it has no notarization ticket
- `syspolicy_check notary-submission` also reports a fatal Gatekeeper rejection
  for `TCFSProvider.app/Contents/MacOS/TCFSProvider`
- postinstall smoke run `25456341985` shows the installed host-app policy probe
  times out after 15s with no instrumented stderr and a sample stuck at
  `_dyld_start`; the later harness host launch also emits no instrumented
  stderr, then
  AppleSystemPolicy denies the host process
- `fileproviderd` launches the extension process, then AppleSystemPolicy also
  terminates the extension before the evict/rehydrate lifecycle can complete
- smoke run `25458526158` showed macOS 15 rejects `spctl --add` rule mutation
  with exit 4, so the repo moved to a managed `SystemPolicyRule` profile
- smoke run `25562087555` verifies that profile and passes the FileProvider
  evict/rehydrate harness. This proves only the non-production lab path;
  production Finder still needs separate Developer ID clean-host evidence.

Still manual or weakly proven:

- production Finder/FileProvider enablement on arbitrary clean machines
- badges, progress, notifications, and conflict UX
- production mutation, conflict, and realistic desktop usage beyond the PZM
  testing-mode lab. PZM smoke run `25565943781` proves CloudStorage mutation
  upload and exact 68-byte remote pull; PZM smoke run `25569596910` proves
  CLI `sync state: conflict` and exact FileProvider content preservation under
  testing mode, not production Developer ID Finder behavior.

Canonical docs:

- [Apple Surface Status](apple-surface-status.md)
- [macOS Finder and FileProvider Reality](macos-fileprovider-reality.md)
- [TCFS Usage Reality Sprint Plan](usage-reality-sprint-plan-2026-05-06.md)

### Lazy Traversal And Hydration Demo

The filesystem implementation can list remote index entries and hydrate content
on open, and the repo now has named Linux, mounted-view, Desktop-to-honey, and
Finder/FileProvider harnesses. The archived Linux FUSE run
`docs/release/evidence/lazy-linux-20260508T170825Z/` proves the expanded
lifecycle against real remote state: traverse/list before hydration, exact
`cat` hydration, mounted write/readback, cache clear/rehydrate, dirty-child
recursive `unsync` refusal, clean recursive conversion to `.tc` stubs, and
persisted `NotSynced` state. The PZM
testing-mode FileProvider lane now proves mutation upload/readback under run
`25565943781` and deterministic conflict/status content preservation under run
`25569596910`. The canonical acceptance target is now
[Lazy Hydration Demo Acceptance](lazy-hydration-demo.md).

The representation contract for that demo is:

- mounted VFS/FUSE/NFS surfaces show clean filenames and hydrate on open
- physical `.tc`/`.tcf` files are the offline/dehydrated sync-root format
- macOS Finder uses FileProvider placeholders / APFS dataless files, not raw
  `.tc` suffixes as the primary UX

### iOS

iOS remains proof-of-concept in practice:

- no continuously exercised simulator or device acceptance lane
- no repeatable TestFlight/App Store delivery proof
- no truthful end-user UX claim beyond experimental FileProvider direction

Canonical doc: [iOS Surface Status](ios-surface-status.md).

### Accessibility (AX)

There is currently no named accessibility acceptance lane.

Not presently proven:

- keyboard-only desktop flows
- screen-reader behavior
- contrast/readability audits
- accessible error/recovery UX on Apple surfaces

This is a real backlog area, not just missing wording.

## DX / UX Reality

### DX

Developer and operator experience is in decent shape for maintainers:

- release smoke is documented
- live backend smoke is documented
- host-backed acceptance has a real matrix
- on-prem authority recovery has a runbook and deploy scripts

The rough edges are environment-sensitive proving lanes:

- Nix proof is per-tag and environment-sensitive; keep using the distribution
  smoke matrix and capture the profile/build context for each release
- privileged on-host cluster work is still outside repo automation
- Apple desktop proof is still too manual

### UX

Today’s strongest user story is:

1. install on Linux or Homebrew/macOS
2. start the daemon
3. push/pull/sync files
4. run multi-device sync against the live backend

Today’s weakest user story is:

1. fresh Apple desktop install
2. register FileProvider
3. browse placeholders in Finder
4. hydrate, mutate, conflict, recover
5. trust visible badges/progress and system-level behavior

## Prioritized Backlog

If the goal is better product reality rather than more code surface, the next
work should be ordered like this:

1. **Linux lazy traversal and hydration evidence**
   - expanded lifecycle is green in
     `docs/release/evidence/lazy-linux-20260508T170825Z/`
   - remaining Linux work is product polish around conflict/status surfacing,
     not the lifecycle proof packet itself
2. **Apple desktop acceptance**
   - stop retrying hosted production packages until FileProvider can be enabled
   - treat PZM testing-mode read/hydrate/evict/rehydrate as green under the
     installed lab `SystemPolicyRule` profile
   - treat PZM testing-mode mutation as green under smoke run `25565943781`
   - treat PZM testing-mode conflict/status content preservation as green under
     smoke run `25569596910`
   - keep the installed-host policy probe and profile verification in the PZM
     postinstall workflow so install/provenance failures stay classified before
     deeper Finder assertions
   - keep Finder badges/progress as observational evidence until there is a
     reliable assertion for those UI signals
   - keep production Developer ID clean-host Finder acceptance separate from
     non-production testing-mode evidence
3. **odrive-style lifecycle productionization**
   - surface `FileSyncStatus`, progress, and conflict state in CLI/TUI/Finder
   - prove `PathLocks`, dirty-child unsync, auto-unsync, and blacklist behavior
     with acceptance tests, not just unit tests
   - add folder policy CLI/desktop controls and status reporting
   - keep arbitrary-folder sync separate from one-way backup semantics
4. **Desktop-originated cross-host demo**
   - use `~/Desktop/TCFS Demo`, not the daily-driver `~/Desktop`, as the first
     arbitrary-folder sync proof
   - mount the same remote prefix on `honey` under an explicit disposable path
     such as `~/tcfs-demo/Desktop`
   - prove `find`/`ls` pre-hydration and `cat` hydration over SSH without
     claiming macOS Finder Desktop and honey home directories are the same
5. **Release support truth**
   - keep Homebrew and Nix proof current in the distribution matrix
   - Debian 12 posture is decided for the current packages: do not claim it as
     a supported `.deb` target until a bookworm-specific build exists
6. **Credential hygiene**
   - rotate and migrate legacy plaintext Ansible inventory credentials into the
     SOPS-managed path before claiming repo-wide secret handling is complete
7. **Accessibility**
   - define an explicit AX bar before claiming mature desktop UX
8. **Diagnostics and recovery UX**
   - on-demand diagnostic dump
   - clearer support/recovery flows for operator and end-user failures

## Open Issue Map

As of May 9, 2026, the narrow GitHub backlog snapshot is below. Verify live
GitHub state before acting on exact issue or milestone status.

- M10 release-proof tranche
  - `#280`: distribution install and upgrade proof umbrella. Homebrew/Nix
    current-tag proof is archived for `v0.12.12`; Linux `.deb`/`.rpm` package
    proof is archived; container amd64 pull/version/startup proof is archived,
    and release workflow readiness for native arm64 is merged, but current-tag
    native arm64 registry proof remains open; production macOS `.pkg`
    current-tag proof remains a named follow-up.
  - `#309`: macOS `.pkg` clean-host and FileProvider acceptance lane. PZM
    testing-mode enumerate/hydrate/evict/rehydrate/mutation/conflict-status is
    green under the installed lab `SystemPolicyRule` profile; production Finder
    lifecycle proof remains open.
- Adjacent non-M10 lanes
  - `#298`: residual Civo TCFS PVC retirement after on-prem recovery
  - `#327`: TCFS on-prem OpenTofu migration and cutover
  - `#312`: tinyland branch-tranche triage. PR #351 recorded a concrete
    non-destructive prune proposal; the remaining decision is operator
    approve/defer for Tranche A.

Closed during the May 9 branch-hygiene pass:

- `#313`: yoga retirement decision. The recorded decision is
  documentation-only retirement; no archive, host deletion, key revocation,
  local remote removal, or branch deletion was performed.

Milestone `#9 M10: Usage Reality & Product Parity` remains open because the
release-proof tranche still has active issues beyond the umbrella. The earlier
M10 GitHub issues (`#276`-`#279`, `#281`, `#307`, `#308`, `#317`, `#318`) are
already closed.

Open review surfaces should be checked in GitHub. The on-prem render/apply work
from `#337` was merged on April 29, 2026 and now feeds `#327` rather than
representing an open review surface.

The default lazy traversal demo backend is now a disposable, run-scoped
S3-compatible prefix. The on-prem authority remains separate until its
downtime-gated migration is complete or a private-runner evidence lane is
chosen deliberately.

The broader product backlog still lives mostly in the parity and acceptance
docs rather than in a large live GitHub issue set.

## odrive Parity Horizon

Public odrive parity should be treated as a user-behavior target: visible
remote trees before download, hydrate on open, unsync/free-space safely,
folder-level sync policy, desktop status/progress, scriptable CLI/headless
agent behavior, and arbitrary-folder sync/backup workflows. TCFS should not
copy odrive's legacy placeholder-extension architecture. Mounted views should
keep clean filenames; physical `.tc` / `.tcf` files remain sync-root/offline
representations; FileProvider uses platform placeholders.

The current parity summary and Desktop/honey demo contract live in
[odrive Parity and Product Horizon](odrive-parity-product-horizon.md).

## Linear Mirror State

As of May 9, 2026, Linear is a useful management mirror but is not the
freshest truth source for `tummycrypt`.

- `TIN-133` is `In Progress`, is titled `Prove lazy traversal and
  Finder/FileProvider hydration reality`, and points at GitHub `#309` plus the
  current repo docs. The latest comments mirror the `v0.12.12` PZM
  testing-mode lifecycle success and the remaining production Finder lifecycle
  gaps.
- `TIN-131` and `TIN-132` remain in Backlog under `Tummycrypt M10: Usage
  Reality & Product Parity`; their descriptions were refreshed on April 29,
  2026 to separate current repo truth from the older GitHub issue framing they
  originally mirrored.
- `TIN-134` and `TIN-135` were moved to Done on April 29, 2026 as
  completed/superseded mirrors.
- Infrastructure Linear items such as `TIN-615` and `TIN-720` are relevant to
  on-prem storage and tailnet posture, but they should not be treated as blockers
  for a lazy hydration demo unless that demo explicitly depends on the on-prem
  backend.

Linear hygiene decision on April 29, 2026:

- keep `TIN-131` open as the active Linear mirror for GitHub `#280` and
  distribution install/upgrade proof
- keep `TIN-132` open as the live backend / neo-honey acceptance mirror
- close `TIN-134` as completed/superseded by the iOS and Apple status docs
- close `TIN-135` as completed/superseded by the refreshed product reality and
  lazy hydration demo docs

## Related Documents

- [Distribution Smoke Matrix](distribution-smoke-matrix.md)
- [Packaged Install To First-Real-Use Acceptance](packaged-install-first-use.md)
- [v0.12.2 Evidence Matrix](../release/v0.12.2-evidence-matrix.md)
- [Remote Governance](remote-governance.md)
- [Lab Host Acceptance Matrix](lab-host-acceptance-matrix.md)
- [Neo-Honey Live Acceptance](neo-honey-acceptance.md)
- [Lazy Hydration Demo Acceptance](lazy-hydration-demo.md)
- [odrive Parity and Product Horizon](odrive-parity-product-horizon.md)
- [Apple Surface Status](apple-surface-status.md)
- [macOS Finder and FileProvider Reality](macos-fileprovider-reality.md)
- [iOS Surface Status](ios-surface-status.md)
- [TCFS Workstream Reality Check - 2026-05-09](workstream-reality-check-2026-05-09.md)
- [Feature Parity Gap Analysis](../../odrive-re/docs/feature-parity-gap-analysis.md)
