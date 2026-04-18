# Packaged Install To First-Real-Use Acceptance

This runbook defines the bridge between:

- [Distribution Smoke Matrix](distribution-smoke-matrix.md), which proves that a
  shipped artifact installs and starts, and
- host-backed acceptance lanes, which prove richer operator and end-user-ish
  behavior on real machines.

The purpose of this document is to answer a narrower question:

Can a released artifact get a user to the first truthful use of `tcfs`, rather
than only to a runnable binary in a temp home?

## Gate Decision

For `v0.12.x`, packaged-install to first-real-use proof is:

- **required every tag** on the primary mutable user-facing install surfaces:
  - Homebrew
  - macOS `.pkg`
  - Debian/Ubuntu `.deb`
- **sampled or scenario-driven** on the narrower surfaces:
  - Fedora/RHEL `.rpm` (daemon-only today)
  - container image
  - Nix

This is intentionally stricter than installed-binary smoke, but narrower than
full host acceptance or desktop UX parity.

## Minimum Contract

Treat a surface as having reached first-real-use proof only when the released
artifact proves all of the following:

1. **Artifact install is real**
   - install from the published artifact for that surface
   - do not swap in local build outputs
2. **Runtime config is intentional**
   - use a real config, credentials, or acceptance fixture rather than only the
     isolated temp-home default used by `scripts/install-smoke.sh`
3. **Backend reachability is visible**
   - when the CLI is present, `tcfs status` should report `storage [ok]`
   - if the surface is daemon-only, use the nearest truthful equivalent
4. **One real action succeeds**
   - examples: push/pull, hydrate, enumerate, sync-status against real remote
     state, or worker startup against real/disposable backend dependencies
5. **One edge case or power-user action is exercised on primary mutable surfaces**
   - examples: unsync/rehydrate, conflict recovery, symlink handling, large-file
     path, or a restart/reconnect path

Passing `scripts/install-smoke.sh` alone is not sufficient to claim this bar.

## Surface Contract

| Surface | Per-tag requirement | First real action | Edge case / follow-on |
|---------|---------------------|-------------------|-----------------------|
| Homebrew | every tag | `tcfs status` with `storage [ok]`, then a minimal push/pull or sync-status path | one of: unsync/rehydrate, conflict, symlink, large file |
| macOS `.pkg` | every tag | package install, then the named Finder/FileProvider lane from [macOS Finder and FileProvider Reality](macos-fileprovider-reality.md) through enumerate + hydrate | clean-host install and one desktop follow-on such as mutate/conflict or unsync/rehydrate |
| Debian/Ubuntu `.deb` | every tag | `tcfs status` with `storage [ok]`, then minimal push/pull or sync-status | one of: upgrade carry-forward, unsync/rehydrate, conflict, large file |
| Fedora/RHEL `.rpm` | sampled | daemon/worker startup against intentional config | worker restart or backend reconnect as needed |
| Container image | sampled | worker startup against real or disposable backend dependencies | restart/reconnect or rollout-oriented check |
| Nix | sampled until `#307` is resolved | same bar as CLI surfaces once cache/install is truthful | use the same edge-case menu as `.deb` once the install path is stable |

## Relationship To Existing Runbooks

- Use [Distribution Smoke Matrix](distribution-smoke-matrix.md) to prove the
  published artifact installs and starts.
- Use this document to prove the released artifact reaches a truthful first use.
- Use [Lab Host Acceptance Matrix](lab-host-acceptance-matrix.md) for richer
  real-host acceptance beyond the first user action.
- Use [Neo-Honey Live Acceptance](neo-honey-acceptance.md) for the named
  live-backend sync lane.
- Use [macOS Finder and FileProvider Reality](macos-fileprovider-reality.md) for
  the Apple desktop path after package install.
- Use [`scripts/macos-postinstall-smoke.sh`](../../scripts/macos-postinstall-smoke.sh)
  as the current named harness for the macOS package-to-FileProvider lane.

## Evidence Capture

Record results using a table like this:

| Surface | Installed artifact | `storage [ok]` or equivalent | First real action | Edge case | Notes |
|---------|--------------------|------------------------------|-------------------|-----------|-------|
| Homebrew | pass/fail | pass/fail | pass/fail | pass/fail | |
| macOS `.pkg` | pass/fail | pass/fail | pass/fail | pass/fail | |
| Debian/Ubuntu `.deb` | pass/fail | pass/fail | pass/fail | pass/fail | |
| Fedora/RHEL `.rpm` | pass/fail | n/a or pass/fail | pass/fail | sampled | |
| Container image | pass/fail | equivalent only | pass/fail | sampled | |
| Nix | pass/fail | pass/fail | pass/fail | sampled | |

## Current Scope Notes

- `install.sh` is published convenience tooling, not part of the canonical
  release-proof surface. See [Distribution Smoke Matrix](distribution-smoke-matrix.md).
- The macOS clean-host lane remains tracked in `#309`; the harness now exists,
  and the repo now carries a manual GitHub-hosted approximation in
  [`.github/workflows/macos-postinstall-smoke.yml`](../../.github/workflows/macos-postinstall-smoke.yml),
  but live backend secrets still need to exist and at least one tagged run
  still needs to pass before that executor can be treated as proven.
- The Nix install path remains blocked on `#307`.
