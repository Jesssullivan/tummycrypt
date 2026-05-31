# Product Reality And Priority

As of May 19, 2026, `tummycrypt` is in a much better state operationally than
its remaining gaps might suggest.

The latest release candidate is `v0.12.13-rc4`, and most release-facing
surfaces now have explicit proof paths. The important distinction is that
`buildable`, `packaged`, `product-surface proven`, and `actually ready for
daily-driver use` are still different things.

Use this document as the short answer to:

- what is actually proven today
- what is still only buildable or manually explorable
- what should be prioritized next

Current execution todo:
[TCFS Daily Driver Productionization Todo - 2026-05-24](tcfs-daily-driver-productionization-todo-2026-05-24.md).

## Current Product Posture

2026-05-25 storage update: run `26417405494` is now the first exact
package-backed multi-GiB restore packet for `TIN-1546`: 30 files and
3,222,239,922 bytes restored from the public HTTPS production-smoke endpoint.
The table below still keeps the broader storage lane open because the same run
needed heavy `502` retry recovery and restored at about 1.14 MB/s; beta still
needs repeated soak/load, retry/noise budgets, and endpoint posture decisions.

| Surface | Current truth | Source of proof |
| --- | --- | --- |
| Linux CLI + daemon | strongest and most routinely proven path; x86_64 FUSE lifecycle has current real-host evidence, while packaged systemd/mount first-use remains a separate gate | CI, release smoke, live host acceptance, archived Linux lifecycle evidence |
| Fleet sync / backend path | materially exercised on real hosts; current live transcripts are archived in the fleet pilot packets | `neo-honey` live acceptance plus lab host matrix and `docs/release/evidence/fleet-pilot-extended-20260509T2152Z/` |
| Lazy traversal / hydration | core code and harnesses exist; Linux FUSE proves browse-before-download, exact `cat` hydration, mounted write/readback, cache clear/rehydrate, and recursive safe-unsync refusal/success on real host evidence; the extended fleet packet carries that lifecycle proof as a honey companion next to isolated `Documents`/`git` traversal and live backend smoke; PZM now proves production Developer ID FileProvider enumerate, exact-content hydrate, evict/rehydrate, mutation upload/readback, and conflict-status preservation without `fileprovider_testing_mode=true` | `tcfs-vfs`/FUSE implementation, archived Linux and fleet evidence, PZM production Dev ID smoke, and the lazy hydration demo runbook |
| Real project-tree canary / storage posture | scoped isolated `linux-xr` shadow parity is green, including symlink target preservation through honey-mounted traversal. The current release-binary storage-posture packet completed the 7.7 GB shadow, then reused that prefix for honey mounted `find -maxdepth 8`, exact `.clang-format` hydration, and all 85 mounted symlink target checks. The mounted warning follow-up is closed: the exact `.tc` filename fix rerun dropped S3 `NoSuchKey` warnings from 274 to 0 while preserving real ftrace `.tc` filenames. The lifecycle companion now reuses that same prefix and reports `scoped-project-tree-parity-evidence-complete`, including mounted write/readback, cache clear/rehydrate, dirty recursive safe-unsync refusal, and clean recursive safe-unsync success. The small real-repo dogfood surface is green for both source-built and explicit current Nix flake package binaries: `git-repo-canary-oauth-mux-sourcebin-fresh-20260515T014640Z/` and `git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/` both prove clean `oauth-mux` shadow push, 0 skipped symlinks, honey mounted traversal/hydration, 9 mounted symlink target checks, and Linux lifecycle. The original Nix restore timeout remains archived at `git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/restore-proof/`; source-built `restore-proof-source-fix-empty-dirs-20260515T183805Z/` and rebuilt Nix package `restore-proof-nixpkg-current-empty-dirs-20260515T200359Z/` now prove fresh-tree restore for 4,601 regular files, 9 symlinks, synced state for all 4,610 restored paths, and all 12 empty directories with `--require-empty-dirs`. The larger `linux-xr-fast` source-built packet is green for shadow push, honey mounted traversal/hydration, and Linux lifecycle in `git-repo-canary-linux-xr-fast-sourcefix-index-20260516T045054Z/`: the clean shadow has 2,038 regular files, 0 symlinks, about 8.7 GB of file bytes, and the Git pack-index, temp-pack, and exact `.git/index` chunk-profile fixes are proven in one run. Fresh-tree restore remains blocked because 2 of 2,038 regular files did not restore; both are multi-GB raw Git pack files that failed after transient chunk read errors, while all 6 empty directories restored exactly. Homebrew current-tap install is now green for rc4, but Homebrew remains a package-backed real-repo blocker until the symlink/large-canary path is repeated with the current formula: `tcfs-symlink-package-probe-20260515T041947Z/` showed current-checkout Nix and source-built `tcfs 0.12.12` preserving symlinks while installed Homebrew `tcfs 0.12.12` skipped them; `tcfs-symlink-package-probe-20260515T051126Z/` proves neo current-checkout Nix can publish a symlink index that honey current source-built Linux can mount, read, and verify as `link.txt -> target.txt`; `tcfs-symlink-package-probe-20260515T060330Z/` proves current Nix flake packages on neo and honey pass the same tiny mounted parse/target check. The production storage posture gate now has a current scoped HTTPS canary: run `26246264661` on `main@43ce227` proved public HTTPS, `enforce_tls=true`, public CA trust, allowed-prefix list/write/read/delete/delete-verify, and denied-prefix `PermissionDenied`. Large-restore throughput, socket/highwater behavior, transient recovery classification, enough local free space for full restore, and package-backed fresh-tree restore/rollback proof remain open before live repo moves. | `docs/release/evidence/git-repo-canary-linux-xr-fast-sourcefix-index-20260516T045054Z/`, `docs/release/evidence/git-repo-canary-linux-xr-fast-sourcefix-tmppack-20260516T024810Z/`, `docs/release/evidence/git-repo-canary-linux-xr-fast-sourcefix-20260516T024122Z/`, `docs/release/evidence/git-repo-canary-linux-xr-fast-nixpkg-tuned-20260516T010911Z/`, `docs/release/evidence/git-repo-canary-linux-xr-fast-nixpkg-20260516T005236Z/`, `docs/release/evidence/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/restore-proof-nixpkg-current-empty-dirs-20260515T200359Z/`, `docs/release/evidence/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/restore-proof-source-fix-empty-dirs-20260515T183805Z/`, `docs/release/evidence/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/restore-proof-source-fix-symlink-state-20260515T171712Z/`, `docs/release/evidence/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/restore-proof/`, `docs/release/evidence/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/`, `docs/release/evidence/tcfs-symlink-package-probe-20260515T060330Z/`, `docs/release/evidence/tcfs-symlink-package-probe-20260515T051126Z/`, `docs/release/evidence/tcfs-symlink-package-probe-20260515T041947Z/`, `docs/release/evidence/git-repo-canary-oauth-mux-sourcebin-fresh-20260515T014640Z/`, `docs/release/evidence/home-canary-linux-xr-storage-posture-lifecycle-20260514T213826Z/`, `docs/release/evidence/home-canary-linux-xr-shadow-20260511T040325Z/`, `docs/release/evidence/tcfs-symlink-config-probe-20260515T005858Z/`, `docs/release/evidence/git-repo-canary-oauth-mux-20260515T000411Z/`, `docs/ops/git-repo-canary-dogfood.md`, `docs/release/evidence/home-canary-linux-xr-storage-posture-tc-extfix-20260514T202343Z/`, `docs/release/evidence/home-canary-linux-xr-storage-posture-20260514T021513Z/`, `docs/release/evidence/home-canary-linux-xr-storage-posture-20260513T220442Z/`, PR `#367`, and blocker packet `docs/release/evidence/home-canary-linux-xr-storage-posture-20260513T174944Z/` |
| macOS | experimental but real; `v0.12.13-rc4` ships a notarized production Developer ID `.pkg`. PZM run `26218940950` proves the package lane (install, notarization, strict signing/preflight) and the installed-host FileProvider lane (storage `[ok]`, domain rebuild, signed HostApp user-visible root enumeration, exact hydrate, evict/rehydrate, mutation upload/readback, rename, and conflict-status preservation) without `fileprovider_testing_mode=true`. Remaining macOS production gaps are first-run setup from installer to valid config/status, badge/progress assertions, recovery UX, longer desktop soak, and continuous release-day viability | build + packaging + PZM production Dev ID smoke + v0.12.13 evidence matrix + local desktop evidence |
| iOS | proof-of-concept | Swift type-check and scaffold only |
| Windows | planned / skeleton | code exists, but there is no release-grade CLI, daemon, or Explorer flow |

## What Is Actually Proven

### 1. Release Artifact Proof

This is the narrowest and most important truth for public release claims.

| Surface | Status | Current reality |
| --- | --- | --- |
| Homebrew | current tap fresh-install and upgrade pass | `homebrew-tap@b5877df` points at `v0.12.13-rc4`; run `26221252765` proves fresh install and run `26221711601` proves upgrade from the prior `v0.12.12` tap ref to rc4 with installed `tcfs 0.12.13` / `tcfsd 0.12.13` smoke |
| macOS `.pkg` | FileProvider release-asset pass; first-run UX gap | `v0.12.13-rc4` publishes a notarized production Developer ID `.pkg`; PZM run `26218940950` proves the exact public GitHub Release `.pkg` through hydrate, evict/rehydrate, mutation upload/readback, rename, and conflict/status. First-run config UX remains pending |
| `.deb` | rc4 public-asset first-use pass plus Debian/Ubuntu install-upgrade smoke | Ubuntu 24.04+ is proven by run `26218940925` through install, storage `[ok]`, FUSE mount, exact hydrate, `tcfs cache evict` + rehydrate, and mutation remote pull. PR #442 run `26243913292` proves public `v0.12.12` to public `v0.12.13-rc4` package upgrade smoke on Debian 13 and Ubuntu 24.04, plus Debian 13 fresh install smoke |
| `.rpm` | rc4 Fedora 42 daemon-only install and sampled upgrade smoke pass | Fedora 42 x86_64 daemon-only installed-binary smoke and public `v0.12.12` to public `v0.12.13-rc4` RPM upgrade smoke passed in run `26243913292`; the CLI `.rpm` surface is still absent |
| container image | rc4 runtime smoke pass | `v0.12.13-rc4` container runtime smoke run `26218940985` proves manifest inspect, platform pull, version check, and worker startup |
| Nix install | rc4 external profile install pass | run `26242122899` installed `tcfs-cli` and `tcfsd` from `github:Jesssullivan/tummycrypt/v0.12.13-rc4` into a temporary Nix profile on hosted Ubuntu 24.04; binaries reported `tcfs 0.12.13` / `tcfsd 0.12.13` and installed-binary smoke passed |

Canonical runbook: [Distribution Smoke Matrix](distribution-smoke-matrix.md).
Install-to-first-use bridge:
[Packaged Install To First-Real-Use Acceptance](packaged-install-first-use.md).
Historical per-release evidence freeze for `v0.12.2`:
[v0.12.2 Evidence Matrix](../release/v0.12.2-evidence-matrix.md).
Current release-surface evidence for `v0.12.13-rc4`:
[v0.12.13 Evidence Matrix](../release/v0.12.13-evidence-matrix.md).
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

- production Finder/FileProvider first-run setup, badge/progress UX, and
  continuous release-day acceptance
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
- production Finder/FileProvider badge, progress, recovery, and long-running
  daily-use UX are continuously tested
- iOS is a truthful active release target
- every host/platform combination is equally mature

## What Is Still Only Buildable Or Manual

### Apple Desktop UX

macOS is no longer “missing” as a code path, but it is still not a release-grade
desktop surface in the same sense Linux is.

Still useful as non-production PZM lab evidence:

- a `v0.12.11` testing-mode package on `petting-zoo-mini`
- package install, signing/profile checks, shared-Keychain config, live S3/E2EE
  access, daemon startup, FileProvider registration, CloudStorage enumeration,
  host-app `requestDownload`, and exact-content hydration
- a `v0.12.12` testing-mode package with the installed
  `TCFS FileProvider Lab Gatekeeper Rules` profile
- smoke run `25562087555`: installed host policy probe, shared-Keychain config,
  E2EE, daemon startup, FileProvider registration, CloudStorage enumeration,
  `requestDownload`, `evict`, re-`requestDownload`, and exact 55-byte hydration

Now proven on the production Dev ID PZM lane:

- `v0.12.13-rc4` release publication includes a notarized production
  Developer ID macOS `.pkg`.
- production Dev ID smoke run `26061402177` first proved installed strict
  preflight, storage `[ok]`, domain add, CloudStorage enumeration,
  `requestDownload`, and exact-content hydration.
- production Dev ID smoke run `26062554542` then proved the layered M10
  lifecycle on PZM: hydrate, evict/rehydrate, mutation upload/readback, and
  conflict-status preservation without `fileprovider_testing_mode=true`.
- key artifacts from that follow-up are archived under
  `docs/release/evidence/macos-postinstall-prod-devid-hydration-20260518T212705Z/run-26062554542/`.
- public asset smoke run `26218940950` repeated the signed HostApp path against
  `tcfs-0.12.13-rc4-macos-aarch64.pkg` with exact hydrate, evict/rehydrate,
  mutation, rename, and conflict/status enabled.
- the older PZM testing-mode lane remains useful as a lab signal, but it is no
  longer the strongest FileProvider lifecycle evidence.

Still manual or weakly proven:

- first-run setup from installer to a valid user config, unlocked credentials,
  and `tcfs status [ok]`
- merged-workflow release-day repeatability beyond the PR #389 branch proof of
  the exact published GitHub Release `.pkg`
- badges, progress, notifications, recovery UX, and user-facing conflict
  resolution
- long-running daily use across arbitrary clean machines

Canonical docs:

- [Apple Surface Status](apple-surface-status.md)
- [macOS Finder and FileProvider Reality](macos-fileprovider-reality.md)
- [TCFS Usage Reality Sprint Plan](usage-reality-sprint-plan-2026-05-06.md)

### Lazy Traversal And Hydration Demo

The filesystem implementation can list remote index entries and hydrate content
on open, and the repo now has named Linux, mounted-view, Desktop-to-honey, and
Finder/FileProvider harnesses. The extended fleet packet
`docs/release/evidence/fleet-pilot-extended-20260509T2152Z/` proves isolated
`Documents`/`git` seed and honey traversal/hydration, then runs the honey Linux
lifecycle companion for mounted write/readback, cache clear/rehydrate, and
recursive safe-unsync, alongside a live `neo-honey` backend smoke. The archived
Linux FUSE run
`docs/release/evidence/lazy-linux-20260508T170825Z/` proves the expanded
lifecycle against real remote state: traverse/list before hydration, exact
`cat` hydration, mounted write/readback, cache clear/rehydrate, dirty-child
recursive `unsync` refusal, clean recursive conversion to `.tc` stubs, and
persisted `NotSynced` state. The PZM
testing-mode FileProvider lane now proves mutation upload/readback under run
`25565943781` and deterministic conflict/status content preservation under run
`25569596910`. The canonical acceptance target is now
[Lazy Hydration Demo Acceptance](lazy-hydration-demo.md).

The realistic home/project-tree canary is now split into two truths.
Functional isolated project-tree behavior is green in
`docs/release/evidence/home-canary-linux-xr-shadow-20260511T040325Z/`: the
shadowed `linux-xr` tree could be pushed, traversed from honey, hydrate selected
content, preserve all 85 symlink targets through the mounted view, and pass the
Linux lifecycle companion. Scoped HTTPS storage posture is now green for the
small canary gate: run `26246264661` at `main@43ce227` proved public HTTPS,
`enforce_tls=true`, public CA trust, allowed-prefix list/write/read/delete,
delete verification, and denied-prefix `PermissionDenied`. The larger storage
lane remains open for throughput and recovery: `20260513T220442Z` reduced the dominant
6.2 GB raw Git `.pack` from 70,856 chunks to 1,211 chunks, and
`docs/release/evidence/home-canary-linux-xr-storage-posture-20260514T021513Z/`
reduced the adjacent 45.6 MB `.rev` from 8,405 chunks to 8 chunks while
completing the 7.7 GB shadow with no retry or error rows. A follow-up against
that same prefix also passed honey mounted `find -maxdepth 8`, all 85 mounted
symlink target checks, and exact `.clang-format` hydration. The lifecycle
companion `home-canary-linux-xr-storage-posture-lifecycle-20260514T213826Z/`
closes the same-prefix mounted write/readback, cache clear/rehydrate, and
recursive safe-unsync row. Do not claim broad home-directory readiness or
large-object production storage maturity until socket accounting,
candidate-package proof of the Git metadata profile fixes, large-pack restore
reliability, transient-recovery classification, and generated-large-file policy
are closed.

The next generic git-repo dogfood lane is narrower and currently blocked at
Homebrew package truth plus larger/restore proof, not source or current Nix
package behavior. `docs/release/evidence/tcfs-symlink-package-probe-20260515T041947Z/`
shows installed Homebrew `tcfs 0.12.12` skipping a symlink under the same
`sync_symlinks = true` config that source-built and current-checkout Nix
`0.12.12` preserve. `docs/release/evidence/tcfs-symlink-package-probe-20260515T051126Z/`
adds a tiny mounted proof for neo Nix producer to honey source-built consumer,
and `docs/release/evidence/tcfs-symlink-package-probe-20260515T060330Z/` adds
the same tiny proof with current Nix flake packages on both hosts.
`docs/release/evidence/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/`
then proves the small clean repo shadow with explicit current Nix package
binaries on both hosts. `task lazy:git-repo-restore-proof` now archives the
fresh-tree restore gate; the first run against the Nix packet timed out during
remote-index dry-run scanning after 120s, before restore execution. The
source-built follow-up
`restore-proof-source-fix-empty-dirs-20260515T183805Z/` restores 4,601
regular files, 9 symlinks, synced state for all 4,610 restored paths, and all
12 archived empty directories with `--require-empty-dirs`. The latest
`~/git/linux-xr-fast` source-built packet is green for shadow push,
honey-mounted traversal/hydration, and Linux lifecycle, but fresh-tree restore
is blocked on two multi-GB raw Git pack pulls. Rerun
`task lazy:git-repo-canary` for Homebrew only after a rebuilt Homebrew lane is
selected; otherwise the next mobility proof is `~/git/linux-xr-fast` with a
selected candidate binary/package, enough local free space for full restore,
hardened download retry posture, and package-backed restore/rollback proof.
Local symlink `sync-status` / recursive `unsync` semantics are not yet claimed;
the repo-canary symlink bar is remote publication plus mounted target
verification.

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
   - treat production Dev ID FileProvider lifecycle as green on PZM run
     `26062554542`: hydrate, evict/rehydrate, mutation upload/readback, and
     conflict-status preservation without `fileprovider_testing_mode=true`
   - keep the installed-host signing/profile/preflight gates in the PZM
     postinstall workflow so install/provenance failures stay classified before
     deeper Finder assertions
   - keep rerunning the exact published GitHub Release `.pkg` smoke on main so
     release-day viability stays continuous, not one branch proof
   - keep Finder badges/progress as observational evidence until there is a
     reliable assertion for those UI signals
   - move the active Apple work from "can FileProvider hydrate?" to first-run
     setup, badge/progress/recovery UX, release-day repeatability, and longer
     daily-use soak
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

As of May 19, 2026, the narrow GitHub backlog snapshot is below. Verify live
GitHub state before acting on exact issue or milestone status.

- M10 release-proof tranche
  - `#280`: distribution install and upgrade proof umbrella. `v0.12.13-rc4`
    public Linux `.deb`, macOS `.pkg`, Homebrew, Debian 13 install/upgrade,
    Ubuntu 24.04 package upgrade, Fedora 42 daemon-only RPM install/upgrade,
    Nix external profile install, and container runtime smokes are green.
    NixOS host proof and rc package version semantics remain pending.
    Debian 12 remains excluded by the libc/OpenSSL floor unless a separate
    bookworm package is produced.
  - `#309`: macOS `.pkg` clean-host and FileProvider acceptance lane. PZM
    production Dev ID smoke run `26062554542` proves hydrate,
    evict/rehydrate, mutation upload/readback, and conflict-status. Remaining
    Apple follow-ups are first-run setup UX, badge/progress/recovery
    assertions, and long-running daily-use proof.
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

The current alpha/beta daily-driver claim boundary lives in
[TCFS Alpha/Beta QA Readiness](tcfs-alpha-beta-qa-readiness-2026-05-19.md).

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

As of May 19, 2026, Linear is a useful management mirror but is not the
freshest truth source for `tummycrypt`.

- `TIN-133`, `TIN-1414`, and `TIN-1415` are Done for the M10 production
  FileProvider hydration/lifecycle proof.
- `TIN-131` is In Progress under `Tummycrypt M10: Usage Reality & Product
  Parity` for the remaining distribution breadth and upgrade lanes. `TIN-132`
  is Done with a fresh named neo/honey packet, but release-day acceptance should
  keep that transcript current or explicitly supersede the named-lane
  requirement.
- `TIN-1421` is Done for the real-storage CI lane.
- `TIN-1422` and `TIN-1540` are Done for the reachable Linux package-smoke
  backend and hosted Linux first-use lane.
- `TIN-1417`, `TIN-1424`, and `TIN-1418` are the enrollment/security and
  multitenancy chain: per-device identity must land before self-enrollment or
  multitenant trust-boundary claims are product-real.
- `TIN-1546` is the production S3/storage posture gate: scoped HTTPS canary
  evidence is green on current `main`, while transient-error classification,
  large-pack restore, socket/highwater evidence, and storage latency/object-count
  evidence remain open.
- `TIN-1547` is the post-M10 FileProvider product-hardening gate: exact release
  asset smoke and rename/conflict paths are green; badges/progress, recovery UX,
  first-run setup, and longer desktop soak remain open.
- `TIN-1548` keeps iOS as a proof-of-concept until there is a real-device or
  simulator Files.app acceptance lane with safe enrollment posture.
- `TIN-1549` is the beta desktop status/progress/conflict recovery UX gate
  across FileProvider, Linux FUSE, CLI, and TUI surfaces.
- `TIN-1556` is the beta stable-root/broad-directory ownership gate; it blocks
  broad `~/git`, `~/Documents`, dotfile, and home-directory claims.
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
- [TCFS Feature and Objective Matrix - 2026-05-09](feature-objective-matrix-2026-05-09.md)
- [TCFS Large Workdir Onboarding Design - 2026-05-25](large-workdir-onboarding-design-2026-05-25.md)
- [Remote Governance](remote-governance.md)
- [Lab Host Acceptance Matrix](lab-host-acceptance-matrix.md)
- [Neo-Honey Live Acceptance](neo-honey-acceptance.md)
- [Lazy Hydration Demo Acceptance](lazy-hydration-demo.md)
- [odrive Parity and Product Horizon](odrive-parity-product-horizon.md)
- [Apple Surface Status](apple-surface-status.md)
- [macOS Finder and FileProvider Reality](macos-fileprovider-reality.md)
- [iOS Surface Status](ios-surface-status.md)
- [TCFS Workstream Reality Check - 2026-05-09](workstream-reality-check-2026-05-09.md)
- [TCFS Fleet Parity Sprint Plan - 2026-05-09](fleet-parity-sprint-plan-2026-05-09.md)
- [Feature Parity Gap Analysis](../../odrive-re/docs/feature-parity-gap-analysis.md)
