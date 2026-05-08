# TCFS Usage Reality Sprint Plan

Date: May 6, 2026

This is the short-lived execution plan for turning the current TCFS surface
matrix into archived, repeatable proof. It is intentionally separate from
long-term product docs: update it as proof lands, then retire it once the
M10 usage-reality issues are closed.

## Current Evidence Ledger

| Surface | Truth today | Strongest evidence | Next proof gate |
| --- | --- | --- | --- |
| Linux CLI + daemon | Strongest supported path | CI, release smoke, `neo-honey` live acceptance | Archive a current Linux first-use transcript against the live/disposable backend |
| Linux mounted FS | Green for the expanded lifecycle on a real FUSE host: browse remote tree, hydrate exact content, mounted write/readback, clear cache, rehydrate, and recursive safe-unsync refusal/success. | `tcfs-vfs` tests, mounted helper regressions, archived lifecycle evidence `docs/release/evidence/lazy-linux-20260508T170825Z/`, and `task lazy:linux-lifecycle-demo` | Keep the harness green while deeper conflict/status surfacing work proceeds |
| Physical `.tc` / `.tcf` stubs | Stub wire format, parse/write, compatibility, and recursive CLI safe-unsync behavior are tested and host-proven | `tcfs-vfs`, daemon unsync tests, CLI recursive unsync tests, and `docs/release/evidence/lazy-linux-20260508T170825Z/` | Broaden product-level status visibility beyond CLI transcript evidence |
| macOS CLI + daemon | Package install, signing, E2EE, storage, and daemon startup are repeatedly proven | `v0.12.12` release and PZM smoke pre-harness stages | Keep install smoke green while FileProvider lab work continues |
| macOS Finder/FileProvider, lab | Green for the non-production PZM testing-mode lane through enumerate, exact-content hydrate, evict, rehydrate, CloudStorage mutation upload/readback, and deterministic CLI conflict/exact-content preservation. This remains testing-mode lab proof, not production Finder proof. | PZM run `25446601375` green for read/hydrate; package run `25456290021` proves build-output host startup; smoke run `25456341985` captured the pre-profile `_dyld_start` AppleSystemPolicy block; smoke run `25458526158` proves macOS 15 rejects `spctl --add`; smoke run `25562087555` passes profile verification, installed host policy probe, shared-Keychain config, E2EE, FileProvider registration, enumeration, requestDownload, evict, re-requestDownload, and exact 55-byte hydration; package run `25565895586` builds the p12-signed testing-mode package; smoke run `25565943781` passes mutation with exact 68-byte remote pull and post-mutation storage `[ok]`; package run `25569345240` builds the current branch testing-mode package; smoke run `25569596910` passes `exercise_conflict_status=true` with CLI `sync state: conflict` and exact FileProvider content preservation | Treat badges/progress as captured evidence until reliable assertions exist |
| macOS Finder/FileProvider, production | Not proven on arbitrary clean Developer ID hosts | Local `neo` source-tree proof and production package install/signing gates | Separate clean-host production Finder enablement from PZM testing-mode evidence |
| iOS | Proof-of-concept only | Swift build/type-check scaffold | Decide whether to keep as scaffold or create a real Files.app device lane |
| On-prem backend | Live endpoint client smoke works; source-owned migration is still open and explicitly deferred from this usage-reality sprint unless a maintenance window is scheduled | `neo-honey` smoke using MagicDNS endpoints | Keep out of the lazy/Finder proof path; schedule `#327` downtime separately, then archive post-cutover storage/NATS proof |
| Distribution parity | Release assets are publishing; Homebrew and Nix current-tag proof is refreshed, while current `.deb` proof remains scoped to Ubuntu 24.04+ / Debian 13+ | `v0.12.12` distribution evidence `docs/release/evidence/distribution-v01212-20260508T205913Z/`, current tagged releases, and the `v0.12.2` distribution matrix | Refresh `.deb` proof on Ubuntu 24.04+ / Debian 13+ when the next full distribution matrix is run |

## Apple Lab Ground Truth

The PZM lab is no longer blocked on "no certificate" or "wrong profile."
The current lab material is:

- runner: `petting-zoo-mini-tcfs`
- device UDID: `00008132-001240C80138801C`
- team: `QP994XQKNH`
- development certificate currently bound to the installed lab profiles:
  `E9B03E55D391E4368F1C4E8C8A7AE0FC1372D5E6`
- host profile: `tcfs-host-development-testing-mode-pzm-e9b03e55`
- extension profile: `tcfs-fileprovider-development-pzm-e9b03e55`
- local runner profile directory:
  `/Users/jsullivan2/.tcfs-fileprovider-lab`
- matching p12:
  `/Users/jsullivan2/.tcfs-fileprovider-lab/tcfs-fileprovider-lab-E9B03E55.p12`

Apple's FileProvider testing-mode entitlement is development/testing-only. The
host must request `com.apple.developer.fileprovider.testing-mode` before
assigning a non-empty `NSFileProviderDomain.testingModes` value. The App Store
Connect API can create/download profiles and certificates, but the private key
still lives on the machine that generated the CSR. See Apple's current
documentation for the
[testing-mode entitlement](https://developer.apple.com/documentation/BundleResources/Entitlements/com.apple.developer.fileprovider.testing-mode),
[App Store Connect provisioning profiles](https://developer.apple.com/documentation/appstoreconnectapi/profiles),
and
[certificates](https://developer.apple.com/documentation/appstoreconnectapi/certificates).

Current PZM lab proof:

1. `taskgated-helper` accepts the host and extension profiles for
   `io.tinyland.tcfs` and `io.tinyland.tcfs.fileprovider`.
2. Package run `25456290021` proves the build-output host app reaches Swift
   `main()` in `TCFS_FILEPROVIDER_HOST_POLICY_PROBE_ONLY=1` mode and exits 0
   with `policyProbe: main entered`, `policyProbe: domain created`, and
   `policyProbe: OK`, even though `spctl` rejects the bundle.
3. Smoke run `25456341985` adds an installed-host policy probe before live
   config and domain mutation. It times out after 15s with no Swift stderr, and
   the captured `sample` shows the process still at `_dyld_start`.
4. The same run's full harness still writes no `host-domain-launch.log` event;
   AppleSystemPolicy denies that installed host process before the instrumented
   Swift startup path emits.
5. `spctl --assess --type execute` rejects both the installed host app and
   extension.
6. The installed app tree carries `com.apple.provenance` xattrs, but stripping
   provenance/quarantine from a temporary copy does not make Gatekeeper accept
   the Mac Development signature.
7. `syspolicy_check distribution` reports a missing notarization ticket.
8. `syspolicy_check notary-submission` reports a fatal Gatekeeper rejection for
   `TCFSProvider.app/Contents/MacOS/TCFSProvider`.
9. `fileproviderd` starts the extension process.
10. AppleSystemPolicy terminates both installed processes:
   `Security policy would not allow process ... TCFSProvider` and
   `Security policy would not allow process ... TCFSFileProvider`.
11. Smoke run `25458526158` proved macOS 15 exits 4 for `spctl --add`; the
    supported managed path is a configuration profile carrying
    `com.apple.systempolicy.rule` payloads.
12. Smoke run `25562087555` verified the installed computer-level
    `TCFS FileProvider Lab Gatekeeper Rules` profile, then passed installed
    host policy probe, live config provisioning, S3/E2EE fixture checks,
    `tcfsd` startup, FileProvider registration, CloudStorage enumeration,
    `requestDownload`, `evict`, re-`requestDownload`, and exact 55-byte
    hydration.
13. Package run `25565895586` proved the explicit runner-local p12/profile path:
    p12 import, signing-asset resolution, FileProvider app build/signing,
    testing-mode entitlement verification, policy probe, and testing-mode
    `.pkg` artifact upload.
14. Smoke run `25565943781` passed the extended mutation harness: write a file
    through CloudStorage, verify local content, pull the same object from the
    configured remote prefix, and capture post-mutation `tcfs status`. The
    downloaded mutation file matched the expected 68-byte content exactly.
15. Package run `25569345240` passed on branch `codex/tcfs-parity-proof`.
    Smoke run `25569596910` reused that package artifact and passed the
    `exercise_conflict_status=true` lane: CLI status reported
    `sync state: conflict` for `ci-smoke/0.12.12/conflict-status-1.txt`, the
    FileProvider read matched expected content exactly, and the run explicitly
    captured that the FileProvider enumerator did not emit a conflict
    hydration-state log.

This is not production acceptance. It is a bounded non-production PZM proof
that the Mac App Development testing-mode lab can cross Apple's runtime policy
boundary once the managed `SystemPolicyRule` profile is installed.

## Parallel Work Packets

Each packet should produce an archived evidence directory or a linked CI run.

| Packet | Scope | Acceptance bar | Can run in parallel with |
| --- | --- | --- | --- |
| A. PZM signing/runtime-policy maintenance | macOS lab package only | Done for current lab depth: run `25562087555` proves read/evict/rehydrate, run `25565943781` proves mutation upload/readback, and run `25569596910` proves conflict/status content preservation under testing mode. | B, C, D |
| B. Linux FUSE proof | Linux real host | Done for expanded lifecycle: `docs/release/evidence/lazy-linux-20260508T170825Z/` proves `find`/`ls` before hydration, exact `cat`, mounted write/readback, cache clear, exact rehydrate, and recursive safe-unsync refusal/success | C, D, E |
| C. Safe-unsync product proof | CLI/daemon/VFS | CLI recursive unsync refuses dirty children without force and succeeds after clean state in regression tests and host lifecycle evidence | A, B, D |
| D. Distribution refresh | release surfaces | Homebrew fresh install/upgrade and Nix fresh install are refreshed for `v0.12.12` in `docs/release/evidence/distribution-v01212-20260508T205913Z/`; `.deb` proof is scoped to Ubuntu 24.04+ / Debian 13+ and remains the next full-matrix refresh item | A, B, C |
| E. Finder lifecycle depth | macOS lab after A | Evict/rehydrate is green in run `25562087555`; mutation upload/readback is green in run `25565943781`; CLI conflict state and exact FileProvider content preservation are green in run `25569596910`; badges/progress remain observational | B, C, D |
| F. On-prem authority | infra/backend | Deferred for this sprint unless a maintenance window is explicitly scheduled; when resumed, `#327` needs candidate service/cutover proof and post-cut tailnet endpoint smoke | A, B, C, D |
| G. iOS posture | product/docs | Explicit keep-as-scaffold or create a real device/Files.app lane | all |
| H. Remote/branch hygiene | repo governance | Deferred until product proof is no longer actively moving. Current pass only inspected branches; do not delete local or remote branches until the next proof packet is chosen. | A, D, E |

## SLA Bar For This Week

The minimum credible M10 proof bar is:

1. One green Linux mounted-surface evidence bundle. Done:
   `docs/release/evidence/lazy-linux-20260508T170825Z/` for the expanded
   lifecycle parity packet.
2. One green PZM FileProvider read/hydrate-or-better run from a current tag.
   Done: run `25562087555`; mutation is also green in `25565943781`.
   Conflict/status content preservation is green in `25569596910` with
   `exercise_conflict_status=true`; badges/progress remain observational.
3. One updated product-status document that does not conflate testing-mode lab
   proof with production Finder proof.
4. Linear/GitHub trackers updated with run IDs, artifact paths, and the next
   owner packet.

Anything below that is still useful engineering work, but not enough to claim
the desktop/lazy-hydration story is release-proven.
