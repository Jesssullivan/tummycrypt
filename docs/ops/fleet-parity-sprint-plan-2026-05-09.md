# TCFS Fleet Parity Sprint Plan - 2026-05-09

This sprint follows the usage-reality proof packet. Its job is to move from
separate lane proofs to one grounded "work from any machine" acceptance packet
for TCFS, without overstating Finder, on-prem, or release-artifact readiness.

Current local base at planning time:

- repo: `Jesssullivan/tummycrypt`
- branch: `main`
- commit: `17569d445c203071370fbffb9cb56f5ce9c55628`
- open PRs: none
- open GitHub issues: `#280`, `#298`, `#309`, `#312`, `#327`
- closed prerequisite: `#308`

Planning-pass validation:

- GitHub API confirmed the same five open issues and no open PRs.
- Linear comments for `TIN-131`, `TIN-133`, and `TIN-720` were checked against
  the repo docs and current issue boundaries.
- `task docs:links` passed: 241 links checked, 0 errors.
- `task lazy:check` passed after adding the fleet-pilot helper.
- `cargo test -p tcfs-cli cli_unsync` passed: 6 tests.
- `cargo test -p tcfs-vfs --test vfs_lifecycle_test` passed: 19 tests.
- `docs/release/evidence/fleet-pilot-20260509T1919Z/` was archived after a live
  run: seed to disposable SeaweedFS prefix, honey mounted traversal/hydration,
  and live `neo-honey` SeaweedFS/NATS smoke.
- `docs/release/evidence/fleet-pilot-extended-20260509T2152Z/` was archived
  after a live run: seed to disposable SeaweedFS prefix, honey mounted
  traversal/hydration, honey Linux lifecycle companion for mounted
  write/readback, cache clear/rehydrate, recursive safe-unsync refusal/success,
  and live `neo-honey` SeaweedFS/NATS smoke.

## Readiness Answer

Short answer: TCFS is close to an isolated pilot for "browse a tree, hydrate
what I open, edit, unsync, and rehydrate elsewhere" on Linux, but it is not yet
ready to manage real `~/Documents` or `~/git` across arbitrary machines.

| Host/surface | Ready today | Not ready yet |
| --- | --- | --- |
| `honey` Linux mounted surface | Strongest lane. The archived lifecycle proof shows clean-name traversal before hydration, exact `cat` hydration, mounted write/readback, cache clear, exact rehydrate, dirty recursive `tcfs unsync` refusal, clean recursive `.tc` conversion, and persisted `sync state: not_synced`. | Treating packaged Linux install, systemd first-use, and every distro/service-manager path as continuously proven. |
| `neo` Darwin workstation | Useful release-adjacent control host and CLI participant for live backend sync. Local FileProvider smoke has historical source-tree evidence. The `fleet-pilot-extended-20260509T2152Z` packet includes a green live `neo-honey` smoke from this host. | Not a clean production Finder host. Ambient binary/path state can be stale, and local source-tree FileProvider proof does not satisfy `#309`. |
| `petting-zoo-mini` FileProvider lab | Strong non-production Apple lane. Testing-mode package/smoke proof covers enumerate, hydrate, evict, rehydrate, mutation, CLI conflict state, and exact content preservation. | Not production Developer ID clean-host Finder acceptance. Badges/progress remain observational until reliable assertions exist. |
| Live on-prem backend | Usable enough for live smoke via named endpoints, and source-owned migration commands are renderable. | Not source-owned/storage-mobile yet. Do not couple the parity sprint to live OpenTofu mutation unless `#327` has a named downtime window and rollback owner. |

The isolated fleet-pilot evidence bundle now exists at
`docs/release/evidence/fleet-pilot-extended-20260509T2152Z/`. It uses a
disposable remote prefix and pilot directories, not real `~/Documents` or
`~/git`. Do not make real home subtrees the default sync roots until the staged
rollout gates below are archived.

## Product Semantics

Use these names consistently in docs, issues, and release notes:

| Name | Meaning |
| --- | --- |
| `tcfs` | The product and protocol surface: CLI, daemon, sync state, VFS, fleet sync, storage, encryption, mounts, and operator tooling. |
| `tcfs` binary | User CLI for status, push, pull, mount, unsync, and device commands. |
| `tcfsd` | Daemon process for gRPC, Linux FUSE/NFS mount support, NATS fleet sync, metrics, and FileProvider-facing services. |
| `TCFSProvider.app` | macOS host app. It provisions shared config, registers the FileProvider domain, and can request download/eviction. It is not the whole TCFS product and not the daemon. |
| `TCFSFileProvider.appex` | macOS/iOS FileProvider extension process used by Finder/CloudStorage or Files.app for enumerate, hydrate, and mutation hooks. |
| Linux mounted view | Clean filenames from remote index plus local cache hydration. Users should not see `.tc` names here. |
| Physical sync root | Real files plus `.tc`/`.tcf` stubs for dehydrated content. This is the CLI/offline representation, not the Finder representation. |
| macOS CloudStorage root | Finder placeholders/APFS dataless files managed by FileProvider. Raw `.tc` stubs are not the intended Finder UX. |

This distinction matters for the home-directory goal: a Linux FUSE mount,
physical sync-root stubs, and macOS FileProvider placeholders are three product
representations of the same remote tree, not three unrelated products.

## Sprint Goal

Produce one repo-archived parity packet that proves an isolated project tree can
move between `neo`, `honey`, and the Apple lab without forcing full hydration.

Minimum acceptable packet:

1. Seed an isolated tree from one host into a disposable remote prefix.
2. Browse the tree on another host without hydrating all file bodies.
3. Hydrate exact selected content on demand.
4. Edit through the mounted or provider-backed view and prove exact remote
   pullback.
5. Dehydrate/unsync clean descendants and refuse dirty descendants unless
   `--force` is used.
6. Rehydrate exact content after cache clear or placeholder eviction.
7. Record CLI/daemon/FileProvider status where each surface can currently
   report it.
8. Archive transcript, config, remote prefix, host names, run IDs, and redacted
   metadata under `docs/release/evidence/`.

## Work Packets

| Packet | Tracker | Work | Acceptance |
| --- | --- | --- | --- |
| A. Fleet pilot packet | `TIN-133`, `#309` adjacent | Create a cross-host evidence lane from isolated `neo` or `honey` pilot roots, not real home directories. Reuse `task lazy:fleet-pilot-plan`, the helper's `--run-linux-lifecycle` companion, `task lazy:linux-lifecycle-demo`, `just neo-honey-smoke`, and lab host acceptance docs. | One archived bundle shows traversal, hydrate, edit, unsync, rehydrate, and exact content across at least `neo` and `honey`; PZM can be included as Apple lab proof but does not replace production Finder. |
| B. Safe-unsync hardening | `TIN-133`, code | Keep recursive `tcfs unsync <directory>` behavior product-grade: clean descendants convert, dirty descendants refuse, `--force` preserves tracked remote metadata, state flips to `NotSynced` before destructive file/stub operations. | `cargo test -p tcfs-cli cli_unsync`, `tcfs-vfs` lifecycle tests, daemon RPC unsync tests, and host transcript stay green. |
| C. Production Finder lane | `TIN-133`, `#309` | Select a true production Developer ID clean-host executor and run the published `.pkg` path through app install, host launch, domain add, CloudStorage enumeration, hydrate, mutate/conflict if reliable, and log capture. | `#309` gets one tagged production clean-host run. PZM testing-mode remains regression evidence only. |
| D. Distribution proof closure | `TIN-131`, `#280` | Keep `v0.12.12` proof boundaries explicit and finish next-tag native `linux/arm64/v8` GHCR proof. Tie macOS `.pkg` closure to Packet C. | `#280` can narrow to only future policy decisions after production macOS and native arm64 container proof land. |
| E. On-prem cutover | `TIN-720`, `#327`, `#298` | Keep the parity sprint on disposable prefixes unless a maintenance window is named. If scheduled, run preflight, inventory, plan, retained-PVC migration, candidate workload/service cutover, smoke, and rollback proof. | `#327` only moves with live plan/apply evidence, assigned rollback owner, and post-cut smoke owner. `#298` remains blocked until then. |
| F. Remote branch hygiene | `#312` | Decide whether to approve or defer the 44-branch Tranche A tinyland prune proposal. | No deletion happens without explicit operator approval. Decision is recorded either way. |
| G. iOS posture | Apple docs | Keep iOS as compile/typecheck proof-of-concept unless a real Files.app device lane is scheduled. | CI keeps simulator typecheck green; no public write/FileProvider device claim is added. |

## Home Directory Rollout Gates

Do not jump straight to real `~/Documents` or `~/git`. Use staged gates:

1. Disposable remote prefix plus tiny pilot tree.
2. Isolated `~/TCFS Pilot/Documents` and `~/TCFS Pilot/git` roots.
3. One real but expendable project repo, with `.git` behavior and exclusions
   explicitly checked.
4. Several real project repos, with conflict/status and unsync behavior
   archived on at least two machines.
5. Opt-in subtrees under `~/Documents` or `~/git`.
6. Only then consider broader default management.

Each gate needs an exit transcript and a rollback story. For project repos,
include at least `.git`, hidden files, symlinks if supported, large binaries,
permissions, ignored/build directories, and network-interruption behavior.

## Test Matrix

Run these before claiming a sprint packet is green.

Local and CI:

```bash
task lazy:check
cargo test -p tcfs-cli cli_unsync
cargo test -p tcfs-vfs --test vfs_lifecycle_test
cargo test -p tcfsd unsync
task docs:links
```

Host evidence:

```bash
task lazy:fleet-pilot-plan
TCFS_FLEET_PILOT_RUN_LINUX_LIFECYCLE=1 task lazy:fleet-pilot-plan
task lazy:linux-lifecycle-demo
just neo-honey-smoke
```

Apple lab evidence:

```bash
scripts/macos-fileprovider-testing-mode-dispatch.sh \
  --exercise-conflict-status
```

Production Apple evidence:

- run the release `.pkg` on a true clean Developer ID macOS host
- capture package install, signing/notarization checks, host policy probe,
  FileProvider domain add, CloudStorage enumeration, exact hydrate, and at
  least one desktop follow-on such as mutation or conflict/status if reliable

Distribution evidence:

- Homebrew current tag fresh install and upgrade
- Nix tagged profile install
- Ubuntu 24.04+ and Debian 13+ `.deb` fresh/upgrade proof
- Fedora 42 RPM daemon-only proof unless CLI support changes
- GHCR amd64 and native `linux/arm64/v8` pull/version/startup proof after the
  next multi-arch tag

Kubernetes/on-prem evidence, only if Packet E is scheduled:

- `TCFS_CONTEXT=honey just onprem-preflight`
- `TCFS_CONTEXT=honey just onprem-data-inventory`
- `just onprem-tofu-validate`
- rendered migration plan archived before any mutation
- post-cut `just neo-honey-smoke`

## Tracker Update Plan

| Tracker | Next update should say |
| --- | --- |
| `#280` / `TIN-131` | Current release proof is Homebrew/Nix/Linux packages/amd64 container; remaining blockers are production macOS `.pkg` clean-host Finder and native arm64 container proof on a future tag. No release artifact cut unless explicitly scheduled. |
| `#309` / `TIN-133` | Linux and PZM lab evidence are strong, but production Developer ID clean-host Finder remains open. Link the fleet-pilot packet when archived. |
| `#312` | Record approve/defer for Tranche A branch pruning. Do not delete tinyland branches without explicit approval. |
| `#327` / `TIN-720` | Record whether a downtime window exists. If not, state that parity proof uses disposable prefixes and no live OpenTofu cutover occurred. |
| `#298` | Keep blocked on `#327` unless an operator makes a separate Civo retirement decision. |
| `#308` | No new work; already closed. |

## Definition Of Done

The sprint is done when all of these are true:

1. An archived fleet-parity evidence directory exists under
   `docs/release/evidence/`.
2. The bundle proves clean traversal, exact hydrate, edit, unsync/dehydrate,
   exact rehydrate, and status/conflict visibility at the strongest available
   surface.
3. Production Finder remains accurately labeled: either green via Developer ID
   clean-host proof, or still open in `#309`.
4. Distribution state remains accurate in `#280` and `TIN-131`.
5. On-prem work remains explicitly deferred or has named downtime, rollback,
   and post-cut smoke evidence.
6. Docs link to the evidence bundle from product reality, lazy hydration,
   workstream reality, and the relevant tracker comments.

## Non-Goals

- No automatic takeover of real `~/Documents` or `~/git`.
- No production Finder claim from PZM testing-mode evidence.
- No live OpenTofu cutover without a named window and rollback owner.
- No tinyland branch deletion without explicit operator approval.
- No new release artifact unless release work is explicitly scheduled.

## Related Docs

- [TCFS Feature and Objective Matrix - 2026-05-09](feature-objective-matrix-2026-05-09.md)
- [Product Reality and Priority](product-reality-and-priority.md)
- [Lazy Hydration Demo Acceptance](lazy-hydration-demo.md)
- [macOS Finder and FileProvider Reality](macos-fileprovider-reality.md)
- [Distribution Smoke Matrix](distribution-smoke-matrix.md)
- [Neo-Honey Live Acceptance](neo-honey-acceptance.md)
- [On-Prem Authority Recovery](onprem-authority-recovery.md)
