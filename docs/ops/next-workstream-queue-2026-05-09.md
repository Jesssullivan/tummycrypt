# TCFS Next Workstream Queue - 2026-05-09

This queue turns the current repo, GitHub, and Linear truth into execution
order. It is intentionally narrower than the full backlog: each lane below has
a concrete acceptance bar and a boundary that prevents accidental overclaiming.

## Current Source Of Truth

| Lane | Trackers | Ready state | Do next | Boundary |
| --- | --- | --- | --- | --- |
| Production macOS Finder/FileProvider | [#309](https://github.com/Jesssullivan/tummycrypt/issues/309), [TIN-133](https://linear.app/tinyland/issue/TIN-133/prove-lazy-traversal-and-finderfileprovider-hydration-reality) | PZM testing-mode proof is green through enumerate, hydrate, evict, rehydrate, mutation, and deterministic conflict/status content preservation. Extended fleet lifecycle proof is archived and tracker-linked. Hosted production `.pkg` attempt `25613963424` passed package install, signing, installed CLI, and config provisioning, then failed before daemon/Finder because the public Cloudflare quick-tunnel endpoint no longer resolved from GitHub-hosted macOS. | Refresh `TCFS_SMOKE_S3_ENDPOINT` to a currently reachable public endpoint or move this lane to a private/self-hosted Mac with backend reachability, then rerun the published `.pkg` through install, host launch, domain presence, CloudStorage enumeration, exact-content hydrate, and log capture. | PZM testing-mode, isolated fleet proof, and hosted install/signing partial pass do not close production Finder acceptance. Finder badge/progress assertions stay observational until reliable. |
| Distributions | [#280](https://github.com/Jesssullivan/tummycrypt/issues/280), [TIN-131](https://linear.app/tinyland/issue/TIN-131/prove-distribution-install-and-upgrade-flows-across-supported-release) | `v0.12.12` Homebrew, Darwin Nix, Linux package, and amd64 container evidence is archived. Release workflow is ready to publish future `linux/arm64/v8` images. | Do not cut a release just for proof hygiene. On the next real tag, archive native arm64 container pull/version/startup proof. Tie production `.pkg` closure to the #309 clean-host run. | Current `v0.12.12` container proof is amd64-only. Packaged Linux FUSE/systemd first-use is separate unless explicitly promoted into release acceptance. |
| Fleet/home-directory parity | [#309](https://github.com/Jesssullivan/tummycrypt/issues/309), [TIN-133](https://linear.app/tinyland/issue/TIN-133/prove-lazy-traversal-and-finderfileprovider-hydration-reality) | `docs/release/evidence/fleet-pilot-extended-20260509T2152Z/` proves isolated `Documents`/`git` traversal, hydration, mounted write/readback, cache clear/rehydrate, recursive safe-unsync refusal/success, and live `neo-honey` smoke. | Choose one expendable real project repo and prove cross-host browse/hydrate/edit/pullback. Keep broad `~/Documents` or `~/git` management out until the smaller real-project lane is boring. | The archived fleet packet uses disposable roots and prefixes, not real home-directory takeover. |
| S3 storage posture | [#309](https://github.com/Jesssullivan/tummycrypt/issues/309), [#280](https://github.com/Jesssullivan/tummycrypt/issues/280), [TIN-133](https://linear.app/tinyland/issue/TIN-133/prove-lazy-traversal-and-finderfileprovider-hydration-reality) | The scoped `linux-xr` parity packet is functionally green, and the storage observations exposed the raw-Git `.pack`/`.idx` object shape. Release-binary/fresh-prefix packet `home-canary-linux-xr-storage-posture-20260512T034347Z/` is blocker evidence: the large `.pack` completed, but multi-minute no-progress/no-retry gaps and the slow small-file walk keep production storage posture open. | Merge the per-chunk timeout/retry telemetry patch, then rerun `task lazy:home-canary-linux-xr-storage-posture` on a new disposable prefix with timeout-enabled release binary, fresh-prefix upload telemetry, endpoint/TLS and credential-presence metadata, memory/timing/object counts, honey traversal, and selected hydration latency. | Correctness success and lab S3 observations do not imply production storage readiness. Do not claim production S3 posture until the timeout-enabled packet lands and its endpoint/security/performance bars are accepted. |
| On-prem source-owned cutover | [#327](https://github.com/Jesssullivan/tummycrypt/issues/327), [TIN-720](https://linear.app/tinyland/issue/TIN-720/converge-remaining-tcfs-tailscale-proxy-source-ownership) | Source-owned OpenTofu migration commands, candidate services, target PVC commands, preflight, render-only validation, and the downtime cutover packet renderer exist. Live namespace is serving well enough for current smoke. | Only proceed after naming a downtime window, preflight owner, rollback owner, and post-cut smoke owner. Then render `just onprem-cutover-packet`, attach it to the tracker, run preflight, target PVC apply, quiesce/copy, candidate smoke, canonical hostname cutover, fleet smoke, and retained-PV rollback hold. | No live OpenTofu apply, PVC migration, or tailnet cutover has happened. Do not fix the tailnet gate by cosmetic ProxyClass-only Service mutation. |
| Residual Civo retirement | [#298](https://github.com/Jesssullivan/tummycrypt/issues/298) | Civo is documented as legacy/standby; honey/on-prem owns the active path. | Keep blocked on #327 unless an operator explicitly separates the Civo keep/retire decision. | Do not delete preserved Civo state without an explicit keep/retire decision. |
| Tinyland branch hygiene | [#312](https://github.com/Jesssullivan/tummycrypt/issues/312) | A non-destructive prune proposal is archived. It recommends 44 fix/chore branches for explicit Tranche A deletion and 17 feature/test branches for a short human pass. | Wait for operator approve/defer on Tranche A. If approved, delete only the named approved branches and record the close reason. | No tinyland branch deletion occurred in the parity sprint. |

## Immediate Sprint Slice

1. Production Finder executor decision.
   Pick the executor for #309: GitHub-hosted macOS with a refreshed public
   storage endpoint, private-network hosted runner, self-hosted VM, or manual
   Darwin lane. Record the fallback rule before running. Hosted attempt
   `25613963424` proves the current public quick-tunnel secret is stale from
   GitHub-hosted macOS.

2. Production `.pkg` run packet.
   Use the current macOS postinstall harness and archive a tagged run with
   package install, host launch, FileProvider domain presence, CloudStorage
   enumeration, exact-content hydrate, and logs.

3. Expendable real-project fleet proof.
   Seed one non-critical real project repo through TCFS, traverse it from a
   second host, hydrate exact files, edit one file, pull the edit back, and
   unsync clean descendants. Keep this outside real `~/git` takeover.

4. S3 storage posture packet.
   Merge the chunk-timeout telemetry patch, then rerun
   `task lazy:home-canary-linux-xr-storage-posture` with a timeout-enabled
   release binary and a new disposable prefix. Archive the storage packet
   before using any raw-Git throughput, object-count, or memory observation as
   a claim.

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
