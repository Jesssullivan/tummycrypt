# TCFS Feature And Objective Matrix - 2026-05-09

This matrix is the wayfinding layer between README claims, repo evidence,
GitHub issues, and Linear mirrors. It should answer two questions quickly:

1. What can TCFS honestly claim today?
2. Which objective closes the next proof gap?

## Tracker Cross-Links

| Objective | GitHub | Linear | Current state |
| --- | --- | --- | --- |
| Distribution install and upgrade proof | [#280](https://github.com/Jesssullivan/tummycrypt/issues/280) | [TIN-131](https://linear.app/tinyland/issue/TIN-131/prove-distribution-install-and-upgrade-flows-across-supported-release) | Open. Strong `v0.12.12` Homebrew/Nix/Linux package/amd64 container evidence exists; production macOS `.pkg` clean-host proof and native arm64 container proof remain. |
| Live neo-honey backend acceptance | repo docs | [TIN-132](https://linear.app/tinyland/issue/TIN-132/operationalize-the-neo-honey-live-fleet-acceptance-lane) | Backlog mirror. `neo-honey` proves live SeaweedFS/NATS sync when run, but it is not a Finder or lazy traversal proof by itself. |
| Lazy traversal, unsync, and Finder/FileProvider reality | [#309](https://github.com/Jesssullivan/tummycrypt/issues/309) | [TIN-133](https://linear.app/tinyland/issue/TIN-133/prove-lazy-traversal-and-finderfileprovider-hydration-reality) | In progress. Linux and PZM lab evidence are strong; production Developer ID clean-host Finder remains open. |
| Tinyland branch hygiene | [#312](https://github.com/Jesssullivan/tummycrypt/issues/312) | none primary | Open. A non-destructive prune proposal exists; no tinyland branch deletion without explicit approval. |
| On-prem source-owned cutover | [#327](https://github.com/Jesssullivan/tummycrypt/issues/327) | [TIN-720](https://linear.app/tinyland/issue/TIN-720/converge-remaining-tcfs-tailscale-proxy-source-ownership) | Open. Source/runbook work is ready, but live mutation waits on a named downtime window, rollback owner, and post-cut smoke owner. |
| Residual Civo PVC retirement | [#298](https://github.com/Jesssullivan/tummycrypt/issues/298) | blocked by infra lane | Open, blocked on `#327` unless an operator explicitly separates it. |
| Debian 12 exclusion decision | [#308](https://github.com/Jesssullivan/tummycrypt/issues/308) | distribution context | Closed. Current docs state the Ubuntu 24.04+ / Debian 13+ floor. |

## Feature Matrix

| Feature / surface | Objective | Readiness | Strongest proof | Next action |
| --- | --- | --- | --- | --- |
| Fleet-pilot packet | Bundle isolated `Documents`/`git` fixture generation, honey commands, and optional live smoke transcripts. | Green for isolated seed, honey traversal/hydration, and live `neo-honey` smoke. | `docs/release/evidence/fleet-pilot-20260509T1919Z/`, `task lazy:fleet-pilot-plan`, `scripts/fleet-parity-pilot-demo.sh`, `scripts/test-fleet-parity-pilot-demo.sh`. | Extend the packet with mounted write/readback and recursive safe-unsync if this becomes the broader home-directory acceptance gate. |
| Linux clean-name mounted traversal | Browse remote trees without hydrating all file bodies. | Green on `honey` for the archived lifecycle lane. | `docs/release/evidence/lazy-linux-20260508T170825Z/`, `tcfs-vfs` lifecycle tests. | Keep harness green while cross-host pilot evidence is built. |
| On-demand hydration | Hydrate exact selected content on open/read. | Green on Linux; green in PZM testing-mode FileProvider lab; production Finder still open. | Linux lifecycle evidence; PZM run IDs in FileProvider docs; `cargo test -p tcfs-vfs --test vfs_lifecycle_test`. | Prove production Developer ID clean-host hydrate through `#309`. |
| Mounted write/readback | Edit through the mounted view and prove exact remote content. | Green on Linux lifecycle evidence. | `mounted-write-remote-pull.log` in the Linux evidence bundle. | Add cross-host edit/pullback to the fleet-pilot packet. |
| Recursive safe-unsync | Convert clean tracked descendants back to dehydrated representation while refusing dirty descendants unless forced. | Green in CLI tests and Linux host evidence. | `cargo test -p tcfs-cli cli_unsync`, `unsync-dirty.out`, `unsync-success.out`, `unsync-status.out`. | Keep status/state-ordering tests in the sprint gate. |
| Physical `.tc` / `.tcf` stubs | Represent dehydrated sync-root files outside mounted/FileProvider views. | Real and tested for CLI/offline roots. | CLI unsync tests and lazy lifecycle evidence. | Avoid presenting physical stubs as the Finder UX. |
| macOS FileProvider lab | Prove enumerate, hydrate, evict, rehydrate, mutation, and conflict/status content preservation. | Green in non-production PZM testing mode. | `macos-fileprovider-reality.md` and testing-mode run IDs. | Treat as regression proof, not production acceptance. |
| Production Finder / CloudStorage | Install a signed `.pkg` on a clean Developer ID host and prove user-facing lifecycle. | Not proven. | Local `neo` source-tree proof and PZM testing-mode proof are supporting evidence only. | Select executor and run `#309` clean-host package-to-Finder lane. |
| Fleet sync / live backend | Prove two devices can sync through live SeaweedFS + NATS. | Materially exercised; transcripts should be archived per release claim. | `docs/ops/neo-honey-acceptance.md`, lab host acceptance docs. | Run and archive a current `neo-honey` packet when cross-host parity starts. |
| Distribution install | Prove supported install/upgrade surfaces. | Partial but strong for `v0.12.12`. | Distribution, Linux package, and container evidence bundles. | Finish production macOS `.pkg` and native arm64 container proof after a future tag. |
| K8s worker/backend | Run TCFS backend workers and storage against the on-prem cluster. | Live enough for smoke; source-owned cutover deferred. | On-prem runbooks, preflight/data-inventory notes, `#327`. | Schedule downtime before any OpenTofu apply or storage migration. |
| CI/test coverage | Keep code and docs claims defensible. | Strong for Rust, docs, Nix, staticlib, and iOS typecheck. | PR green matrices and local focused tests. | Do not imply CI proves production Finder, iOS device, K8s apply, or badge/progress UX. |
| iOS FileProvider | Maintain experimental Files.app direction. | Proof-of-concept only. | `docs/ops/ios-surface-status.md`, simulator typecheck. | Keep compile/typecheck green; do not claim write/device/TestFlight readiness. |
| Windows / Cloud Files | Future Explorer placeholder path. | Planned/skeleton. | README/platform support docs. | Keep out of release claims until a real CLI/daemon/Explorer lane exists. |
| Remote governance | Keep canonical source and branch hygiene understandable. | Grounded. | `remote-governance.md`, `tinyland-branch-prune-proposal-2026-05-09.md`. | Decide `#312` Tranche A; do not delete without approval. |

## Next Workstream Todo List

1. Build the fleet-pilot evidence packet.
   Use isolated roots such as `~/TCFS Pilot/Documents` and `~/TCFS Pilot/git`,
   a disposable remote prefix, and at least `neo` plus `honey`. Prove browse,
   hydrate, edit, unsync/dehydrate, rehydrate, and exact content preservation.
   Start with `task lazy:fleet-pilot-plan`; use `TCFS_FLEET_PILOT_PUSH=1`,
   `TCFS_FLEET_PILOT_RUN_HONEY=1`, and `TCFS_HONEY_START_MOUNT=1` when the
   disposable remote and honey credentials are ready.
   Initial packet `docs/release/evidence/fleet-pilot-20260509T1919Z/` is green
   for seed, honey traversal/hydration, and live `neo-honey`; writeback and
   safe-unsync remain covered by the Linux lifecycle packet, not this pilot.

2. Keep safe-unsync as a hard gate.
   Run `cargo test -p tcfs-cli cli_unsync`,
   `cargo test -p tcfs-vfs --test vfs_lifecycle_test`, and the Linux lifecycle
   harness before claiming the pilot is green.

3. Turn `#309` into a production Finder decision.
   Pick the clean Developer ID host or runner, install the published `.pkg`,
   launch `TCFSProvider.app`, add the FileProvider domain, enumerate
   CloudStorage, hydrate exact content, and capture logs. Keep PZM as lab
   regression evidence only.

4. Keep `#280` narrow.
   Do not reopen already-proven surfaces. The remaining proof is production
   macOS `.pkg` plus native `linux/arm64/v8` GHCR proof after the next
   multi-arch tag. Packaged Linux FUSE/systemd first-use is a separate decision.

5. Defer `#327` unless a maintenance window exists.
   The parity sprint can use disposable prefixes. On-prem mutation requires a
   named window, preflight owner, rollback owner, and post-cut smoke owner.

6. Resolve or defer `#312`.
   Record whether the 44-branch Tranche A tinyland prune proposal is approved.
   Do not delete branches from this sprint without explicit approval.

7. Keep `#298` blocked.
   Residual Civo PVC retirement should wait for `#327` or an explicit operator
   decision that separates it from the TCFS source-owned cutover.

8. Keep iOS and Windows honest.
   iOS remains proof-of-concept; Windows remains planned/skeleton. They should
   not block the Linux/Finder parity sprint and should not gain stronger public
   claims without new device or Explorer evidence.

## Related Docs

- [Product Reality and Priority](product-reality-and-priority.md)
- [TCFS Fleet Parity Sprint Plan - 2026-05-09](fleet-parity-sprint-plan-2026-05-09.md)
- [TCFS Workstream Reality Check - 2026-05-09](workstream-reality-check-2026-05-09.md)
- [Lazy Hydration Demo Acceptance](lazy-hydration-demo.md)
- [macOS Finder and FileProvider Reality](macos-fileprovider-reality.md)
- [Distribution Smoke Matrix](distribution-smoke-matrix.md)
- [Neo-Honey Live Acceptance](neo-honey-acceptance.md)
- [Lab Host Acceptance Matrix](lab-host-acceptance-matrix.md)
- [On-Prem Authority Recovery](onprem-authority-recovery.md)
- [iOS Surface Status](ios-surface-status.md)
