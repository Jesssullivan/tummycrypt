# Product Reality And Priority

As of April 17, 2026, `tummycrypt` is in a much better state operationally than
its remaining gaps might suggest.

The repo is clean, the latest release is `v0.12.2`, and most release-facing
surfaces now have explicit proof paths. The important distinction is that
`buildable`, `packaged`, and `actually proven in user-facing flows` are still
different things.

Use this document as the short answer to:

- what is actually proven today
- what is still only buildable or manually explorable
- what should be prioritized next

## Current Product Posture

| Surface | Current truth | Source of proof |
| --- | --- | --- |
| Linux CLI + daemon | strongest and most routinely proven path | CI, release smoke, live host acceptance |
| Fleet sync / backend path | materially proven on real hosts | `neo-honey` live acceptance plus lab host matrix |
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
| Nix install | blocked | cache/builder path is still not proving cleanly enough to count as release proof |

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

- Nix proof still depends too heavily on cache/builder availability
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

1. **Sync lifecycle correctness and safety**
   - explicit sync state machine
   - per-item locking
   - dirty-child unsync safety
2. **Policy and reconciliation**
   - per-folder sync policies
   - auto-unsync with aging / pressure awareness
   - structured refresh and reconciliation pipeline
   - centralized blacklist / exclusion semantics
3. **Apple desktop acceptance**
   - clean-machine `.pkg` install lane
   - named Finder/FileProvider smoke from install through mutate/conflict
   - make desktop proof more than manual spot-checking
4. **Release support truth**
   - finish Nix proof
   - decide Debian 12 support posture honestly
5. **Accessibility**
   - define an explicit AX bar before claiming mature desktop UX
6. **Diagnostics and recovery UX**
   - on-demand diagnostic dump
   - clearer support/recovery flows for operator and end-user failures

## Open Issue Map

As of April 17, 2026, the narrow GitHub backlog is:

- M10 release-proof tranche
  - `#280`: distribution install and upgrade proof umbrella
  - `#307`: Nix cache externality / operator-host install proof
  - `#308`: Debian 12 `.deb` support-floor decision
  - `#309`: macOS `.pkg` clean-host fresh-install lane
  - `#317`: decide whether published `install.sh` is a supported distribution surface
  - `#318`: define the packaged-install to first-real-use acceptance bar
- Adjacent non-M10 lanes
  - `#298`: privileged on-prem authority/namespace reconcile on `honey`
  - `#312`: tinyland branch-tranche triage
  - `#313`: yoga retirement decision

Milestone `#9 M10: Usage Reality & Product Parity` remains open because the
release-proof tranche now consists of six active issues, not just the umbrella.
The earlier M10 GitHub issues (`#276`-`#279`, `#281`) are already closed.

The broader product backlog still lives mostly in the parity and acceptance
docs rather than in a large live GitHub issue set.

## Related Documents

- [Distribution Smoke Matrix](distribution-smoke-matrix.md)
- [Packaged Install To First-Real-Use Acceptance](packaged-install-first-use.md)
- [v0.12.2 Evidence Matrix](../release/v0.12.2-evidence-matrix.md)
- [Remote Governance](remote-governance.md)
- [Lab Host Acceptance Matrix](lab-host-acceptance-matrix.md)
- [Neo-Honey Live Acceptance](neo-honey-acceptance.md)
- [Apple Surface Status](apple-surface-status.md)
- [macOS Finder and FileProvider Reality](macos-fileprovider-reality.md)
- [iOS Surface Status](ios-surface-status.md)
- [Feature Parity Gap Analysis](../../odrive-re/docs/feature-parity-gap-analysis.md)
