# macOS FileProvider Testing-Mode Strategy

As of May 1, 2026, the hosted FileProvider proof is blocked by Apple's
profile-type boundary, not by TCFS package assembly, storage, signing profile
rotation, or a missing App ID checkbox.

The production lane and testing-mode lane must stay separate:

- production release packages use Developer ID Application / Developer ID
  Installer signing, production host and extension provisioning profiles, and
  notarization
- FileProvider testing-mode proof uses a registered Mac plus Mac App
  Development or Ad Hoc provisioning profiles that actually include
  `com.apple.developer.fileprovider.testing-mode`

Do not set `TCFS_HOST_TESTING_MODE_PROVISIONING_PROFILE_BASE64` to a Developer
ID Application profile. Apple can show FileProvider Testing Mode as an enabled
App ID capability while still omitting the entitlement from Developer ID
profiles. The workflow correctly rejects that profile shape.

## Apple Constraints

The relevant Apple behavior is:

- `com.apple.developer.fileprovider.testing-mode` is a testing/development
  entitlement. It is required before setting a non-empty
  `NSFileProviderDomain.testingModes` value.
- Apple managed capabilities can be limited to only some distribution options,
  such as development or ad hoc.
- Eligible provisioning profiles include enabled managed entitlements
  automatically, but ineligible profile types do not.
- Mac App Development profiles require registered devices.
- Notarization is for Developer ID distribution. Apple's notary requirements
  explicitly call for Developer ID signing, not Apple Development or ad hoc
  signing.

Primary Apple references:

- <https://developer.apple.com/documentation/BundleResources/Entitlements/com.apple.developer.fileprovider.testing-mode>
- <https://developer.apple.com/documentation/fileprovider/nsfileproviderdomain>
- <https://developer.apple.com/help/account/reference/provisioning-with-managed-capabilities>
- <https://developer.apple.com/help/account/provisioning-profiles/create-a-development-provisioning-profile/>
- <https://developer.apple.com/help/account/devices/devices-overview>
- <https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution>

## Observed TCFS Profile State

The fresh production host profile is valid for the production lane:

- profile: `tcfshostdeveloperid.provisionprofile`
- name: `tcfs-host-developer-id`
- application identifier: `QP994XQKNH.io.tinyland.tcfs`
- app group: `group.io.tinyland.tcfs`
- keychain group: `QP994XQKNH.*`
- FileProvider testing-mode entitlement: absent

The attempted Developer ID testing profile is useful evidence but not useful
input:

- profile: `tcfshosttestingmodedeveloperid.provisionprofile`
- name: `tcfs-host-testing-mode-developer-id`
- application identifier: `QP994XQKNH.io.tinyland.tcfs`
- FileProvider testing-mode entitlement: absent

That confirms the dead end: Developer ID profiles for this App ID do not carry
the testing-mode entitlement. Remove or archive the attempted testing-mode
Developer ID profile in Apple Developer to avoid future operator confusion.

## Recommended Lane

Use **Mac App Development** for FileProvider testing mode.

Why:

- it matches Apple's "testing and development" wording for the entitlement
- it is the common path for FileProvider domain testing on a developer or lab
  Mac
- it avoids pretending the artifact is a production distribution package
- it does not require notarization

Use Ad Hoc only if the goal becomes distributing the testing-mode artifact to a
small fixed set of registered Macs without requiring the local development
trust posture. Ad Hoc still requires registered devices and is not a substitute
for the public Developer ID release lane.

## Required Apple Assets

Register the Mac that will run the FileProvider smoke as a team Device. On that
Mac:

```bash
system_profiler SPHardwareDataType \
  | awk -F': ' '/Provisioning UDID/{print $2}'
```

Create or select an Apple Development certificate available to the build
machine. Then create two Mac App Development profiles for the registered Mac:

| Bundle | Profile type | Required profile contents |
| --- | --- | --- |
| `io.tinyland.tcfs` | Mac App Development | App Group, Keychain group, `com.apple.developer.fileprovider.testing-mode = true` |
| `io.tinyland.tcfs.fileprovider` | Mac App Development | App Sandbox, network client, App Group, Keychain group |

Verify the host profile before using it:

```bash
security cms -D -i path/to/tcfs-host-development-testing-mode.provisionprofile \
  > /tmp/tcfs-host-development-testing-mode.plist

/usr/libexec/PlistBuddy \
  -c 'Print :Entitlements:com.apple.developer.fileprovider.testing-mode' \
  /tmp/tcfs-host-development-testing-mode.plist
```

The expected output is:

```text
true
```

If that check does not print `true`, the profile is not valid for the
testing-mode lane regardless of its display name.

## CI Topology

Keep the existing production release workflow on Developer ID signing.

Add or refactor a separate testing-mode development workflow with these
properties:

1. Run the FileProvider smoke on a registered self-hosted/lab Mac, not on a
   GitHub-hosted macOS runner.
2. Build/sign `TCFSProvider.app` with an Apple Development identity and the two
   Mac App Development profiles.
3. Set `TCFS_FILEPROVIDER_TESTING_MODE_ENTITLEMENT=1` only for this workflow.
4. Skip Developer ID notarization for this lane.
5. Package signing should be optional. An unsigned local `.pkg` is acceptable
   for self-hosted lab proof when installed deliberately by the harness.
6. Run `scripts/macos-postinstall-smoke.sh --fileprovider-testing-mode` on the
   same registered Mac or on another registered Mac included in the profiles.

Preferred first implementation:

- a self-hosted runner with labels such as `self-hosted`, `macOS`, `ARM64`, and
  `tcfs-fileprovider-lab`
- Apple Development certificate and profiles installed in that runner's
  Keychain/profile directory, not exported to GitHub secrets
- repository secrets continue to hold storage/E2EE test credentials
- workflow checks local signing assets and fails with actionable profile
  metadata if the registered-Mac development profiles are missing

Possible later implementation:

- store Apple Development signing material as GitHub secrets
- build the development-signed package in GitHub Actions
- upload it as an artifact
- run installation and smoke only on a registered self-hosted Mac

Do not run a development/ad hoc testing-mode package on unregistered hosted
macOS. The profile device list is part of the trust model.

## Developer Loop

On the registered Mac, the local loop should be:

```bash
TCFS_HOST_PROVISIONING_PROFILE=/path/to/host-development-testing-mode.provisionprofile \
TCFS_EXTENSION_PROVISIONING_PROFILE=/path/to/extension-development.provisionprofile \
TCFS_FILEPROVIDER_TESTING_MODE_ENTITLEMENT=1 \
TCFS_REQUIRE_PRODUCTION_SIGNING=1 \
swift/fileprovider/build.sh target/release path/to/tcfs_file_provider.h build/fileprovider "Apple Development: ..."
```

Then verify:

```bash
codesign -d --entitlements :- build/fileprovider/TCFSProvider.app \
  | grep -F com.apple.developer.fileprovider.testing-mode

scripts/macos-fileprovider-preflight.sh \
  --signing-only \
  --require-production-signing \
  --app-path build/fileprovider/TCFSProvider.app
```

The existing `--require-production-signing` flag name is broader than its
current behavior: it checks that signing, entitlements, embedded profiles, and
profile certificate coverage are coherent. A future cleanup can rename or alias
this to a profile-signing gate for development lanes.

## Test Gates

Use these gates in order:

| Gate | Purpose | Pass condition |
| --- | --- | --- |
| Profile decode | Avoid spending runner time on unusable Apple assets | host development profile prints testing-mode entitlement as `true` |
| Build/sign | Prove Swift/Rust app can be signed for the registered Mac | app and extension codesign valid with embedded profiles |
| Entitlement split | Protect production builds | production app lacks testing-mode; development app includes it |
| Package structure | Verify installer payload shape | package contains CLI, daemon, app, appex, and repo postinstall |
| Install/start | Prove the package runs on the registered Mac | install succeeds, `tcfsd` starts, shared Keychain config is provisioned |
| FileProvider enablement | Prove the Apple boundary is crossed | harness sets testing mode and FileProvider does not fail with `FP -2011` |
| Read/hydrate | Prove product behavior | enumerate remote fixture and hydrate exact content |
| Parity follow-on | Move beyond read-only proof | unsync/evict+rehydrate, mutate/conflict, badges/progress/status evidence |

## Feature Goals

The testing-mode lane is not the end product. It is a system-test escape hatch
for an Apple consent boundary that GitHub-hosted macOS cannot cross.

Feature goals remain:

1. production `v0.12.x` packages stay Developer ID signed and notarized
2. user-enabled lab Mac proves production Finder behavior without testing mode
3. development testing-mode lane proves repeatable CI FileProvider behavior
4. Linux FUSE lane proves the scriptable reference behavior independently
5. parity evidence expands from read/hydrate into lifecycle, mutation,
   conflict, status, badges, and recovery

## Immediate Work Items

1. Register the selected lab Mac as an Apple Developer Device.
2. Create Mac App Development profiles for host and extension.
3. Verify the host profile contains
   `com.apple.developer.fileprovider.testing-mode = true`.
4. Add a development signing mode to
   `.github/workflows/macos-fileprovider-testing-mode-pkg.yml` or create a
   separate workflow with self-hosted runner labels.
5. Update `scripts/test-release-workflow-fileprovider.sh` so regression coverage
   reflects the development-signing testing-mode lane.
6. Update `scripts/macos-fileprovider-testing-mode-dispatch.sh` so it refuses
   GitHub-hosted execution unless a valid development/ad hoc package artifact is
   explicitly supplied for a registered Mac.
7. Run the self-hosted FileProvider testing-mode smoke against `v0.12.7`.

