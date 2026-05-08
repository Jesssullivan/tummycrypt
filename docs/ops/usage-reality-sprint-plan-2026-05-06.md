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
| Linux mounted FS | Green for the read lifecycle on a real FUSE host: browse remote tree, hydrate exact content, clear cache, and rehydrate. Mutate/conflict/status are still open. | `tcfs-vfs` tests, mounted helper regressions, and archived evidence `docs/release/evidence/lazy-linux-20260508T151858Z/` | Extend mounted/sync-root proof into mutation, dirty-child safe-unsync, conflict, and status reporting |
| Physical `.tc` / `.tcf` stubs | Stub wire format, parse/write, and compatibility are tested | `tcfs-vfs` and daemon unsync tests | Product-level recursive safe-unsync acceptance, including dirty-child refusal |
| macOS CLI + daemon | Package install, signing, E2EE, storage, and daemon startup are repeatedly proven | `v0.12.12` release and PZM smoke pre-harness stages | Keep install smoke green while FileProvider lab work continues |
| macOS Finder/FileProvider, lab | Green for the non-production PZM testing-mode lane through enumerate, exact-content hydrate, evict, and rehydrate. Mutation harness support is now in `main`, but the first package attempt for that depth is blocked by the PZM runner signing security domain before app build. This remains testing-mode lab proof, not production Finder proof. | PZM run `25446601375` green for read/hydrate; package run `25456290021` proves build-output host startup; smoke run `25456341985` captured the pre-profile `_dyld_start` AppleSystemPolicy block; smoke run `25458526158` proves macOS 15 rejects `spctl --add`; smoke run `25562087555` passes profile verification, installed host policy probe, shared-Keychain config, E2EE, FileProvider registration, enumeration, requestDownload, evict, re-requestDownload, and exact 55-byte hydration; package run `25564780049` proves the mutation package lane now reaches signing-asset resolution and fails there, before app build | Fix PZM runner signing domain, then rerun mutation/conflict/status/badges/recovery, while separately proving production Developer ID Finder enablement |
| macOS Finder/FileProvider, production | Not proven on arbitrary clean Developer ID hosts | Local `neo` source-tree proof and production package install/signing gates | Separate clean-host production Finder enablement from PZM testing-mode evidence |
| iOS | Proof-of-concept only | Swift build/type-check scaffold | Decide whether to keep as scaffold or create a real Files.app device lane |
| On-prem backend | Live endpoint client smoke works; source-owned migration is still open and explicitly deferred from this usage-reality sprint unless a maintenance window is scheduled | `neo-honey` smoke using MagicDNS endpoints | Keep out of the lazy/Finder proof path; schedule `#327` downtime separately, then archive post-cutover storage/NATS proof |
| Distribution parity | Release assets are publishing; per-surface install proof is uneven | Current tagged releases plus `v0.12.2` distribution matrix | Refresh Homebrew, Nix per-tag proof, and `.deb` proof for Ubuntu 24.04+ / Debian 13+ |

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
- local runner profile directory: `~/.tcfs-fileprovider-lab`
- matching p12: `~/.tcfs-fileprovider-lab/tcfs-fileprovider-lab-E9B03E55.p12`

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
13. Mutation harness support is implemented in `main`: the PZM smoke can now
    write a file through CloudStorage, verify local content, pull the same
    object from the configured remote prefix, and capture `tcfs status` after
    mutation. The first package attempt carrying this change, run
    `25564780049`, stopped before app build at signing-asset resolution.
    Current diagnosis: the PZM service/SSH security domain resolves the system
    keychain as default, while explicit user/temp keychains show identities to
    `security find-identity` but are not resolved by `codesign`.

This is not production acceptance. It is a bounded non-production PZM proof
that the Mac App Development testing-mode lab can cross Apple's runtime policy
boundary once the managed `SystemPolicyRule` profile is installed.

## Parallel Work Packets

Each packet should produce an archived evidence directory or a linked CI run.

| Packet | Scope | Acceptance bar | Can run in parallel with |
| --- | --- | --- | --- |
| A. PZM signing/runtime-policy maintenance | macOS lab package only | Read lifecycle is done for the non-production profile-backed lane: run `25562087555` is green and archived. Mutation-depth rerun is blocked on the runner signing security domain from run `25564780049`. | B, C, D |
| B. Linux FUSE proof | Linux real host | Done for read lifecycle: `docs/release/evidence/lazy-linux-20260508T151858Z/` proves `find`/`ls` before hydration, exact `cat`, cache clear, and exact rehydrate | C, D, E |
| C. Safe-unsync product proof | CLI/daemon/VFS | Recursive unsync refuses dirty children without force, succeeds after clean state, and rehydrates exact content | A, B, D |
| D. Distribution refresh | release surfaces | Homebrew fresh install/upgrade refreshed, `.deb` proof scoped to Ubuntu 24.04+ / Debian 13+, Nix proof recorded or explicitly scoped | A, B, C |
| E. Finder lifecycle depth | macOS lab after A | Evict/rehydrate is green in run `25562087555`; next add mutation, conflict/status, badges/progress, and recovery evidence through FileProvider | B, C, D |
| F. On-prem authority | infra/backend | Deferred for this sprint unless a maintenance window is explicitly scheduled; when resumed, `#327` needs candidate service/cutover proof and post-cut tailnet endpoint smoke | A, B, C, D |
| G. iOS posture | product/docs | Explicit keep-as-scaffold or create a real device/Files.app lane | all |
| H. Remote/branch hygiene | repo governance | Deferred until product proof is not actively moving. Current pass only inspected branches; do not delete local or remote branches while PZM mutation proof is blocked. | A, D, E |

## SLA Bar For This Week

The minimum credible M10 proof bar is:

1. One green Linux mounted-surface evidence bundle. Done:
   `docs/release/evidence/lazy-linux-20260508T151858Z/`.
2. One green PZM FileProvider read/hydrate-or-better run from a current tag.
   Done: run `25562087555`.
3. One updated product-status document that does not conflate testing-mode lab
   proof with production Finder proof.
4. Linear/GitHub trackers updated with run IDs, artifact paths, and the next
   owner packet.

Anything below that is still useful engineering work, but not enough to claim
the desktop/lazy-hydration story is release-proven.
