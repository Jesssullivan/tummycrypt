# macOS Finder and FileProvider Reality

As of May 18, 2026, the production Developer ID FileProvider hydration path
is proven on a clean install of the notarized `.pkg`. GitHub Actions run
`26061402177` on the registered `petting-zoo-mini-tcfs` self-hosted runner
passed the full post-install smoke with `fileprovider_testing_mode=false`:
installed strict preflight, storage `[ok]`, host-app domain add, CloudStorage
enumeration, host-app `requestDownload`, and exact-content hydration of a
55-byte seeded fixture through the installed `/Applications/TCFSProvider.app`.
That replaces the previous "production Dev ID Finder hydration is the open
blocker" status that this document has carried since the May 17 packets.

The non-production PZM testing-mode FileProvider lane remains proven on
`v0.12.12` through enumerate, exact-content hydrate, evict, rehydrate,
mutation upload/readback, and deterministic CLI conflict/status content
preservation when the lab `SystemPolicyRule` profile is installed. The
production Dev ID lane has now proven the same enumerate plus exact-content
hydrate path, but evict/rehydrate, mutation upload/readback, conflict/status
content preservation, badge visibility, and continuously exercised release-day
viability are still open on the Dev ID lane (TIN-133 follow-ups this week).

This document defines the actual workflow the repo supports today, separates
what is proven from what remains experimental, and records the highest-value
smoke path for the Finder/FileProvider surface.
For the current PZM GitHub Actions run links, see the
[Release Evidence Index](../release/evidence/README.md).

## 2026-05-18 — Production Dev ID FileProvider hydration PROVEN

Run `26061402177` of `macos-postinstall-smoke.yml` on the registered
`petting-zoo-mini-tcfs` self-hosted runner installed the notarized arm64
`.pkg` built by run `26057944325` from main commit `c08a0a4` (PR #370,
which added `tcfs index inspect` + `--seed-expected-file` and the
`require_expected_remote_index` gate) and passed the full strict harness
with `fileprovider_testing_mode=false` — the production Developer ID lane,
not Mac App Development testing-mode.

Concrete evidence from `/tmp/tcfs-smoke-26061402177/macos-postinstall-smoke-v0.12.12/`:

- `harness/expected-file-index.json` reports `status: "visible"`,
  `entry_state: "committed"`, `manifest_exists: true`, `size: 55`, `chunks: 1`
  for `ci-smoke/0.12.12/postinstall-1.txt` under remote prefix
  `gha/macos-postinstall/v0.12.12/26061402177-1`, so the seeded remote index
  gate fired before FileProvider was asked to download anything.
- `harness/hydrated-expected-file` contains exactly the 55 expected bytes:
  `tcfs macOS post-install smoke v0.12.12 run 26061402177`.
- `harness/hydrate-read-error.log` is empty (no `Operation timed out`,
  no coordinated-read failure).

Four stacked unblocks were required to get the production Dev ID lane to
green; none of them are loadbearing on a single commit, and all of them are
worth recording because the same classes will recur:

1. Backend endpoint moved from the temporary Cloudflare quick tunnel to the
   tailnet hostname `http://seaweedfs-tcfs:8333` so the self-hosted runner
   could actually reach SeaweedFS without a public ingress.
2. The `gh secret set` invocation syntax was corrected so the new endpoint
   actually landed on the `tcfs-macos-smoke` environment.
3. The post-install smoke workflow learned to derive `enforce_tls` from the
   endpoint scheme (commit `0b1dc0c`) so a tailnet `http://` endpoint is not
   rejected as plaintext while public `https://` endpoints remain strict.
4. The stale per-user `~/Applications/TCFSProvider.app` installed on
   `petting-zoo-mini-tcfs` on May 9 was removed so PlugInKit reported a single
   registration parented by `/Applications/TCFSProvider.app`.

Honest scope for this milestone:

- **Proven now on Dev ID:** installed strict preflight, storage health,
  PlugInKit single-registration, domain add, CloudStorage enumeration,
  host-app `requestDownload`, exact-content hydration through the installed
  app, shared-Keychain config-source extension log, and the remote-index
  visibility gate from PR #370.
- **Not yet proven on Dev ID:** evict + rehydrate, CloudStorage mutation
  upload/readback, deterministic CLI conflict/status content preservation,
  badge/progress assertions as a release gate, recovery UX, and continuous
  release-day viability of every published macOS artifact without explicit
  post-cut smoke. These layered proofs exist today only in the PZM Mac App
  Development testing-mode lane and are TIN-133 work this week.

A sibling evidence packet under `docs/release/evidence/` archives the full
artifact tree for this run; see the Release Evidence Index for the link once
that packet lands.

## Supported Workflow In The Repo Today

The macOS FileProvider path currently consists of these pieces:

1. A packaged host app: `TCFSProvider.app`
2. A packaged non-UI FileProvider extension:
   `io.tinyland.tcfs.fileprovider`
3. A host-app registration step that adds or updates the
   `io.tinyland.tcfs` FileProvider domain on launch
4. A daemon and FileProvider socket path that the extension uses for
   enumeration, hydration, and watch signaling

In practical terms, the intended operator flow is:

1. install the macOS package or app bundle
2. ensure `tcfsd` is present and can start with the needed config
3. ensure the containing app is registered with LaunchServices so PlugInKit
   discovers one parented FileProvider extension record
4. launch `TCFSProvider.app` so the host app provisions config and re-adds the
   FileProvider domain
5. let `fileproviderd` enumerate the domain into `~/Library/CloudStorage/`
6. use Finder to enumerate and open items, which should hydrate on demand

Finder should expose FileProvider items as normal filenames backed by platform
placeholders / APFS dataless files. Raw `.tc` suffixes are the physical
sync-root stub representation, not the desired primary Finder UX.

## Proven Today

- CI proves the Rust staticlib/header required by the macOS FileProvider bridge
  and separately type-checks the iOS Swift lane. The regular CI workflow does
  not yet build the macOS FileProvider Swift bundle.
- Runner split: CI and release build jobs currently use GitHub-hosted
  `macos-14`; the production post-install smoke defaults to GitHub-hosted
  `macos-15`; the PZM testing-mode package/smoke lanes run on the registered
  self-hosted `petting-zoo-mini` Mac.
- Release automation builds `TCFSProvider.app`, packages it into the Apple
  Silicon `.pkg`, and asks LaunchServices to register the containing app in the
  active console user's context. The
  package builder source is
  [`scripts/macos-build-pkg.sh`](../../scripts/macos-build-pkg.sh), and the
  postinstall script source is
  [`scripts/macos-pkg-postinstall.sh`](../../scripts/macos-pkg-postinstall.sh).
- The host app contains a real domain-registration path: it adds/updates
  `NSFileProviderDomainIdentifier("io.tinyland.tcfs")` on launch without
  removing the existing domain, then signals the replicated FileProvider
  working set so existing domains refresh from remote state.
- For harness-driven hydration, the host app can request a full FileProvider
  download when launched with
  `TCFS_FILEPROVIDER_REQUEST_DOWNLOAD_IDENTIFIER`. This uses
  `NSFileProviderManager.requestDownloadForItem` from the containing app after
  the expected placeholder exists, then the harness verifies the hydrated bytes
  through a coordinated read.
- The extension contains real enumeration, hydration, watch, and badge
  decoration code paths.
- The non-production `petting-zoo-mini` testing-mode lane has passed a full
  package-to-FileProvider read/hydrate smoke on `v0.12.11`.
- The `v0.12.12` PZM testing-mode lane passes package install, signing,
  profile, E2EE, storage, daemon startup, installed host policy probe,
  FileProvider registration, enumeration, requestDownload, evict, re-request,
  exact-content hydration, CloudStorage mutation upload/readback, and
  deterministic CLI conflict/status content preservation when the
  computer-level `TCFS FileProvider Lab Gatekeeper Rules` `SystemPolicyRule`
  profile is installed. This is a non-production Mac App
  Development/testing-mode proof, not a production Developer ID clean-host
  claim.
- The production Developer ID source-package lane now has real notarization,
  stapling, Gatekeeper install assessment, and strict package-smoke evidence
  from GitHub Actions run `25973109986`.
- `neo` has installed that notarized workflow artifact into
  `/Applications/TCFSProvider.app` with authenticated `osascript`, quarantined
  the stale user app after inventory, and passed strict installed preflight
  with one PlugInKit registration.
- `neo` package daemon proof now shows `/usr/local/bin/tcfsd 0.12.12`
  reaching storage `[ok]` from file-backed credentials after removing the
  stale user daemon.
- The production-signed Finder smoke now reaches host-app domain add,
  CloudStorage enumeration, and host-app `requestDownload` for
  `shared/alpha-test.txt`. (Updated 2026-05-18: the May 17 raw-read
  `Operation timed out` blocker is closed on the hosted self-hosted lane; run
  `26061402177` hydrated the seeded fixture through the installed Dev ID
  package. See the milestone section above.)
- The smoke harness now gates expected-file hydration on a read-only remote
  index check before it asks FileProvider to download the item. The diagnostic
  command is `tcfs index inspect <relative-path> --json`; it reports
  `visible`, `missing_index`, `missing_manifest`, `preparing_only`,
  `no_visible_entry`, or `parse_error` without promoting pending remote index
  records. Future production Finder packets should archive
  `expected-file-index.json` so a missing remote fixture is separated from a
  real FileProvider read failure.

## Important Constraints

- The package postinstall script only auto-registers the containing app if it is
  installed at `/Applications/TCFSProvider.app`.
- The April 15, 2026 smoke path that used
  `installer -target CurrentUserHomeDirectory` landed the app at
  `~/Applications/TCFSProvider.app`, so that install path should be treated as
  requiring manual app launch and manual verification.
- The host app provisions config from `~/.config/tcfs/fileprovider/config.json`
  into the shared app-group keychain as a best-effort startup step.
- The repo now uses explicit shared-keychain semantics for that config, but the
  clean-machine validity of the `group.io.tinyland.tcfs` app-group entitlement
  is still only as good as the signing and provisioning reality of the shipped
  app bundle.

## Not Yet Proven

- A **continuously** exercised production clean-host Finder/FileProvider
  acceptance lane from Developer ID package install through user enablement,
  register, enumerate, hydrate, mutate, and conflict handling (updated
  2026-05-18: single-run install + enumerate + hydrate is now proven on the
  Dev ID lane by run `26061402177`; continuous coverage and the mutate/conflict
  legs are still open)
- A production clean-host Finder/FileProvider lifecycle smoke beyond
  install/signing/storage/domain-add/enumeration/requestDownload/hydrate gates
  (updated 2026-05-18: hydrate is now inside the proven set on Dev ID; what
  remains is evict/rehydrate, mutation upload/readback, and conflict-status
  preservation)
- Finder badge visibility as a release gate
- Conflict UX, notification behavior, and Finder badge/progress visibility as
  release gates
- Release-day viability of every published macOS artifact without explicit
  post-cut smoke
- A stable claim that write flows are supported for end users on macOS

## Current Local Preflight Notes

April 30, 2026 non-mutating preflight on `neo` found:

- ambient binaries are present but report `0.12.0`:
  `tcfs` resolves to `~/.local/bin/tcfs`, and `tcfsd` resolves to
  `~/.nix-profile/bin/tcfsd`; pass `EXPECTED_VERSION=...` or explicit
  `TCFS_BIN` / `TCFSD_BIN` paths when release-smoking a newer package
- `~/Applications/TCFSProvider.app` is present
- `~/.config/tcfs/fileprovider/config.json` is present
- `~/Library/CloudStorage/TCFSProvider-TCFS` is present
- initial verbose `pluginkit` output reported both `0.2.0` and `0.1.0`
  registrations; the stale `0.1.0` registration resolved to
  `/Users/jess/git/tummycrypt/build/TCFSProvider.app`
- after explicitly unregistering the stale build appex with `pluginkit -r`,
  current verbose `pluginkit` output shows one `0.2.0` registration under
  `~/Applications/TCFSProvider.app`
- `task lazy:macos-finder-preflight-workspace` builds local `tcfs` / `tcfsd`
  and passes with `target/debug/tcfs` and `target/debug/tcfsd` reporting
  `0.12.2`
- `fileproviderctl domain list` is not a usable command form on this macOS
  version; the harness treats that check as optional and relies on host log plus
  CloudStorage root when the command is unavailable

This is not clean-host acceptance. Before claiming a local Finder/FileProvider
pass on `neo`, run the preflight, ensure `pluginkit` still reports one
registration for `io.tinyland.tcfs.fileprovider`, point the smoke at the intended
package binaries/version, then run the named smoke with `--expected-content-file`.

April 30, 2026 local source-tree smoke on `neo` then passed the full named
Finder/FileProvider lane with exact content hydration:

- app: `/Users/jess/Applications/TCFSProvider.app`
- extension registration: exactly one `io.tinyland.tcfs.fileprovider`
  registration under that app
- CloudStorage root: `/Users/jess/Library/CloudStorage/TCFSProvider-TCFS`
- fixture: `finder-smoke-20260430T0305Z/finder-smoke.txt`
- result: CloudStorage enumeration succeeded, `cat` through FileProvider
  hydrated 120 bytes, and the hydrated content matched the expected fixture

Evidence is recorded in
[macOS FileProvider Local Evidence](../release/macos-fileprovider-local-evidence-2026-04-30.md).
This remains a workstation/source-tree proof because the running daemon was
already present and reported `0.12.0`, but the active app used for the latest
pass is Developer ID signed, embeds matching host/extension provisioning
profiles, disables build-time embedded FileProvider config, and proves runtime
config loaded from the shared Keychain. Do not treat it as clean-host `.pkg` or
notarization proof.

The historical no-embedded-config investigation resolved the local signing and
Keychain blockers for that source-tree app path:

- the host app can now enrich the Keychain config from `master_key_file`
- ad-hoc builds cannot carry `keychain-access-groups`; macOS rejects those
  restricted entitlements before launch
- Developer ID signing without a matching provisioning profile still fails with
  `amfid` "No matching profile found"
- App Group file fallback is not enough on this host because the extension is
  denied permission to read `config.json`
- matching Developer ID profiles are now installed locally for both
  `io.tinyland.tcfs` and `io.tinyland.tcfs.fileprovider`
- that source-tree release smoke passed with exact-content hydration and
  `loadConfig: loaded from shared Keychain`

The May 10 neo cleanup packet below supersedes this as current local-host
readiness. The next production acceptance step is still packaging/clean-host
proof, not another raw-key diagnostic build.

`swift/fileprovider/build.sh` now has explicit provisioning-profile hooks for
that step:

```bash
TCFS_HOST_PROVISIONING_PROFILE=/path/to/host.provisionprofile \
TCFS_EXTENSION_PROVISIONING_PROFILE=/path/to/fileprovider.provisionprofile \
TCFS_REQUIRE_PRODUCTION_SIGNING=1 \
swift/fileprovider/build.sh target/release path/to/tcfs_file_provider.h build/fileprovider auto
```

When `TCFS_REQUIRE_PRODUCTION_SIGNING=1` is set, the build script runs the same
signing-only strict preflight against the assembled app before it reports
success. It also disables build-time embedded FileProvider config by default so
the Finder proof exercises host-app Keychain provisioning rather than the
diagnostic embedded-config path. Diagnostic evidence may opt back in with
`TCFS_EMBED_FILEPROVIDER_CONFIG=1` only when
`TCFS_ALLOW_PRODUCTION_EMBEDDED_CONFIG=1` is set as an explicit override.

The required profile inputs are concrete:

| Bundle | Identifier | Required entitlements |
|--------|------------|-----------------------|
| Host app | `io.tinyland.tcfs` | App Group `group.io.tinyland.tcfs`; Keychain group `$(AppIdentifierPrefix)group.io.tinyland.tcfs` |
| FileProvider extension | `io.tinyland.tcfs.fileprovider` | App Sandbox; network client; App Group `group.io.tinyland.tcfs`; Keychain group `$(AppIdentifierPrefix)group.io.tinyland.tcfs` |

The host app and extension profiles must come from the same Apple team prefix
so the runtime Keychain access-group entitlement resolves to the same concrete
value in both processes.

Apple's 2026 App ID UI does not expose a separate "Keychain Sharing" checkbox
for this macOS App ID shape. The portal-side requirement is App Groups with
`group.io.tinyland.tcfs` assigned to both App IDs. The keychain requirement is
still real: the signed bundles and embedded provisioning profiles must carry
the concrete `keychain-access-groups` value ending in
`.group.io.tinyland.tcfs`, and strict preflight verifies that after build.

When the Apple Developer portal shows multiple Developer ID Application
certificates with the same display name or expiry date, do not pick one by
guessing. Download the candidate `.cer` files and match them against the local
Keychain identity first:

```bash
scripts/macos-developer-cert-match.sh ~/Downloads/developer-id-*.cer
```

Use the certificate marked with `*` when creating the host and FileProvider
Developer ID provisioning profiles.

Before building, inventory locally installed profiles:

```bash
task lazy:macos-finder-profile-inventory
```

That helper scans `~/Library/MobileDevice/Provisioning Profiles`, decodes each
profile, and emits `TCFS_HOST_PROVISIONING_PROFILE=...` plus
`TCFS_EXTENSION_PROVISIONING_PROFILE=...` when it finds a compatible pair. On
the current local `neo` host, it finds the Developer ID profile pair:

- host profile UUID `8e93c5be-685f-4503-bf0a-d647a2062149`
- extension profile UUID `fa455f84-5e7d-4a14-9d4f-68a26c6a9939`

Apple's direct-distribution profiles expose the profile keychain group as
`QP994XQKNH.*`; the strict preflight accepts that only as a team wildcard that
covers the concrete signed entitlement
`QP994XQKNH.group.io.tinyland.tcfs`.

For GitHub release builds, store the downloaded profiles as base64-encoded
Actions secrets:

```bash
base64 -i ~/Downloads/tcfs-host-developer-id.provisionprofile \
  | gh secret set TCFS_HOST_PROVISIONING_PROFILE_BASE64

base64 -i ~/Downloads/tcfs-fileprovider-developer-id.provisionprofile \
  | gh secret set TCFS_EXTENSION_PROVISIONING_PROFILE_BASE64
```

When `APPLE_CERTIFICATE_BASE64` is configured, the release workflow now treats
those two profile secrets as required, inventories the decoded profiles as a
compatible host/extension pair, and runs the same strict profile-backed signing
preflight during `swift/fileprovider/build.sh`.
`task lazy:check` includes `scripts/test-release-workflow-fileprovider.sh`,
which extracts the release workflow's profile-import step, verifies the package
builder delegates to `scripts/macos-build-pkg.sh`, verifies the package
postinstall source, and regression-tests those paths outside GitHub Actions.
For a built package artifact, `task lazy:macos-pkg-structure-smoke` provides a
non-installing structure check before moving to a clean-host Finder run.

For a production build on a host that already has matching profiles installed,
the build script can discover the pair automatically:

```bash
TCFS_AUTO_PROVISIONING_PROFILES=1 \
TCFS_REQUIRE_PRODUCTION_SIGNING=1 \
swift/fileprovider/build.sh target/release path/to/tcfs_file_provider.h build/fileprovider auto
```

The build script deliberately resolves the host Xcode SDK, `swiftc`, and
`clang` through system `xcrun` instead of inheriting Nix `DEVELOPER_DIR` /
`SDKROOT`. This keeps local Nix dev shells from pairing a Nix SDK with a newer
Apple Swift compiler.

`TCFS_CODESIGN_TIMESTAMP=0` may be used for a local diagnostic Developer ID
build when timestamping is unavailable. Do not use that for release evidence.

The non-mutating preflight helper is:

```bash
task lazy:macos-finder-preflight
```

It intentionally does not launch the app or change FileProvider domain state.
Use it before `task lazy:macos-finder-smoke` to identify stale app bundles,
missing configs, duplicate extension registrations, and CloudStorage ambiguity.
By default it warns on missing profile-backed signing material so diagnostic
local apps remain inspectable. For release evidence and the no-embedded-config lane,
make those checks fatal:

```bash
TCFS_REQUIRE_PRODUCTION_SIGNING=1 task lazy:macos-finder-preflight
```

Strict mode requires valid codesign verification for both the host app and
FileProvider extension, a `keychain-access-groups` entitlement on both bundles,
and an embedded provisioning profile in each bundle. It also decodes the
embedded profiles and cross-checks the App Group, Keychain access group, bundle
identifier, Apple team prefix, and profile `DeveloperCertificates` list so a
mismatched profile or wrong Developer ID certificate cannot satisfy the gate by
presence alone.

To validate a built app bundle before installing or registering it, use the
signing-only task. It skips `tcfs` version/config checks, `pluginkit`,
`fileproviderctl`, and CloudStorage discovery:

```bash
APP_PATH=build/fileprovider/TCFSProvider.app \
TCFS_REQUIRE_PRODUCTION_SIGNING=1 \
task lazy:macos-finder-signing-preflight
```

The May 17, 2026 neo packets supersede the older local-app assumption:
`/Applications/TCFSProvider.app` is now installed from the notarized workflow
artifact, the stale user app has been quarantined after inventory, and strict
installed preflight passes. (Updated 2026-05-18: on the hosted
`petting-zoo-mini-tcfs` self-hosted runner — which uses the same notarized
Dev ID `.pkg` artifact path — run `26061402177` now proves exact FileProvider
read/hydration end to end. See the milestone section at the top of this doc.
A neo-local replay of that hydrated path is still useful workstation
acceptance evidence, but it is no longer the gating proof.)

Current neo evidence:

- `docs/release/evidence/macos-fileprovider-neo-cleanup-20260516T073644Z/`
  is a historical canonical-install preflight packet from before the May 17
  install. It confirms
  `/Applications/TCFSProvider.app` is absent and therefore strict production
  preflight fails before signing checks. The same inventory records the only
  visible app location as `~/Applications/TCFSProvider.app`, PlugInKit parented
  to that user app, ambient `tcfs` from the workspace at `0.12.12`, and ambient
  `tcfsd` from the Nix profile at `0.12.2`.
- `docs/release/evidence/macos-fileprovider-userapp-preflight-20260516T073758Z/`
  reruns strict preflight against the registered user app. Codesign validation
  passes and App Group entitlements are present, but strict production
  preflight fails because both host and extension lack
  `keychain-access-groups` entitlements and embedded provisioning profiles.
  `STRICT=1 task lazy:macos-finder-profile-inventory` finds a compatible local
  Developer ID host/extension profile pair, so the next packaging step is to
  embed/sign with those profiles and install into `/Applications`.
- `docs/release/evidence/macos-fileprovider-signed-app-preflight-20260516T183213Z/`
  builds the Rust FileProvider bridge and assembles a source-built
  `TCFSProvider.app` with the compatible local Developer ID host/extension
  profiles embedded. Direct strict signing-only preflight passes for host and
  extension codesign, App Group entitlements, concrete
  `QP994XQKNH.group.io.tinyland.tcfs` keychain-access-groups entitlements, and
  embedded profile/signing-certificate checks. This closes the local
  source-built signing/profile blocker, but it is not an installed `.pkg`,
  does not touch PlugInKit, and does not prove Finder lifecycle.
- `docs/release/evidence/macos-fileprovider-candidate-pkg-20260516T190702Z/`
  wraps that signed source-built app with current source-built
  `tcfs`/`tcfsd 0.12.12` into a local candidate `.pkg`. Package structure smoke
  passes for `usr/local/bin/tcfs`, `usr/local/bin/tcfsd`,
  `/Applications/TCFSProvider.app`, the FileProvider appex, and the repo
  postinstall script. `pkgutil --check-signature` reports Developer ID
  Installer signing with a trusted timestamp. This is package-shape/signature
  proof only; it still does not install, register, launch, or exercise Finder.
- `docs/release/evidence/macos-fileprovider-candidate-pkg-assessment-20260516T194612Z/`
  assesses that candidate without installing it. `pkgutil --check-signature`
  still passes and `pkgutil --expand-full` shows the expected payload, but
  `spctl --assess --type install` rejects the package as
  `Unnotarized Developer ID` and `xcrun stapler validate` reports no stapled
  ticket. This remains useful historical blocker evidence.
- `docs/release/evidence/macos-fileprovider-pkg-notarization-proof-20260516T211425Z/`
  is the first remote source-package notarization packet. GitHub Actions run
  `25973109986` built the arm64 macOS package from source on `macos-14`,
  imported Developer ID Application and Installer identities plus FileProvider
  profiles, passed strict signing-only FileProvider preflight, submitted the
  signed `.pkg` to Apple with `xcrun notarytool submit --wait`, received
  `Accepted`, stapled and validated the ticket, passed
  `spctl --assess --type install` with `source=Notarized Developer ID`, and
  passed `scripts/macos-pkg-structure-smoke.sh --require-signature
  --require-gatekeeper-install --require-stapled-ticket`. This proves
  notarization/stapling/Gatekeeper package acceptance is real for the workflow
  artifact. It still does not install into `/Applications`, clean PlugInKit, or
  prove Finder lifecycle.
- `docs/release/evidence/macos-fileprovider-neo-notarized-pkg-inventory-20260516T222519Z/`
  downloads the notarized workflow artifact onto `neo` and validates it
  locally. The SHA-256 is
  `c6fd1a6fd18638c53f0d0b88bc79249e65d08766d99853bef6896ee69bcd6d45`, and
  local strict package smoke passes with signature, Gatekeeper install
  assessment, and stapled-ticket checks required. The same packet records that
  `/Applications/TCFSProvider.app` is still absent, PlugInKit is still parented
  by `~/Applications/TCFSProvider.app`, and ambient `tcfs`/`tcfsd` resolve to
  `0.12.2`.
- `docs/release/evidence/macos-fileprovider-neo-notarized-pkg-install-20260516T222606Z/`
  attempts the real local install from that notarized artifact. The installer
  command fails before payload installation because `sudo -n installer` reports
  `sudo: a password is required`; strict preflight then fails on the missing
  `/Applications/TCFSProvider.app`. This is retained as the historical
  non-interactive install blocker, not a Finder/FileProvider lifecycle claim.
- `docs/release/evidence/macos-fileprovider-neo-notarized-pkg-install-auth-20260517T005618Z/`
  supersedes the admin-auth blocker for the workflow artifact. It installs the
  notarized package into `/Applications` with authenticated `osascript`.
  Strict preflight verifies the installed app signing/profile material but
  still fails because PlugInKit reports both the canonical app and the stale
  user app.
- `docs/release/evidence/macos-fileprovider-neo-stale-userapp-quarantine-20260517T010423Z/`
  intentionally moves the stale user app after the install packet exists.
  PlugInKit still reports the quarantined path, so preflight remains red.
- `docs/release/evidence/macos-fileprovider-neo-strict-preflight-installed-20260517T010916Z/`
  is the first green strict installed preflight against
  `/Applications/TCFSProvider.app`: `/usr/local/bin/tcfs` and
  `/usr/local/bin/tcfsd` report `0.12.12`, host and extension codesign/profile
  checks pass, and one PlugInKit registration is parented by the canonical app.
- `docs/release/evidence/macos-fileprovider-neo-package-daemon-env-20260517T012916Z/`
  records the package daemon environment fix. Before remediation, the old
  user-app daemon and package daemon were both present and storage was
  `[UNREACHABLE]`; after booting out the stale daemon and providing file-backed
  credentials to launchd, only `/usr/local/bin/tcfsd` remained and storage was
  `[ok]`.
- `docs/release/evidence/macos-fileprovider-neo-finder-release-smoke-20260517T013241Z/`
  shows why the harness needed a deterministic direct-host launch path: normal
  `open` reached strict preflight and storage `[ok]`, then stalled before a
  useful FileProvider lifecycle result.
- `docs/release/evidence/macos-fileprovider-neo-finder-release-smoke-directhost-20260517T015411Z/`
  proves direct host-app launch can add the domain, enumerate CloudStorage, and
  request the expected file download, but the run was terminated before read
  proof.
- `docs/release/evidence/macos-fileprovider-neo-finder-release-smoke-directhost-20260517T020246Z/`
  repeats the direct-host lane and blocks before FileProvider read because the
  Swift coordinated-read helper picked up a mismatched Nix SDK/toolchain
  (`SwiftShims` missing). `fileproviderctl check` also reports reconciliation
  failures on `1/129` files.
- `docs/release/evidence/macos-fileprovider-neo-finder-release-smoke-directhost-catread-20260517T020417Z/`
  disables the coordinated Swift helper and was the production Finder blocker
  packet through 2026-05-17: strict preflight, storage `[ok]`, domain add,
  CloudStorage enumeration, and host-app `requestDownload` all happen, then
  plain `cat` of `shared/alpha-test.txt` fails with `Operation timed out`.
  (Updated 2026-05-18: superseded by hosted self-hosted run `26061402177`,
  which hydrates a seeded fixture end to end through the installed Dev ID
  package; this neo-local packet is now retained as historical context for the
  read-timeout class, not as the live blocker.)
- `docs/release/evidence/macos-fileprovider-neo-preflight-20260516T023852Z/`
  refreshes the divergence inventory. At the start of this packet the visible
  PlugInKit registration still pointed at
  `~/Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex`;
  the app was ad-hoc from Gatekeeper's perspective, strict production preflight
  failed on missing host/extension Keychain access-group entitlements and
  embedded provisioning profiles, ambient `tcfs` was workspace `0.12.12`, and
  ambient `tcfsd` was still `0.12.2` from the Nix profile.
- `docs/release/evidence/macos-fileprovider-neo-pkg-install-20260516T024006Z/`
  verifies the published `v0.12.12` `.pkg` checksum/signature/notarization,
  quarantines the stale user app under the evidence packet, and records the
  remaining local blocker: `sudo -n installer` failed because a password is
  required. `/Applications/TCFSProvider.app` was therefore not installed by
  this non-interactive run.
- `docs/release/evidence/macos-fileprovider-neo-cleanup-20260510T003148Z/`
  inventories PATH resolution, app locations, PlugInKit records, CloudStorage
  roots, configs, sockets, launchd labels, and a bounded `~/tcfs` tree before
  any cleanup.
- `docs/release/evidence/macos-fileprovider-neo-cleanup-pkg-20260510T0036Z/`
  verifies the published `v0.12.12` `.pkg` signature, notarization, payload
  shape, and postinstall script without installing it.
- `docs/release/evidence/macos-fileprovider-strict-preflight-blocker-20260510T0040Z/`
  records strict preflight failure against the existing user app: missing
  host/extension keychain access-group entitlements and missing embedded
  provisioning profiles.
- `docs/release/evidence/macos-fileprovider-neo-cleanup-install-blocker-20260510T0048Z/`
  records the non-interactive install blocker: `sudo -n installer` required a
  password, so `/Applications/TCFSProvider.app` remained absent.
- Ambient `tcfs` resolves to `0.12.12`, but ambient `tcfsd` still resolves to
  `0.12.2` in the older May 16 inventory packets; package or workspace smoke
  must keep passing explicit binary paths so evidence identifies the actual
  binaries under test.

So a local neo Finder smoke is now production-adjacent but still red. The next
acceptance step is no longer basic package install or strict preflight; it is
root-causing the FileProvider read timeout and archiving exact-content
hydration through the installed `/Applications/TCFSProvider.app` path.
(Updated 2026-05-18: the read-timeout root cause is closed on the hosted
self-hosted runner — run `26061402177` proves exact-content hydration through
the installed Dev ID app. A local neo replay is still a useful workstation
proof but no longer the gating blocker. See the milestone section at the top
of this doc. The remaining Dev ID acceptance step is the layered
evict/rehydrate, mutation, and conflict-status proofs that the PZM
testing-mode lane already carries.)

After the signing/profile gate passes, production Finder evidence must also
prove the runtime config source. The post-install smoke can make this fatal by
requiring the FileProvider extension log line that says config loaded from the
shared Keychain; it fails if the extension reports build-time embedded config
or emits no config-source evidence.

For a source-tree proof, use the workspace variant. It builds `tcfs` and `tcfsd`,
defaults `EXPECTED_VERSION` from the workspace `Cargo.toml`, and points the
preflight at `target/debug/tcfs` and `target/debug/tcfsd`:

```bash
task lazy:macos-finder-preflight-workspace
```

## Highest-Value Smoke Lane

This is the current best acceptance path for the macOS desktop surface.

### Preconditions

- a macOS machine with the packaged app and binaries installed
- a valid tcfs daemon config
- a valid FileProvider config at
  `~/.config/tcfs/fileprovider/config.json`
- a runnable `tcfsd`

### Named Harness

The repo now carries a named operator-facing harness for this lane:

```bash
bash scripts/macos-postinstall-smoke.sh \
  --expected-version "${VERSION}" \
  --config "$HOME/.config/tcfs/config.toml" \
  --expected-file "path/to/known/remote-backed-file" \
  --expected-content-file /tmp/tcfs-expected-content.txt
```

For a fresh diagnostic fixture, use `--seed-expected-file` instead of selecting
an existing path. The harness creates a timestamped
`finder-smoke-<UTC>/fixture.txt` path unless `--expected-file` is also supplied,
pushes it with `tcfs push`, archives the push log, then requires
`tcfs index inspect` to report `visible` before the FileProvider download/read
phase.

The same Finder/FileProvider lane is exposed through the task surface. The
wrapper requires either `EXPECTED_FILE` or `SEED_EXPECTED_FILE=1`, so it cannot
pass as package-only artifact smoke:

```bash
EXPECTED_VERSION="${VERSION}" \
EXPECTED_FILE="path/to/known/remote-backed-file" \
EXPECTED_CONTENT_FILE=/tmp/tcfs-expected-content.txt \
TCFS_REQUIRE_KEYCHAIN_CONFIG=1 \
task lazy:macos-finder-smoke
```

The task wrapper also accepts `SEED_EXPECTED_FILE=1` for the fresh-fixture path
and `REBUILD_DOMAIN=1` for the direct-host diagnostic domain remove/add path.
Domain rebuild is intentionally opt-in evidence collection for stale-domain
investigation, not the default operator flow.

For release evidence, prefer the strict wrapper so signing/profile checks and
shared-Keychain config-source checks run in one lane:

```bash
EXPECTED_VERSION="${VERSION}" \
EXPECTED_FILE="path/to/known/remote-backed-file" \
EXPECTED_CONTENT_FILE=/tmp/tcfs-expected-content.txt \
task lazy:macos-finder-release-smoke
```

For a source-tree smoke, use the workspace variant so the harness builds and
uses `target/debug/tcfs` plus `target/debug/tcfsd` instead of ambient installed
binaries:

```bash
EXPECTED_FILE="path/to/known/remote-backed-file" \
EXPECTED_CONTENT_FILE=/tmp/tcfs-expected-content.txt \
task lazy:macos-finder-smoke-workspace
```

Notes:

- `--expected-file` should point at a known remote-backed fixture relative to
  the `~/Library/CloudStorage/TCFS*` root for the current domain
- `--expected-content-file` upgrades the smoke from "readable placeholder" to
  exact-content hydration proof and should be used for release evidence
- `--seed-expected-file` creates and pushes a fresh fixture, then requires
  `tcfs index inspect` to prove the expected path is remotely visible before
  FileProvider hydration starts
- `TCFS_REQUIRE_KEYCHAIN_CONFIG=1` upgrades the smoke from diagnostic hydration
  proof to production config-source proof; it requires extension logs showing
  `loadConfig: loaded from shared Keychain` and rejects build-time embedded
  config
- `TCFS_FILEPROVIDER_DIRECT_HOST_LAUNCH=1` or `--direct-host-launch` runs the
  host app executable directly so domain-add and requestDownload logs are
  deterministic. Use this for local proof when LaunchServices/unified-log
  polling is too weak, while still treating the resulting read as the real
  FileProvider acceptance gate.
- `TCFS_FILEPROVIDER_REBUILD_DOMAIN=1` or `--rebuild-domain` launches the host
  executable directly and asks it to remove and re-add the TCFS FileProvider
  domain before the smoke. Use this only for stale-domain diagnostics and
  archive the packet; it is not the default clean-host acceptance behavior.
- the harness fails if `pluginkit` reports multiple registrations for
  `io.tinyland.tcfs.fileprovider`; remove stale app/extension copies before
  claiming clean-host acceptance, or pass
  `--allow-multiple-plugin-registrations` only for diagnostic runs; verbose
  `pluginkit` output includes the app/extension paths that need cleanup
- on `neo`, run `task lazy:macos-fileprovider-neo-cleanup-packet` before any
  cleanup. It archives binary versions, PATH resolution, app locations,
  PlugInKit records, signing/profile state, CloudStorage roots, configs,
  sockets, launchd labels, and bounded `~/tcfs` inventory. Use the published
  `.pkg` or the archived notarized candidate package as the install source, and
  use `INSTALL_MODE=sudo` from an authenticated terminal or
  `INSTALL_MODE=osascript` from the logged-in desktop when non-interactive
  `sudo -n` is not available. Keep `QUARANTINE_STALE=0` for the first
  canonical install attempt; clean stale user/build registrations only after
  the install/inventory packet exists. Require
  `TCFS_REQUIRE_PRODUCTION_SIGNING=1 task lazy:macos-finder-preflight` before
  describing any local Finder smoke as production-adjacent.
- the helper assumes `tcfsd` is already runnable with a real config; it does
  not fabricate temp-home state or start a fake backend
- `#309` still tracks where this harness runs from a known-clean host per tag

### GitHub-Hosted Notarization Proof

The repo also carries a manual source-package proof workflow:

- [`.github/workflows/macos-fileprovider-pkg-notarization-proof.yml`](../../.github/workflows/macos-fileprovider-pkg-notarization-proof.yml)

This lane is intentionally narrower than Finder acceptance. It builds the
current source package on a GitHub-hosted arm64 macOS runner, imports the
Developer ID Application and Installer identities plus FileProvider profiles,
submits the signed `.pkg` to Apple notarization, staples the accepted ticket,
runs Gatekeeper install assessment, and requires strict package smoke with
signature, Gatekeeper, and stapled-ticket checks. It uploads the notarized
package as a workflow artifact and does not publish a GitHub Release, install
into `/Applications`, mutate PlugInKit, or run Finder lifecycle smoke.

### GitHub-Hosted Approximation

The repo now also carries a manual GitHub Actions executor for this lane:

- [`.github/workflows/macos-postinstall-smoke.yml`](../../.github/workflows/macos-postinstall-smoke.yml)

This is a `workflow_dispatch` lane on GitHub's `macos-15` arm64 runner that:

- uses the workflow ref's current acceptance harness while downloading the
  requested release tag's published `.pkg`
- runs `scripts/macos-pkg-structure-smoke.sh --require-signature` before
  installing the package. Current-postinstall equality is opt-in through
  `require_current_postinstall`; older already-published tags can continue
  through install/Finder proof while still checking payload shape and signature.
- runs `scripts/install-smoke.sh`
- writes a real tcfs config from the `tcfs-macos-smoke` GitHub
  environment secrets, including a run-only E2EE master key
- seeds an E2EE remote-backed fixture with `tcfs push`
- proves the fixture cannot be pulled without the E2EE master key, then pulls
  it with the key and verifies exact content
- starts `tcfsd` with both primary and FileProvider sockets
- runs `scripts/macos-postinstall-smoke.sh` with exact-content hydration and
  shared-Keychain config-source proof
- explicitly nudges hosted-runner enumeration by opening the CloudStorage root,
  using supported `fileproviderctl` probes (`materialize`, `evaluate`, or
  `check -a`, depending on the macOS image), and saving `ls` probe logs before
  the hard enumeration wait. This covers headless macOS sessions where the
  domain root appears but the FileProvider extension is not launched by a plain
  filesystem walk.
- after the expected placeholder appears, launches the installed host app
  binary with `TCFS_FILEPROVIDER_REQUEST_DOWNLOAD_IDENTIFIER` so the containing
  app asks FileProvider to download the file before the harness performs its
  coordinated content read. The harness uses direct process environment for
  this step because `open` plus `launchctl` environment propagation is not
  reliable in headless lab sessions.
- by default, sets the current user's PlugInKit election to `use` for
  `io.tinyland.tcfs.fileprovider` before launching the host app. This is a
  hosted-runner approximation of the user enabling the File Provider in System
  Settings, not package postinstall behavior.

Required `tcfs-macos-smoke` environment secrets:

- `TCFS_SMOKE_S3_ENDPOINT`
- `TCFS_SMOKE_S3_BUCKET`
- `TCFS_SMOKE_S3_ACCESS_KEY_ID`
- `TCFS_SMOKE_S3_SECRET_ACCESS_KEY`
- `TCFS_SMOKE_MASTER_KEY_B64`

Create or update the environment secrets with:

```bash
gh api --method PUT repos/Jesssullivan/tummycrypt/environments/tcfs-macos-smoke

gh secret set --env tcfs-macos-smoke TCFS_SMOKE_S3_ENDPOINT
gh secret set --env tcfs-macos-smoke TCFS_SMOKE_S3_BUCKET
gh secret set --env tcfs-macos-smoke TCFS_SMOKE_S3_ACCESS_KEY_ID
gh secret set --env tcfs-macos-smoke TCFS_SMOKE_S3_SECRET_ACCESS_KEY

openssl rand 32 | base64 | tr -d '\n' \
  | gh secret set --env tcfs-macos-smoke TCFS_SMOKE_MASTER_KEY_B64
```

Notes:

- As of 2026-04-30, the `tcfs-macos-smoke` GitHub environment exists and has
  been populated from the `honey` cluster's `tcfs/seaweedfs-admin` secret plus
  a fresh per-environment E2EE smoke key. The temporary public endpoint is a
  Cloudflare quick tunnel to the existing `tcfs` namespace SeaweedFS S3
  service; see
  [`macos-hosted-smoke-backend-bootstrap-2026-04-30.md`](../release/macos-hosted-smoke-backend-bootstrap-2026-04-30.md).
- this workflow is intentionally **storage-driven**, not fleet-sync-driven; it
  does not require NATS because the post-install enumerate + hydrate lane only
  needs S3-backed manifest and chunk access
- `TCFS_SMOKE_S3_ENDPOINT` must be an HTTPS URL; plaintext HTTP is rejected
  because the hosted lane carries live storage credentials
- the remaining unknown is backend reachability from GitHub-hosted macOS
  runners; Tailscale-only, RFC1918, localhost, and other clearly non-public
  endpoints are not sufficient for this executor, and the workflow now rejects
  those classes during preflight. It also resolves and opens TCP to the
  configured HTTPS endpoint during preflight so expired public tunnel hostnames
  fail before package download/install.
- the lazy traversal demo defaults to disposable, run-scoped S3-compatible
  prefixes; the on-prem TCFS authority is not a prerequisite for this proof
  unless an operator intentionally selects it and records the private-runner
  reachability assumption with the evidence
- the E2EE assertion currently covers the single seeded fixture file. Do not
  generalize that to whole-tree CLI push until directory push wires the same
  encryption context as single-file push, daemon push, VFS writes, and
  FileProvider hydration.
- a hosted pass still does not prove that the macOS app-group entitlement and
  provisioning story are correct on every clean machine; treat keychain/app
  group failures as a distinct class from storage reachability failures
- treat this as a clean-host approximation, not as already-proven release
  truth, until at least one tagged run has passed and produced usable logs on
  GitHub

May 9, 2026 hosted production-artifact retry narrowed the current hosted
executor blocker again:

- Run `25613963424` targeted GitHub-hosted `macos-15` with the published
  `v0.12.12` Developer ID `.pkg`.
- The run passed checkout, release input and secret validation, E2EE key
  material install, package download, package structure verification, package
  install, installed FileProvider signing verification, installed-binary smoke,
  live config write, and FileProvider config provisioning.
- It failed at `Seed remote fixture`, before daemon startup and before the
  FileProvider harness. The uploaded `tcfs-push.log` shows the configured
  Cloudflare quick-tunnel S3 endpoint failed DNS resolution from the hosted
  runner and the push timed out after retries.
- This is not a production Finder pass and not a new FileProvider failure. It
  is a public storage endpoint freshness/reachability blocker for the
  GitHub-hosted executor. Refresh `TCFS_SMOKE_S3_ENDPOINT` to a currently
  reachable public endpoint, or move the production `.pkg` smoke to a
  private/self-hosted Mac that can reach the backend.

May 1, 2026 hosted evidence narrowed the blocker at that time:

- `v0.12.6` built, notarized, and published automatically from release run
  `25197243787`.
- The published `.pkg` installs cleanly on the GitHub `macos-15` runner, passes
  production signing checks, provisions shared-Keychain config, starts `tcfsd`,
  reaches the public S3 backend, and proves the seeded E2EE fixture via CLI.
- The package postinstall LaunchServices registration fix works: hosted smoke
  run `25197861348` shows exactly one parented PlugInKit record for
  `io.tinyland.tcfs.fileprovider` under
  `/Applications/TCFSProvider.app`.
- FileProvider enumeration still fails before TCFS extension logs appear because
  macOS reports `NSFileProviderErrorDomainDisabled` (`-2011`): Finder and
  `fileproviderd` log `Sync is not enabled for "TCFSProvider"`.

The later `v0.12.12` hosted production attempt is the current package-lane
truth for this sprint: it passed install/signing/installed-CLI/config gates,
then failed earlier at storage fixture seeding because the public tunnel
hostname no longer resolved from GitHub-hosted macOS.
- The classification retry at `25198428805` now fails with an explicit
  `NSFileProviderErrorDomain -2011` diagnosis and captures the supporting Apple
  FileProvider logs in the workflow artifact.
- The explicit user-election retry at `25198592232` ran
  `pluginkit -e use -i io.tinyland.tcfs.fileprovider`; `pluginkit.txt` shows a
  `+` election for the extension, but FileProvider still reports
  `state:disabled` and `FP -2011`.
- `v0.12.7` shipped the FileProvider-side fixes proven locally: working-set
  import, directory identifier normalization, host-app
  `requestDownloadForItem`, and a coordinated placeholder read in the harness.
  Release run `25223938357` built the FileProvider app, binary matrix, `.pkg`,
  GitHub Release, and Homebrew formula successfully.
- A local real FileProvider smoke on a user-enabled Mac passed with a fresh
  remote fixture: root enumeration, host-app download request, `fetchContents`,
  hydration, and content match all completed. On the same machine,
  `fileproviderctl evaluate ~/Library/CloudStorage/TCFSProvider-TCFS` reports a
  non-empty root, and FPCK passes over the TCFS root.
- Hosted smoke run `25224523480` against the published `v0.12.7` production
  `.pkg` passed install, production signing, storage connectivity, daemon
  startup, and E2EE fixture proof. It still failed at the FileProvider gate with
  `NSFileProviderErrorDomain -2011`; diagnostics show PlugInKit registration
  and host domain add succeeded, while `fileproviderd` kept the provider
  `state:disabled` and logged `Sync is not enabled for "TCFSProvider"`.

That is a user-enable/consent boundary on the hosted runner, not another
package assembly, signing, storage, or duplicate PlugInKit registration failure.
`pluginkit -e use` is not enough to model FileProvider sync enablement on the
GitHub-hosted `macos-15` executor. Apple exposes
`NSFileProviderDomainTestingModeAlwaysEnabled` for test environments, but the
SDK requires the `com.apple.developer.fileprovider.testing-mode` entitlement to
set it. After `v0.12.7`, do not keep cutting production release tags solely to
retry this hosted lane; the remaining useful paths are a clean lab Mac where the
File Provider can be user-enabled, or an allowed testing-mode build that carries
Apple's FileProvider testing-mode entitlement.

Testing-mode support is intentionally opt-in:

- production entitlements do not include
  `com.apple.developer.fileprovider.testing-mode`
- `swift/fileprovider/build.sh` only injects that entitlement when
  `TCFS_FILEPROVIDER_TESTING_MODE_ENTITLEMENT=1`
- the host app only requests `NSFileProviderDomainTestingModeAlwaysEnabled` when
  launched with `TCFS_FILEPROVIDER_TESTING_MODE_ALWAYS_ENABLED=1`
- the post-install harness option `--fileprovider-testing-mode` verifies the
  installed host app carries the testing-mode entitlement before setting the
  launch environment and launching the app
- `.github/workflows/macos-fileprovider-testing-mode-pkg.yml` now builds a
  non-release testing-mode `.pkg` artifact named `dist-testing-mode-pkg` on the
  registered `petting-zoo-mini` runner; it reuses the release CLI tarball, but
  signs the FileProvider app with local Apple Development identity/profiles
- `.github/workflows/macos-postinstall-smoke.yml` can install that package via
  `package_artifact_run_id` plus `fileprovider_testing_mode=true`, so this proof
  does not require publishing a testing-mode package as a GitHub Release; the
  workflow rejects `fileprovider_testing_mode=true` unless a testing package is
  supplied through `package_artifact_run_id` or `package_url`, and unless the
  run targets a non-hosted runner label

Use that path only with an Apple provisioning profile that grants the
testing-mode entitlement. A normal production `v0.12.7` package is expected to
fail that preflight.

May 6, 2026 testing-mode evidence updated the current blocker:

- ASC provisioning on `petting-zoo-mini` produced a fresh lab-owned Apple
  Development certificate, p12, and matching Mac App Development host/extension
  profiles.
- The host development profile grants
  `com.apple.developer.fileprovider.testing-mode`; the extension profile is the
  matching `io.tinyland.tcfs.fileprovider` development profile.
- Testing-mode package run `25445945705` built and uploaded
  `dist-testing-mode-pkg` from `v0.12.11`.
- The first PZM smoke attempts reached FileProvider enumeration and showed the
  expected remote item, but the harness stalled because the second host-app
  launch used LaunchServices and the host process never received
  `TCFS_FILEPROVIDER_REQUEST_DOWNLOAD_IDENTIFIER`.
- Commit `b52ebd7` changed the harness to launch the installed host app binary
  directly for the download-request step, passing
  `TCFS_FILEPROVIDER_REQUEST_DOWNLOAD_IDENTIFIER` in the process environment.
- PZM smoke run `25446601375` then passed end to end: package install,
  signing/profile checks, installed-binary smoke, live S3/E2EE fixture proof,
  `tcfsd` startup, FileProvider registration, CloudStorage enumeration,
  host-app `requestDownload`, 55-byte hydration, exact-content match, and
  shared-Keychain config proof.
- PZM testing-mode package run `25453041957` rebuilt the current `v0.12.12`
  package from `a201c1e`.
- PZM smoke run `25453088909` passed install/signing/profile/E2EE/daemon gates,
  then failed the FileProvider lifecycle harness because `spctl` rejected both
  the Mac Development-signed host app and extension, `syspolicy_check` reported
  the installed app lacks a notarization ticket and has a fatal Gatekeeper
  rejection, and AppleSystemPolicy denied both `TCFSProvider` and
  `TCFSFileProvider`.
- PZM testing-mode package run `25456290021` rebuilt from `5ba8851` and added
  early build-output policy-probe markers. That artifact still shows `spctl`
  rejection, but the host app prints `policyProbe: main entered`,
  `policyProbe: domain created`, `testingMode: requested alwaysEnabled for
  FileProvider domain`, and `policyProbe: OK`, then exits 0. This proves the
  Swift host startup path itself is runnable in the runner context before
  install.
- PZM smoke run `25454681083` installed that package and passed the same
  install/signing/profile/E2EE/daemon gates, but the harness failed again.
  Diagnostics show an empty `harness/host-domain-launch.log`,
  AppleSystemPolicy denial for
  `/Applications/TCFSProvider.app/Contents/MacOS/TCFSProvider`, and
  AppleSystemPolicy denial for
  `/Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex/Contents/MacOS/TCFSFileProvider`.
- PZM smoke run `25456341985` installed that fresh package. The installed-host
  policy probe wrote `exit=124` and `timed out after 15s`, with no Swift stderr;
  its `sample` report shows the live process still at `_dyld_start`. The full
  harness then failed in the same place, with an empty
  `harness/host-domain-launch.log` and AppleSystemPolicy denial for both the
  installed host and extension.
- PZM smoke run `25458526158` proved the old `spctl --add --label` workaround
  is not available on macOS 15: `spctl` exits 4 with "This operation is no
  longer supported" and the man page routes this class of rule to
  configuration profiles.
- PZM smoke run `25562087555` installed the same `v0.12.12` testing-mode
  package, verified the computer-level `TCFS FileProvider Lab Gatekeeper Rules`
  `SystemPolicyRule` profile, passed the installed host policy probe
  (`policyProbe: OK`), loaded extension config from shared Keychain, registered
  the FileProvider domain, enumerated CloudStorage, requested download,
  evicted, requested download again, and hydrated the exact 55-byte fixture.
  `fileproviderctl check` reconciled 35 files for both the root and expected
  parent.

So the testing-mode read/hydrate plus evict/rehydrate lane is proven on the
non-production PZM Mac Development lab. The remaining macOS product work is
production Developer ID clean-host enablement plus production mutation,
conflict/status visibility, reliable badges/progress assertions, and recovery
UX. (Updated 2026-05-18: production Developer ID clean-host enablement +
enumerate + exact-content hydrate is now proven by run `26061402177`. The
production Dev ID lane still owes the layered evict/rehydrate, mutation, and
conflict/status proofs that the PZM testing-mode lane already carries.)

May 8, 2026 update: mutation harness support is now green in the PZM lab. The
first attempt, package run `25564780049`, failed before app build because the
runner-local p12 path was not passed; the local shell had expanded `~` on the
operator workstation. The corrected package run `25565895586` used absolute
runner-local paths under `/Users/jsullivan2/.tcfs-fileprovider-lab` and passed
p12 import, FileProvider app build/signing, testing-mode entitlement
verification, policy probe, `.pkg` assembly, and package upload. Smoke run
`25565943781` then passed install/signing/profile/E2EE/daemon gates plus the
extended FileProvider harness: enumerate, requestDownload, evict, rehydrate,
CloudStorage mutation write, exact 68-byte remote pull, and post-mutation
`tcfs status` with storage `[ok]`. Package run `25569345240` and smoke run
`25569596910` extended the same testing-mode lane with
`exercise_conflict_status=true`: CLI status reported `sync state: conflict`,
FileProvider readback preserved exact content, and the run captured that the
enumerator did not emit a conflict hydration-state log, so badge/progress
assertions remain observational.

May 1, 2026 Apple Developer follow-up changed the shape of this lane:

- enabling FileProvider Testing Mode on the host App ID invalidated and required
  regenerating the production host profile
- the regenerated production Developer ID host profile is valid for
  `io.tinyland.tcfs`, App Groups, and Keychain access, and is now the correct
  production host profile input
- a separate `tcfs-host-testing-mode-developer-id` Developer ID profile was also
  generated as a probe, but the decoded profile still did not include
  `com.apple.developer.fileprovider.testing-mode`
- Apple documentation allows managed capabilities to be limited to a subset of
  distribution options, and the observed TCFS profile behavior shows the
  testing-mode entitlement is available to Mac App Development profiles but not
  Developer ID profiles

So the remaining testing-mode path is no longer a Developer ID hosted package.
It needs a registered Mac plus Mac App Development host/extension profiles that
actually carry the entitlement. The detailed plan is
[macOS FileProvider Testing-Mode Strategy](macos-fileprovider-testing-mode-strategy.md).

Once a Mac App Development host profile exists and terminal decoding proves it
carries the entitlement, install it on `petting-zoo-mini` for the GitHub runner
user. The current lab lane deliberately resolves local profiles from the runner
machine instead of storing development signing material as GitHub repository
secrets:

```bash
mkdir -p "$HOME/Library/MobileDevice/Provisioning Profiles"
cp path/to/tcfs-host-development-testing-mode.provisionprofile \
  "$HOME/Library/MobileDevice/Provisioning Profiles/"
cp path/to/tcfs-fileprovider-development.provisionprofile \
  "$HOME/Library/MobileDevice/Provisioning Profiles/"
```

The helper now targets the `petting-zoo-mini` registered lab Mac by default.
Use the generated PZM p12/profiles when dispatching the current lane:

```bash
scripts/macos-fileprovider-testing-mode-dispatch.sh \
  --tag v0.12.12 \
  --runner-label petting-zoo-mini \
  --signing-p12-path /Users/jsullivan2/.tcfs-fileprovider-lab/tcfs-fileprovider-lab-E9B03E55.p12 \
  --signing-p12-password-file /Users/jsullivan2/.tcfs-fileprovider-lab/p12-password.txt \
  --profiles-dir /Users/jsullivan2/.tcfs-fileprovider-lab \
  --lab-gatekeeper-override
```

It dispatches the non-release testing package workflow, waits for it by default,
then dispatches the post-install smoke with the package artifact run id and
`fileprovider_testing_mode=true`. To inspect the GitHub Actions calls without
dispatching anything, use `--dry-run`.

For the current installed-app policy blocker, reuse the green package artifact
and add the guarded lab trust experiment:

```bash
scripts/macos-fileprovider-testing-mode-dispatch.sh \
  --tag v0.12.12 \
  --runner-label petting-zoo-mini \
  --package-run-id 25456290021 \
  --lab-gatekeeper-override
```

Before dispatching, the helper checks GitHub's self-hosted runner API for an
online macOS runner carrying the requested label. If
`repos/Jesssullivan/tummycrypt/actions/runners` returns no matching
`petting-zoo-mini` runner, enroll the repository-scoped macOS runner first or
rerun with `--skip-runner-check` only when you intentionally want the job to
queue.

Operational note from the May 2, 2026 enrollment: starting the service through
the runner's stock `svc.sh` from SSH can load it into the SSH session's
`Background` launchd manager, after which GitHub may show the runner offline
when that session lifecycle ends. For petting-zoo-mini, bootstrap the generated
LaunchAgent into `gui/$(id -u)` for the dedicated runner user and verify
`launchctl print gui/$(id -u)/actions.runner.Jesssullivan-tummycrypt.petting-zoo-mini-tcfs`
shows `state = running`.

The equivalent manual form is:

```bash
gh workflow run macos-fileprovider-testing-mode-pkg.yml \
  -f tag=v0.12.12 \
  -f runner_label=petting-zoo-mini \
  -f signing_p12_path=/Users/jsullivan2/.tcfs-fileprovider-lab/tcfs-fileprovider-lab-E9B03E55.p12 \
  -f signing_p12_password_file=/Users/jsullivan2/.tcfs-fileprovider-lab/p12-password.txt \
  -f profiles_dir=/Users/jsullivan2/.tcfs-fileprovider-lab

TESTING_PKG_RUN_ID="$(gh run list \
  --workflow macos-fileprovider-testing-mode-pkg.yml \
  --event workflow_dispatch \
  --limit 1 \
  --json databaseId \
  --jq '.[0].databaseId')"

gh run watch "$TESTING_PKG_RUN_ID" --exit-status
```

If that run uploads `dist-testing-mode-pkg`, the old hosted shape feeds that
run id into the hosted post-install smoke:

```bash
gh workflow run macos-postinstall-smoke.yml \
  -f tag=v0.12.12 \
  -f package_artifact_run_id="$TESTING_PKG_RUN_ID" \
  -f package_artifact_name=dist-testing-mode-pkg \
  -f fileprovider_testing_mode=true \
  -f runner_label=petting-zoo-mini

SMOKE_RUN_ID="$(gh run list \
  --workflow macos-postinstall-smoke.yml \
  --event workflow_dispatch \
  --limit 1 \
  --json databaseId \
  --jq '.[0].databaseId')"

gh run watch "$SMOKE_RUN_ID" --exit-status
```

Do not point `fileprovider_testing_mode=true` at the default release package.
That is intentionally a production artifact and should not carry Apple's
testing-mode entitlement.

### Manual Procedure

The script above codifies the manual steps below. Keep them here as the
operator-readable fallback and review path.

1. Verify the expected artifacts exist:

```bash
test -x /usr/local/bin/tcfsd || test -x "$HOME/usr/local/bin/tcfsd"
test -d /Applications/TCFSProvider.app || test -d "$HOME/Applications/TCFSProvider.app"
```

2. Verify the extension is registered with `pluginkit`:

```bash
pluginkit -m -A -D -vvv -i io.tinyland.tcfs.fileprovider
```

Clean acceptance should show exactly one registration for that bundle id.

3. Launch the host app from the installed location:

```bash
open -a TCFSProvider
```

4. Verify the CloudStorage root appears:

```bash
ls "$HOME/Library/CloudStorage" | rg '^TCFS'
```

5. Verify enumeration by listing the mounted root:

```bash
find "$HOME/Library/CloudStorage" -maxdepth 2 -type f | head
```

6. Open or read a known remote-backed file and confirm that content hydration
   succeeds. This is the `--expected-file` target in the named harness.

7. Record whether badges or equivalent Finder state are visible, but treat that
   as observational evidence rather than a hard release gate.

### Pass Bar

Treat the current macOS desktop lane as materially proven only when all of the
following succeed on the same machine:

- extension registration is visible
- host app launch successfully adds/updates the FileProvider domain
- a CloudStorage root appears
- enumeration works
- the host app can request download of the expected placeholder
- opening a placeholder-backed file hydrates content successfully
- extension logs prove runtime config loaded from the shared Keychain, not a
  build-time embedded diagnostic config
- `tcfsd` remains healthy throughout the path

## Relationship To Other Runbooks

- Use [Distribution Smoke Matrix](distribution-smoke-matrix.md) for packaged
  install proof.
- Use [Lazy Hydration Demo Acceptance](lazy-hydration-demo.md) for the shared
  terminal/Finder representation contract and demo target.
- Use [`scripts/macos-postinstall-smoke.sh`](../../scripts/macos-postinstall-smoke.sh)
  for the named post-install harness that exercises this lane.
- Use [Apple Surface Status](apple-surface-status.md) for the broader Apple
  posture.
- Use this document for the macOS Finder/FileProvider desktop acceptance path
  itself.
