# TCFS Workstream Reality Check - 2026-05-09

This checkpoint records the repo, tracker, and proof state after the May 9,
2026 parity-proof and hosted-smoke grounding pass.
It is meant to keep planning language grounded while the remaining M10 work
continues.

## Source Snapshot

| Surface | Current state |
| --- | --- |
| Canonical repo | `Jesssullivan/tummycrypt` |
| Checkpoint commit | `9428513be22f9b55f45cda3713881130b612e9c0` |
| Open PRs | none after PR `#367` merged on May 13, 2026 |
| Current release | `v0.12.12` |
| Primary milestone | GitHub milestone `#9 M10: Usage Reality & Product Parity` |

## GitHub Tracker Reality

Open issues at audit time:

| Issue | Current decision |
| --- | --- |
| `#280` distribution proof | Keep open. Homebrew/Nix, Linux `.deb`/`.rpm`, and amd64 container proof are archived for `v0.12.12`; remaining blockers are production macOS `.pkg` clean-host proof, native arm64 container registry proof on a future tag, and production storage posture before broad project-tree distribution claims. |
| `#309` production macOS `.pkg`/Finder proof | Keep open. PZM testing-mode FileProvider proof is green but explicitly non-production. Hosted production `.pkg` attempt `25613963424` passed package install/signing/installed-CLI/config gates, then failed before daemon/Finder because the public quick-tunnel storage endpoint no longer resolved. The latest `linux-xr` storage-posture packet now includes same-prefix honey mounted traversal/symlink verification, but it is still not Finder or production storage proof. |
| `#312` tinyland branch tranche | Keep open. PR #351 recorded a non-destructive prune proposal; next decision is operator approve/defer for Tranche A. |
| `#327` on-prem OpenTofu migration/cutover | Keep open. Source/runbook work is ready, but live mutation waits on a named downtime window, rollback owner, and post-cut smoke owner. |
| `#298` residual Civo PVC retirement | Keep open until `#327` completes or an operator explicitly overrides the dependency. |

Closed in this pass:

- `#313`: `yoga` retirement is now option 1, documentation-only retirement.
  No archive, host deletion, key revocation, local remote removal, or branch
  deletion was performed.

## Linear Reality

The Linear project `Tummycrypt M10: Usage Reality & Product Parity` is a useful
management mirror, but the repo docs and GitHub issues remain the operational
truth.

| Linear issue | State | Current role |
| --- | --- | --- |
| `TIN-131` | Backlog | Mirror for GitHub `#280` distribution install/upgrade proof. |
| `TIN-132` | Backlog | Mirror for live backend / `neo-honey` acceptance. |
| `TIN-133` | In Progress | Mirror for lazy traversal and Finder/FileProvider hydration reality, currently pointing at GitHub `#309` and repo evidence docs. |
| `TIN-134` | Done | iOS posture decision is superseded by Apple/iOS status docs. |
| `TIN-135` | Done | odrive/desktop parity analysis is superseded by product reality and parity docs. |
| `TIN-151` | Done | macOS OpenSSL-linked daemon runtime defect was closed before the current proof push. |
| `TIN-720` | In Progress | Infrastructure/on-prem tailnet source-ownership lane; relevant to `#327`, not a blocker for disposable lazy/Finder proof. |

## Workstream Fronts

| Front | Current truth | Do not claim yet |
| --- | --- | --- |
| Linux CLI/daemon | Strongest supported path. CI, package smoke, live backend acceptance, and release evidence exist. | Universal Linux desktop UX or every distro/service-manager combination. |
| Linux mounted FUSE | Expanded lifecycle proof is archived in `docs/release/evidence/lazy-linux-20260508T170825Z/`: browse before hydration, exact `cat`, mounted write/readback, cache clear/rehydrate, recursive safe-unsync refusal/success. | Packaged mount/systemd first-use as continuously proven on every supported distro. |
| Fleet pilot | Extended isolated cross-host pilot proof is archived in `docs/release/evidence/fleet-pilot-extended-20260509T2152Z/`: local seed to disposable prefix, honey mounted traversal/hydration of `Documents` and `git`, honey Linux lifecycle companion for mounted write/readback, cache clear/rehydrate, recursive safe-unsync refusal/success, and live `neo-honey` SeaweedFS/NATS smoke. | Real `~/Documents` / `~/git` takeover, production Finder, same-fixture cross-host edit/pullback, or on-prem/OpenTofu cutover. |
| K8s/on-prem backend | Live backend works; source-owned OpenTofu migration/cutover is planned and renderable. | That NATS/SeaweedFS are already source-owned or storage-mobile. |
| CI/test coverage | PR `#367` merged on green. The merge push at `9428513` has Docs green (`25816193832`), Nix CI green (`25816193804`), and CI red (`25816193953`) only in `watcher_debounce_coalesces_rapid_writes`; the follow-up fix changes the assertion to the product contract, per-target-path coalescing, and passes locally. | Production Finder, iOS device, Kubernetes rollout, accessibility, visible badge/progress UX, production storage posture, or remote closure of the follow-up CI run before a release-facing claim. |
| Fuzzing | Four cargo-fuzz targets exist under `fuzz/`. | Continuous fuzz execution in CI or `task check`; fuzz is present but not currently a release gate. |
| macOS package/FileProvider | PZM testing-mode lane is green through enumerate, hydrate, evict, rehydrate, mutation upload/readback, CLI conflict state, and exact FileProvider content preservation. Hosted production `.pkg` attempt `25613963424` adds install/signing/installed-CLI/config proof but stops at stale public storage reachability before daemon/Finder. | Production Developer ID clean-host Finder lifecycle or visible Finder conflict/status UX. |
| iOS | Host app, extension, generated bindings, and simulator type-check surface exist. | Active release target, TestFlight/App Store readiness, real-device Files.app behavior, or write support. |
| Distribution | `v0.12.12` Homebrew/Nix, Linux `.deb`/`.rpm`, and amd64 container proof are archived. Release workflow is ready to publish arm64 container images on the next cut. | Native arm64 container proof for the current tag or production macOS `.pkg` clean-host proof. |
| Signing | Semantic release tags now fail closed on Developer ID signing/profile inputs; PZM testing-mode uses Mac App Development signing material and managed lab policy. | That Mac App Development testing-mode evidence substitutes for production Developer ID distribution evidence. |
| E2E/runners | PZM, `neo`, and `honey` have named roles in lab acceptance docs. GitHub-hosted macOS lanes need public storage endpoints. | That hosted macOS can replace a clean physical production Finder host. |
| S3 storage posture | PR `#367` landed opt-in fresh-prefix file upload concurrency plus timeout/retry telemetry. The current `home-canary-linux-xr-storage-posture-20260514T021513Z/` packet completes the 7.7 GB shadow push, proves `.pack` / `.rev` object-count reductions, and includes honey mounted traversal/hydration plus all 85 mounted symlink target checks against the same prefix. The mounted warning follow-up is now closed by `home-canary-linux-xr-storage-posture-tc-extfix-20260514T202343Z/`: S3 `NoSuchKey` warnings dropped from 274 to 0 after exact `.tc` filenames stopped being treated as legacy stub aliases. It is still not production storage posture: endpoint is tailnet HTTP SeaweedFS, socket highwater exceeds configured upload concurrency, `.idx` and generated large headers remain measured object-count follow-ups, and honey lifecycle was disabled in this storage packet. | Production S3 posture, broad `~/git`/home-directory management, or a claim that more client concurrency alone solves large project-tree storage. |
| Helm/Kubernetes charts | `tcfs-stack` is an umbrella control-plane chart with external SeaweedFS credentials and endpoint; `tcfs-backend` is the direct worker chart. | Blank-cluster storage bootstrap or easy standalone chart install without KEDA/ServiceMonitor CRDs unless optional resources are disabled. |
| Remote governance | `origin` is canonical. `tinyland` has a prune proposal. `yoga` is documentation-only retired. | Any remote branch deletion without explicit operator approval. |

## Next Grounded Goals

1. Decide `#312` Tranche A: approve or explicitly defer deletion of the 44
   superseded fix/chore branches.
2. Keep `#280` focused on native arm64 container proof on the next tag and
   production macOS `.pkg` clean-host proof.
3. Keep `#309` focused on production Developer ID Finder lifecycle and the
   storage/object-model decision exposed by the `linux-xr` canary. Do not turn
   the post-PR `#367` storage blocker into a success claim.
4. Decide the large-project storage product shape before another full raw-Git
   canary: multipart/native SeaweedFS writes, large-tree batching, TLS endpoint
   posture, and raw `.git` defaults.
5. Schedule `#327` only with a named downtime window, rollback owner, and
   post-cut smoke owner.
6. Leave `#298` blocked until the on-prem cutover decision is grounded.
7. Keep `TIN-131`, `TIN-132`, and `TIN-133` as Linear mirrors; do not expand
   Linear into a second source of truth for exact proof claims.

## Related Docs

- [Product Reality And Priority](product-reality-and-priority.md)
- [TCFS Feature and Objective Matrix](feature-objective-matrix-2026-05-09.md)
- [TCFS Usage Reality Sprint Plan](usage-reality-sprint-plan-2026-05-06.md)
- [TCFS Fleet Parity Sprint Plan](fleet-parity-sprint-plan-2026-05-09.md)
- [Distribution Smoke Matrix](distribution-smoke-matrix.md)
- [Lazy Hydration Demo Acceptance](lazy-hydration-demo.md)
- [Apple Surface Status](apple-surface-status.md)
- [macOS Finder and FileProvider Reality](macos-fileprovider-reality.md)
- [iOS Surface Status](ios-surface-status.md)
- [On-Prem Authority Recovery](onprem-authority-recovery.md)
- [Remote Governance](remote-governance.md)
- [Tinyland Branch Prune Proposal - 2026-05-09](tinyland-branch-prune-proposal-2026-05-09.md)
