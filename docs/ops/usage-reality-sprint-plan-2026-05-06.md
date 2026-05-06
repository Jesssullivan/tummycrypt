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
| Linux mounted FS | Code and in-process VFS tests are strong; real-host FUSE evidence is not yet archived for the full user story | `tcfs-vfs` tests and mounted helper regressions | Run `task lazy:linux-demo` on a FUSE-capable host and archive evidence |
| Physical `.tc` / `.tcf` stubs | Stub wire format, parse/write, and compatibility are tested | `tcfs-vfs` and daemon unsync tests | Product-level recursive safe-unsync acceptance, including dirty-child refusal |
| macOS CLI + daemon | Package install, signing, E2EE, storage, and daemon startup are repeatedly proven | `v0.12.12` release and PZM smoke pre-harness stages | Keep install smoke green while FileProvider lab work continues |
| macOS Finder/FileProvider, lab | `v0.12.11` proved testing-mode enumerate/hydrate; `v0.12.12` evict/rehydrate attempt is blocked by Gatekeeper/AppleSystemPolicy for the Mac Development app, before the direct host launch reaches Swift `main()` and after `fileproviderd` starts the extension | PZM run `25446601375` green for read/hydrate; run `25453088909` shows valid host/extension signatures and profiles, `taskgated-helper` profile acceptance, `spctl` rejection, provenance xattrs, and AppleSystemPolicy denial for both host and extension processes | Decide the lab trust model: Xcode/local-development launch path, a dedicated Apple-approved distribution shape, or an explicit non-production Gatekeeper bypass on PZM |
| macOS Finder/FileProvider, production | Not proven on arbitrary clean Developer ID hosts | Local `neo` source-tree proof and production package install/signing gates | Separate clean-host production Finder enablement from PZM testing-mode evidence |
| iOS | Proof-of-concept only | Swift build/type-check scaffold | Decide whether to keep as scaffold or create a real Files.app device lane |
| On-prem backend | Live endpoint client smoke works; source-owned migration is still open | `neo-honey` smoke using MagicDNS endpoints | Complete `#327`/`#298` migration and archive post-cutover storage/NATS proof |
| Distribution parity | Release assets are publishing; per-surface install proof is uneven | Current tagged releases plus `v0.12.2` distribution matrix | Refresh Homebrew, Debian 12 support-floor decision, Nix per-tag proof |

## Apple Lab Ground Truth

The PZM lab is no longer blocked on "no certificate" or "wrong profile."
The current lab material is:

- runner: `petting-zoo-mini-tcfs`
- device UDID: `00008132-001240C80138801C`
- team: `QP994XQKNH`
- Mac App Development certificate SHA-1:
  `4EC8EA7AF447944F877F13FC6A9318AED8A448DF`
- host profile: `tcfs-host-development-testing-mode-pzm-4ec8ea7a`
- extension profile: `tcfs-fileprovider-development-pzm-4ec8ea7a`

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

Current blocker:

1. `taskgated-helper` accepts the host and extension profiles for
   `io.tinyland.tcfs` and `io.tinyland.tcfs.fileprovider`.
2. The direct host-app launch writes no host stderr event, so it is not reaching
   the instrumented Swift startup path.
3. `spctl --assess --type execute` rejects both the host app and extension.
4. The installed app tree carries `com.apple.provenance` xattrs, but stripping
   provenance/quarantine from a temporary copy does not make Gatekeeper accept
   the Mac Development signature.
5. `syspolicy_check distribution` reports a missing notarization ticket.
6. `syspolicy_check notary-submission` reports a fatal Gatekeeper rejection for
   `TCFSProvider.app/Contents/MacOS/TCFSProvider`.
7. `fileproviderd` starts the extension process.
8. AppleSystemPolicy terminates both processes:
   `Security policy would not allow process ... TCFSProvider` and
   `Security policy would not allow process ... TCFSFileProvider`.
9. The current evidence points to a Mac Development-vs-Gatekeeper trust-model
   issue, not to the TCFS storage, E2EE, daemon, profile, or entitlement layers.

## Parallel Work Packets

Each packet should produce an archived evidence directory or a linked CI run.

| Packet | Scope | Acceptance bar | Can run in parallel with |
| --- | --- | --- | --- |
| A. PZM runtime-policy diagnosis | macOS lab package only | Run PZM smoke with host stderr diagnostics, `spctl`, `syspolicy_check`, xattrs, codesign, embedded profile, `taskgated`, `amfid`, and AppleSystemPolicy logs attached | B, C, D |
| B. Linux FUSE proof | Linux real host | `find`/`ls` before hydration, exact `cat`, cache clear or unsync, exact rehydrate, evidence archived | A, C, D |
| C. Safe-unsync product proof | CLI/daemon/VFS | Recursive unsync refuses dirty children without force, succeeds after clean state, and rehydrates exact content | A, B, D |
| D. Distribution refresh | release surfaces | Homebrew fresh install/upgrade refreshed, Debian 12 posture decided, Nix proof recorded or explicitly scoped | A, B, C |
| E. Finder lifecycle depth | macOS lab after A | Evict/rehydrate, mutation, conflict/status evidence through FileProvider | B, C, D; depends on A |
| F. On-prem authority | infra/backend | `#327` candidate service/cutover proof and post-cut tailnet endpoint smoke | A, B, C, D |
| G. iOS posture | product/docs | Explicit keep-as-scaffold or create a real device/Files.app lane | all |

## SLA Bar For This Week

The minimum credible M10 proof bar is:

1. One green Linux mounted-surface evidence bundle.
2. One green PZM FileProvider read/hydrate-or-better run from a current tag, or
   a documented Apple runtime-policy blocker with complete signing evidence.
3. One updated product-status document that does not conflate testing-mode lab
   proof with production Finder proof.
4. Linear/GitHub trackers updated with run IDs, artifact paths, and the next
   owner packet.

Anything below that is still useful engineering work, but not enough to claim
the desktop/lazy-hydration story is release-proven.
