# Product Reality And Priority

As of April 29, 2026, `tummycrypt` is in a much better state operationally than
its remaining gaps might suggest.

The latest release is `v0.12.2`, and most release-facing surfaces now have
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
| Linux CLI + daemon | strongest and most routinely proven path | CI, release smoke, live host acceptance |
| Fleet sync / backend path | materially proven on real hosts | `neo-honey` live acceptance plus lab host matrix |
| Lazy traversal / hydration | core code exists; end-to-end demo proof is still pending | `tcfs-vfs`/FUSE implementation plus the lazy hydration demo runbook |
| macOS | experimental but real | build + packaging + partial release smoke + manual desktop path |
| iOS | proof-of-concept | Swift type-check and scaffold only |
| Windows | partial / CLI-oriented | code exists, but not a release-grade user flow |

## What Is Actually Proven

### 1. Release Artifact Proof

This is the narrowest and most important truth for public release claims.

| Surface | Status | Current reality |
| --- | --- | --- |
| Homebrew | pass | fresh install and upgrade proved on `v0.12.2` |
| macOS `.pkg` | partial pass | upgrade proved on `v0.12.2`; fresh clean-machine install still needs its own lane |
| `.deb` | partial pass | Ubuntu 24.04 fresh install and upgrade proved; Debian 12 is not currently a truthful target for the shipped package deps |
| `.rpm` | pass | fresh install proved in Fedora container |
| container image | pass | version and worker-mode startup proved |
| Nix install | needs per-tag proof | `v0.12.2` evidence was blocked; future proof follows the distribution smoke matrix |

Canonical runbook: [Distribution Smoke Matrix](distribution-smoke-matrix.md).
Install-to-first-use bridge:
[Packaged Install To First-Real-Use Acceptance](packaged-install-first-use.md).
Per-release evidence freeze for `v0.12.2`:
[v0.12.2 Evidence Matrix](../release/v0.12.2-evidence-matrix.md).

### 2. Continuous CI / Build Proof

Current CI proves:

- Rust workspace compile, format, clippy, and test coverage
- sync feature tests in `tcfs-sync`
- wireup tests in `tcfs-e2e`
- Nix flake evaluation/build surfaces
- macOS FileProvider staticlib and Swift header/build integration
- iOS simulator-oriented Swift type-check/build surface

Current CI does **not** prove:

- Finder/FileProvider install-to-enumerate-to-hydrate-to-mutate UX
- real iOS Files.app behavior on simulator or device
- accessibility behavior
- user-facing badges, progress, notifications, or recovery ergonomics

Primary workflow: `.github/workflows/ci.yml`.

### 3. Live Usage Proof

`tummycrypt` does now have real non-CI usage lanes:

- [Neo-Honey Live Acceptance](neo-honey-acceptance.md): canonical live backend and two-device sync smoke
- [Lab Host Acceptance Matrix](lab-host-acceptance-matrix.md): real host acceptance using `honey`, `neo`, and `petting-zoo-mini`

That means the project is no longer relying only on unit tests and packaging.
Real operator flows and real-host sync flows are being exercised.

What this still does **not** mean:

- full desktop UX is release-proven
- Finder/FileProvider mutation UX is continuously tested
- iOS is a truthful active release target
- every host/platform combination is equally mature

## What Is Still Only Buildable Or Manual

### Apple Desktop UX

macOS is no longer “missing” as a code path, but it is still not a release-grade
desktop surface in the same sense Linux is.

Still manual or weakly proven:

- Finder/FileProvider end-to-end desktop flow
- badges, progress, notifications, and conflict UX
- clean-machine `.pkg` install followed by realistic desktop usage

Canonical docs:

- [Apple Surface Status](apple-surface-status.md)
- [macOS Finder and FileProvider Reality](macos-fileprovider-reality.md)

### Lazy Traversal And Hydration Demo

The filesystem implementation can list remote index entries and hydrate content
on open, but the repo still needs a named demo lane that proves the exact user
story: `cd`, `ls`, `cat`, dehydrate/unsync, and rehydrate against real remote
state. The canonical acceptance target is now
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

- Nix proof is per-tag and should follow the distribution smoke matrix rather
  than relying on stale cache-externality tracker state
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

1. **Lazy traversal and hydration demo acceptance**
   - seed a real backend fixture
   - prove `cd`/`ls` before hydration
   - prove `cat` hydrates and returns exact content
   - prove dehydrate/unsync followed by rehydrate
2. **Apple desktop acceptance**
   - clean-machine `.pkg` install lane
   - named Finder/FileProvider smoke from install through mutate/conflict
   - make desktop proof more than manual spot-checking
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
   - finish Nix proof
   - decide Debian 12 support posture honestly
6. **Accessibility**
   - define an explicit AX bar before claiming mature desktop UX
7. **Diagnostics and recovery UX**
   - on-demand diagnostic dump
   - clearer support/recovery flows for operator and end-user failures

## Open Issue Map

As of April 29, 2026, the narrow GitHub backlog is:

- M10 release-proof tranche
  - `#280`: distribution install and upgrade proof umbrella
  - `#308`: Debian 12 `.deb` support-floor decision
  - `#309`: macOS `.pkg` clean-host fresh-install lane
- Adjacent non-M10 lanes
  - `#298`: residual Civo TCFS PVC retirement after on-prem recovery
  - `#327`: TCFS on-prem OpenTofu migration and cutover
  - `#312`: tinyland branch-tranche triage
  - `#313`: yoga retirement decision

Milestone `#9 M10: Usage Reality & Product Parity` remains open because the
release-proof tranche still has active issues beyond the umbrella. The earlier
M10 GitHub issues (`#276`-`#279`, `#281`, `#307`, `#317`, `#318`) are already
closed.

There are currently no open pull requests on `Jesssullivan/tummycrypt`. The
on-prem render/apply work from `#337` was merged on April 29, 2026 and now feeds
`#327` rather than representing an open review surface.

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

As of April 29, 2026, Linear is a useful management mirror but is not the
freshest truth source for `tummycrypt`.

- `TIN-133` has been retitled to `Prove lazy traversal and Finder/FileProvider
  hydration reality` and now points at GitHub `#309` plus the current repo docs.
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
- [Feature Parity Gap Analysis](../../odrive-re/docs/feature-parity-gap-analysis.md)
