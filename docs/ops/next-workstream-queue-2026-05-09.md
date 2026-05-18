# TCFS Next Workstream Queue - 2026-05-09

This queue turns the current repo, GitHub, and Linear truth into execution
order. It is intentionally narrower than the full backlog: each lane below has
a concrete acceptance bar and a boundary that prevents accidental overclaiming.

## Current Source Of Truth

| Lane | Trackers | Ready state | Do next | Boundary |
| --- | --- | --- | --- | --- |
| Production macOS Finder/FileProvider | [#309](https://github.com/Jesssullivan/tummycrypt/issues/309), [TIN-133](https://linear.app/tinyland/issue/TIN-133/prove-lazy-traversal-and-finderfileprovider-hydration-reality) | PZM testing-mode proof is green through enumerate, hydrate, evict, rehydrate, mutation, and deterministic conflict/status content preservation. Extended fleet lifecycle proof is archived and tracker-linked. Hosted production `.pkg` attempt `25613963424` passed package install, signing, installed CLI, and config provisioning, then failed before daemon/Finder because the public Cloudflare quick-tunnel endpoint no longer resolved from GitHub-hosted macOS. May 17 neo packets now prove authenticated install of the notarized workflow artifact, stale user-app quarantine after inventory, strict installed preflight, package daemon storage `[ok]`, host-app domain add, CloudStorage enumeration, and `requestDownload`. | Root-cause the installed production FileProvider read timeout, rerunning with the seeded-fixture/index-inspect gate so `expected-file-index.json` proves the target is remotely visible before FileProvider read. After exact-content hydrate passes, add evict/rehydrate, mutation, and conflict/status proof. Hosted fallback still requires a fresh reachable public storage endpoint. | PZM testing-mode, isolated fleet proof, hosted install/signing partial pass, and neo's domain/enumeration/requestDownload proof do not close production Finder acceptance until exact read/hydration passes. Finder badge/progress assertions stay observational until reliable. |
| Distributions | [#280](https://github.com/Jesssullivan/tummycrypt/issues/280), [TIN-131](https://linear.app/tinyland/issue/TIN-131/prove-distribution-install-and-upgrade-flows-across-supported-release) | `v0.12.12` Homebrew, Darwin Nix, Linux package, and amd64 container evidence is archived. Release workflow is ready to publish future `linux/arm64/v8` images. | Do not cut a release just for proof hygiene. On the next real tag, archive native arm64 container pull/version/startup proof. Tie production `.pkg` closure to the #309 clean-host run. | Current `v0.12.12` container proof is amd64-only. Packaged Linux FUSE/systemd first-use is separate unless explicitly promoted into release acceptance. |
| Fleet/home-directory parity | [#309](https://github.com/Jesssullivan/tummycrypt/issues/309), [TIN-133](https://linear.app/tinyland/issue/TIN-133/prove-lazy-traversal-and-finderfileprovider-hydration-reality) | `docs/release/evidence/fleet-pilot-extended-20260509T2152Z/` proves isolated `Documents`/`git` traversal, hydration, mounted write/readback, cache clear/rehydrate, recursive safe-unsync refusal/success, and live `neo-honey` smoke. `docs/release/evidence/git-repo-canary-oauth-mux-sourcebin-fresh-20260515T014640Z/` proves a clean `~/git/oauth-mux` shadow with source-built binaries on both hosts. `docs/release/evidence/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/` repeats that small clean repo proof with explicit current Nix flake package binaries on both hosts: fresh-prefix push, 0 skipped symlinks, honey mounted traversal/hydration, 9 mounted symlink target checks, and Linux lifecycle. `docs/release/evidence/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/restore-proof/` preserves the original Nix binary timeout. `restore-proof-source-fix-empty-dirs-20260515T183805Z/` proves source-built fresh-tree restore, and `restore-proof-nixpkg-current-empty-dirs-20260515T200359Z/` proves rebuilt Nix package restore for 4,601 regular files, 9 symlinks, synced state for all 4,610 restored paths, and all 12 empty dirs with `--require-empty-dirs`. The first `linux-xr-fast` Nix-package attempts are blocker packets, not parity proof: current package push stalls around a 387 MB `.git/objects/pack/*.idx` upload. Source-built follow-ups prove pack-index and temp-pack fixes, then expose `.git/index` as the next max-chunk path before the current source fix. Homebrew package drift is still real: installed Homebrew `0.12.12` skipped symlinks. | Rerun `~/git/linux-xr-fast` only with a selected candidate binary/package that includes the Git pack-index, temp-pack, and `.git/index` chunk-profile fixes, then add package-backed restore/rollback proof. Prove Homebrew separately if it becomes the client lane. Keep broad `~/Documents` or `~/git` management out until package-backed real-project lanes are boring. | The archived fleet packet and new canary task use disposable shadows/prefixes, not real home-directory takeover. Nix package restore success does not equal Homebrew readiness or live repo move safety. |
| S3 storage posture | [#309](https://github.com/Jesssullivan/tummycrypt/issues/309), [#280](https://github.com/Jesssullivan/tummycrypt/issues/280), [TIN-133](https://linear.app/tinyland/issue/TIN-133/prove-lazy-traversal-and-finderfileprovider-hydration-reality) | The scoped `linux-xr` parity packet is functionally green, and the storage observations exposed the raw-Git `.pack`/`.idx` object shape. PR #367 is merged and proves the fresh-prefix file-concurrency knob plus timeout/retry telemetry are real. The current storage packet completes the 7.7 GB shadow with `file_upload_concurrency=8`, proves same-prefix honey mounted traversal/hydration and all 85 mounted symlink targets, keeps the 6.2 GB `.pack` at 1,211 chunks, reduces the 45.6 MB `.rev` from 8,405 chunks to 8 chunks, and has a lifecycle companion. The remaining measured storage blockers are candidate-package proof for the Git pack-index/temp-pack/`.git/index` profiles, generated AMD headers at 2,986/2,121 chunks, plaintext tailnet HTTP, and socket highwater 11 vs upload concurrency 8. | Rerun a candidate package/binary with the Git metadata chunk-profile fixes, decide generated-large-file policy and socket-pool accounting, then rerun the storage-posture helper against a production-like TLS endpoint before upgrading the claim. Keep multipart/native SeaweedFS writes, batching strategy, and raw `.git` default policy as explicit follow-ups if object counts remain high. | Correctness success and lab S3 observations do not imply production storage readiness. Do not claim production S3 posture from the current tailnet HTTP SeaweedFS packet. |
| On-prem source-owned cutover | [#327](https://github.com/Jesssullivan/tummycrypt/issues/327), [TIN-720](https://linear.app/tinyland/issue/TIN-720/converge-remaining-tcfs-tailscale-proxy-source-ownership) | Source-owned OpenTofu migration commands, candidate services, target PVC commands, preflight, render-only validation, and the downtime cutover packet renderer exist. Live namespace is serving well enough for current smoke. | Only proceed after naming a downtime window, preflight owner, rollback owner, and post-cut smoke owner. Then render `just onprem-cutover-packet`, attach it to the tracker, run preflight, target PVC apply, quiesce/copy, candidate smoke, canonical hostname cutover, fleet smoke, and retained-PV rollback hold. | No live OpenTofu apply, PVC migration, or tailnet cutover has happened. Do not fix the tailnet gate by cosmetic ProxyClass-only Service mutation. |
| Residual Civo retirement | [#298](https://github.com/Jesssullivan/tummycrypt/issues/298) | Civo is documented as legacy/standby; honey/on-prem owns the active path. | Keep blocked on #327 unless an operator explicitly separates the Civo keep/retire decision. | Do not delete preserved Civo state without an explicit keep/retire decision. |
| Tinyland branch hygiene | [#312](https://github.com/Jesssullivan/tummycrypt/issues/312) | A non-destructive prune proposal is archived. It recommends 44 fix/chore branches for explicit Tranche A deletion and 17 feature/test branches for a short human pass. | Wait for operator approve/defer on Tranche A. If approved, delete only the named approved branches and record the close reason. | No tinyland branch deletion occurred in the parity sprint. |

## Immediate Sprint Slice

1. Production Finder read timeout.
   The executor is currently `neo` with the notarized workflow artifact
   installed into `/Applications`. Hosted attempt `25613963424` still proves
   the public quick-tunnel secret is stale from GitHub-hosted macOS, but the
   immediate blocker is local: `macos-fileprovider-neo-finder-release-smoke-directhost-catread-20260517T020417Z/`
   reaches domain add, enumeration, and `requestDownload`, then `cat` returns
   `Operation timed out`.

2. Production `.pkg` run packet.
   Use the current macOS postinstall harness and archive a follow-up run with
   package install/preflight carried forward, host launch, FileProvider domain
   presence, CloudStorage enumeration, `expected-file-index.json`,
   exact-content hydrate, and logs. Use `SEED_EXPECTED_FILE=1` if the target
   fixture should be created fresh for this packet.

3. Expendable real-project fleet proof.
   Start with `task lazy:git-repo-canary`, defaulting to a clean
   `~/git/oauth-mux` shadow. The source-built and explicit current Nix package
   lanes are green. The original
   `git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/restore-proof/` timeout is
   preserved, `restore-proof-source-fix-empty-dirs-20260515T183805Z/`
   proves source-built fresh-tree restore, and
   `restore-proof-nixpkg-current-empty-dirs-20260515T200359Z/` proves rebuilt
   Nix package restore for regular files, symlinks, synced state, and empty
   directories. The first `~/git/linux-xr-fast` package attempts are now
   blocker packets: the clean shadow is `.git`-heavy and current package push
   stalls around a 387 MB `.git/objects/pack/*.idx` upload. Source-built
   follow-ups prove pack-index and temp-pack reductions, and the latest source
   now also covers the exact `.git/index` file. Rerun the large stress lane
   only after the selected candidate binary/package includes all three Git
   metadata profile fixes, then add package-backed fresh-tree restore/rollback
   proof.
   Keep this outside real `~/git` takeover.

4. S3 storage posture decision.
   The `.pack` and `.rev` object-model fixes are archived and accepted with a
   same-prefix mounted traversal follow-up, but production posture is still
   blocked. The `linux-xr-fast` package blocker confirms `.idx` posture was the
   next raw-Git object-count problem; source-built follow-ups then exposed
   temp packs and `.git/index` as the next two raw-Git metadata hotspots. Source
   now covers all three, but generated-large-file policy, socket-pool
   accounting, endpoint/TLS class, and package rerun proof remain open. Keep
   multipart/native SeaweedFS writes, large-tree batching, TLS endpoint posture,
   and raw `.git` defaults as explicit follow-up decisions.

5. Tracker sync.
   Update #309/TIN-133 with the production Finder result or executor blocker,
   update #280/TIN-131 only if the result changes `.pkg` distribution status,
   and leave #327/#298/#312 unchanged unless their explicit gate moves.

## Stop Conditions

- Stop before any real home-directory takeover.
- Stop before any live OpenTofu/Kubernetes mutation without the named downtime
  package.
- Stop before deleting tinyland branches without explicit approval.
- Stop before claiming production Finder if the run used PZM testing mode or a
  source-tree app instead of a published Developer ID package.
