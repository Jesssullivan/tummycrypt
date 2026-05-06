# macOS FileProvider Testing-Mode Strategy

As of May 6, 2026, the hosted production FileProvider proof is blocked by
Apple's profile/user-enable boundary. The registered-Mac testing-mode lane has
one green read/hydrate proof on `petting-zoo-mini`, but the current
evict/rehydrate lifecycle package is blocked by Gatekeeper/AppleSystemPolicy
rejecting the installed Mac Development-signed app and extension at launch.
The same package build output can reach Swift `main()` in policy-probe mode, so
the remaining gap is the lab install/provenance/service trust model.
Testing-mode proof is still not the same as production Developer ID Finder
lifecycle acceptance.

The production lane and testing-mode lane must stay separate:

- production release packages use Developer ID Application / Developer ID
  Installer signing, production host and extension provisioning profiles, and
  notarization
- FileProvider testing-mode proof uses a registered Mac plus Mac App
  Development provisioning profiles that actually include
  `com.apple.developer.fileprovider.testing-mode`

Do not set `TCFS_HOST_TESTING_MODE_PROVISIONING_PROFILE_BASE64` to a Developer
ID Application profile. Apple can show FileProvider Testing Mode as an enabled
App ID capability while still omitting the entitlement from Developer ID
profiles. The first lab implementation uses locally installed profiles on the
registered Mac instead of exporting the development profile through a GitHub
secret.

## Apple Constraints

The relevant Apple behavior is:

- `com.apple.developer.fileprovider.testing-mode` is a testing/development
  entitlement. It is required before setting a non-empty
  `NSFileProviderDomain.testingModes` value.
- Apple managed capabilities can be limited to only some distribution options,
  such as development.
- Eligible provisioning profiles include enabled managed entitlements
  automatically, but ineligible profile types do not.
- Mac App Development profiles require registered devices.
- Gatekeeper outside the Mac App Store is a Developer ID/notarization boundary.
  Apple's distribution guidance says Developer ID is the outside-Mac-App-Store
  identity Gatekeeper verifies, while Mac App Development profiles are for
  registered development computers.

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
- it can carry the testing-mode entitlement that Developer ID profiles omit

This does not make the packaged artifact a Gatekeeper-accepted distribution
artifact. Current PZM evidence shows a Mac Development-signed app installed
from the lab package is rejected by `spctl` and then killed by
AppleSystemPolicy, while the same build output reaches Swift `main()` in
policy-probe mode before install. Treat that as a lab trust-model problem, not
as a storage or FileProvider implementation failure.

The current controlled experiment for that trust-model problem is
`lab_gatekeeper_override=true` on `macos-postinstall-smoke.yml`. The input is
restricted to the `petting-zoo-mini` testing-mode lane. On macOS 15,
`spctl --add`/`--remove` rule mutation is no longer supported, so the helper
generates a `SystemPolicyRule` configuration profile from the installed host
and extension designated requirements, verifies that profile is installed, and
fails early with the generated `.mobileconfig` attached when it is missing.
Use it only to decide whether the non-production Mac Development lab can
continue into FileProvider lifecycle coverage; it is not evidence that the
production Developer ID package is accepted by Gatekeeper.

## Required Apple Assets

Register the Mac that will run the FileProvider smoke as a team Device. On that
Mac:

```bash
system_profiler SPHardwareDataType \
  | awk -F': ' '/Provisioning UDID/{print $2}'
```

Create or select an Apple Development certificate available to the build
machine. For ASC automation this is certificate type `DEVELOPMENT`, even though
the provisioning profiles themselves are `MAC_APP_DEVELOPMENT`. Then create two
Mac App Development profiles for the registered Mac.
Prefer the ASC-backed automation below over manual portal profile downloads:

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

## ASC Desired-State Provisioning

As of May 5, 2026, the lab lane has repo-owned ASC scaffolding instead of a
manual portal/download loop:

- desired state lives in `config/macos-fileprovider-lab.asc.json`
- `scripts/asc-fileprovider-lab-provision.py` plans or applies ASC
  certificate/profile state
- `scripts/macos-codesign-p12-probe.sh` imports a p12 into an isolated
  temporary keychain and proves `codesign` can use it before a GitHub Actions
  dispatch

The provisioner uses the existing ASC key defaults from the iOS pipeline:

- key id: `ZV65L9B864`
- issuer id: `d5db1c0a-0a82-4a50-9490-7d86be080506`
- private key path: `~/.private_keys/AuthKey_ZV65L9B864.p8`

These can be overridden with `ASC_KEY_ID`, `ASC_ISSUER_ID`, and either
`ASC_PRIVATE_KEY_PATH`, `ASC_PRIVATE_KEY_P8`, or `ASC_PRIVATE_KEY_BASE64`.
The same ASC key is also kept in encrypted form at
`credentials/app-store-connect.yaml`; decrypt it with SOPS only into a local
environment or temporary file. Do not commit a plaintext `.p8` key or generated
p12 files.

Plan against an existing Apple Development certificate fingerprint:

```bash
scripts/asc-fileprovider-lab-provision.py \
  --certificate-sha1 <apple-development-cert-sha1>
```

Apply with an existing certificate, download the profiles, and install them
under the current user's provisioning profile directory:

```bash
scripts/asc-fileprovider-lab-provision.py \
  --apply \
  --replace \
  --install \
  --certificate-sha1 <apple-development-cert-sha1>
```

The more repeatable path is to let PZM generate a fresh private key and CSR,
have ASC issue an Apple Development certificate, and bind the generated Mac App
Development profiles to that new certificate. This avoids reusing a damaged
login-keychain identity.

```bash
mkdir -p build/asc-fileprovider-lab
openssl rand -hex 16 > build/asc-fileprovider-lab/p12-password.txt
chmod 600 build/asc-fileprovider-lab/p12-password.txt
export TCFS_FILEPROVIDER_LAB_P12_PASSWORD="$(cat build/asc-fileprovider-lab/p12-password.txt)"

scripts/asc-fileprovider-lab-provision.py \
  --apply \
  --replace \
  --install \
  --create-certificate
```

If ASC refuses that with a current Development certificate conflict and the
current certificate is known-bad for noninteractive PZM signing, rotate it
explicitly by full SHA-1:

```bash
scripts/asc-fileprovider-lab-provision.py \
  --apply \
  --replace \
  --install \
  --create-certificate \
  --create-certificate-type DEVELOPMENT \
  --revoke-certificate-sha1 <current-apple-development-cert-sha1>
```

Certificate revocation is destructive Apple-side state. Use it only for the
lab-owned Apple Development certificate after confirming it is not serving
another active development lane.

That writes:

- a private key and CSR under `build/asc-fileprovider-lab/`
- a downloaded Apple Development certificate by default
- `tcfs-fileprovider-lab-<sha>.p12` (`security export` on macOS, with an
  OpenSSL fallback elsewhere)
- stable installed profile filenames:
  `tcfshostdevelopmenttestingmodepzm.provisionprofile` and
  `tcfsfileproviderdevelopmentpzm.provisionprofile`

That path can fail with ASC 409 if the team already has a current Development
certificate or pending request. Use the explicit revocation form above only
after confirming the current certificate is lab-owned and unusable.

Before dispatching CI, prove the p12 is usable by `codesign` in the same
security context that will run the build. SSH can list identities while still
failing private-key use, so a failing SSH probe is evidence about that session,
not necessarily about the GitHub runner LaunchAgent.

```bash
scripts/macos-codesign-p12-probe.sh \
  --p12 build/asc-fileprovider-lab/tcfs-fileprovider-lab-<sha>.p12 \
  --p12-password-file build/asc-fileprovider-lab/p12-password.txt
```

Then dispatch the lab package lane with the generated p12:

```bash
scripts/macos-fileprovider-testing-mode-dispatch.sh \
  --tag v0.12.11 \
  --runner-label petting-zoo-mini \
  --signing-p12-path ~/git/tummycrypt/build/asc-fileprovider-lab/tcfs-fileprovider-lab-<sha>.p12 \
  --signing-p12-password-file ~/git/tummycrypt/build/asc-fileprovider-lab/p12-password.txt
```

To run the explicit PZM trust experiment against an already-built testing-mode
package, reuse its package run id and ask the smoke workflow to require the
lab `SystemPolicyRule` profile:

```bash
scripts/macos-fileprovider-testing-mode-dispatch.sh \
  --tag v0.12.12 \
  --runner-label petting-zoo-mini \
  --package-run-id 25456290021 \
  --lab-gatekeeper-override
```

That flag passes `lab_gatekeeper_override=true` to the smoke workflow. The
workflow rejects it outside the PZM testing-mode lane, records the override
logs under `lab-gatekeeper-override/`, and uploads the generated
`tcfs-fileprovider-lab-system-policy.mobileconfig` when the profile is missing.
Install that profile through System Settings or MDM on PZM, then rerun the same
dispatch. The macOS `profiles` tool can list and remove configuration profiles,
but Apple no longer supports installing configuration profiles with it.

The default mode is non-mutating. `--apply` is required before the ASC script
creates certificates, creates profiles, deletes stale same-name profiles, writes
profile files, or installs profiles.

## CI Topology

Keep the existing production release workflow on Developer ID signing.

The separate testing-mode development workflow now has these properties:

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

- a self-hosted runner on `petting-zoo-mini`, using the custom runner label
  `petting-zoo-mini`
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

Do not run a development testing-mode package on unregistered hosted macOS. The
profile device list is part of the trust model.

## Repository Implementation

As of May 6, 2026, the repo has a working lab-lane implementation up to the
Apple runtime policy boundary:

- `.github/workflows/macos-fileprovider-testing-mode-pkg.yml` defaults to the
  `petting-zoo-mini` runner label, resolves an `Apple Development` signing
  identity from the local keychain, and selects local profiles with
  `scripts/macos-fileprovider-profile-inventory.sh --require-host-entitlement
  com.apple.developer.fileprovider.testing-mode`
- the testing-mode package workflow skips Developer ID certificate import,
  package signing, and notarization
- `.github/workflows/macos-postinstall-smoke.yml` still defaults to
  `macos-15` for production release proof, but accepts `runner_label`, requires
  a non-hosted runner when `fileprovider_testing_mode=true`, and exposes the
  PZM-only `lab_gatekeeper_override` trust experiment
- `scripts/macos-fileprovider-testing-mode-dispatch.sh` dispatches both the
  package build and smoke to `petting-zoo-mini` by default
- `config/macos-fileprovider-lab.asc.json`,
  `scripts/asc-fileprovider-lab-provision.py`, and
  `scripts/macos-codesign-p12-probe.sh` supersede ad hoc portal profile
  regeneration for the lab lane
- testing-mode package run `25445945705` built a `v0.12.11`
  `dist-testing-mode-pkg` on `petting-zoo-mini`
- post-install smoke run `25446601375` proved package install,
  signing/profile checks, shared-Keychain config, live S3/E2EE access,
  `tcfsd` startup, CloudStorage enumeration, host-app `requestDownload`,
  55-byte hydration, and exact-content match
- testing-mode package run `25453041957` rebuilt the current `v0.12.12`
  package from `a201c1e`
- post-install smoke run `25453088909` proved install/signing/profile/E2EE/
  daemon startup again, then failed the FileProvider lifecycle harness because
  `spctl` rejected both bundles and AppleSystemPolicy denied both
  `TCFSProvider` and `TCFSFileProvider`
- testing-mode package run `25456290021` added early build-output policy-probe
  markers: `spctl` still rejected the app, but `TCFSProvider` printed
  `policyProbe: main entered`, `policyProbe: domain created`, and
  `policyProbe: OK`, then exited 0 before install
- post-install smoke run `25456341985` installed that package and again passed
  install/signing/profile/E2EE/daemon startup. The installed-host policy probe
  timed out after 15s with no Swift stderr and sampled the live process at
  `_dyld_start`; the full harness again produced an empty
  `harness/host-domain-launch.log` plus AppleSystemPolicy denial for the
  installed host and extension.

## Lab Runner Enrollment

The `../blahaj` runner material is useful context, but it should not be the
control plane for this lane:

- Blahaj's current GitHub self-hosted runner path is the cluster/OpenTofu
  `tinyland-nix` lane, backed by ARC and `arc-systems`.
- Blahaj's petting-zoo-mini runner notes are GitLab/Colima-era diagnostics,
  including a historical containerd failure mode. They are not current
  GitHub Actions enrollment guidance.
- Blahaj explicitly routes petting-zoo-mini host/network authority to adjacent
  host and network repos. TCFS can consume the host as a lab runner, but should
  keep the FileProvider workflow contract in this repo.

Use a native repository-scoped GitHub Actions runner on petting-zoo-mini for
Finder/FileProvider proof. Do not use ARC for this job: ARC runs Kubernetes
runner pods, while this proof needs the actual macOS user session, Keychain,
provisioning profiles, `/Applications` install path, Finder/FileProvider
services, and local `launchd` runner service.

Enrollment shape:

1. In `Jesssullivan/tummycrypt`, open
   `Settings -> Actions -> Runners -> New self-hosted runner -> macOS`.
2. On petting-zoo-mini, install the runner under the dedicated runner user.
   Keep the GitHub registration token out of the repo and logs.
3. Configure it as a repository runner with default labels plus custom labels:
   `petting-zoo-mini,tcfs-fileprovider-lab`.
4. Install it as a macOS service with the runner's `svc.sh`, then verify the
   service is running in the runner user's GUI `launchd` domain, not only the
   SSH session's background domain.
5. Install the Apple Development certificate and Mac App Development
   provisioning profiles under the same runner user's Keychain and
   `~/Library/MobileDevice/Provisioning Profiles`.
6. Confirm GitHub sees the runner before dispatch:

   ```bash
   gh api repos/Jesssullivan/tummycrypt/actions/runners \
     --jq '.runners[]? | [.name, .os, .status, ([.labels[].name] | join(","))] | @tsv'
   ```

The dispatch helper now performs this runner-visibility check by default and
fails before dispatch if GitHub cannot see an online macOS runner with the
requested label. Use `--skip-runner-check` only when intentionally queueing a
job while the runner is being enrolled.

On petting-zoo-mini, the stock `./svc.sh start` path can report success from an
SSH session while loading the runner into the `Background` launchd manager. If
GitHub later shows the runner offline, bootstrap the generated LaunchAgent into
the logged-in user's GUI domain and verify the runner log reaches
`Listening for Jobs`:

```bash
label="actions.runner.Jesssullivan-tummycrypt.petting-zoo-mini-tcfs"
uid="$(id -u)"
plist="$HOME/Library/LaunchAgents/${label}.plist"

launchctl bootout "gui/${uid}" "$plist" >/dev/null 2>&1 || true
launchctl bootstrap "gui/${uid}" "$plist"
launchctl enable "gui/${uid}/${label}"
launchctl kickstart -k "gui/${uid}/${label}"
launchctl print "gui/${uid}/${label}" | sed -n '1,80p'
tail -n 20 "$HOME/github-actions-runners/tummycrypt-tcfs/_diag/Runner_*.log"
```

GitHub references for this runner model:

- <https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/use-in-a-workflow>
- <https://docs.github.com/en/actions/how-tos/write-workflows/choose-where-workflows-run/choose-the-runner-for-a-job>
- <https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/monitor-and-troubleshoot?platform=mac>
- <https://docs.github.com/en/actions/how-tos/manage-runners/use-actions-runner-controller/use-arc-in-a-workflow>

Once the Mac App Development profiles exist on `petting-zoo-mini`, run:

```bash
scripts/macos-fileprovider-testing-mode-dispatch.sh \
  --tag v0.12.12 \
  --runner-label petting-zoo-mini \
  --signing-p12-path ~/git/tummycrypt/build/asc-fileprovider-lab/tcfs-fileprovider-lab-4EC8EA7A.p12 \
  --profiles-dir ~/git/tummycrypt/build/asc-fileprovider-lab
```

Use `--dry-run` first if you want to inspect the exact `gh workflow run`
commands without dispatching.

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
| Runtime policy | Prove the lab trust model allows the Mac Development app to launch | `spctl`/`syspolicy_check` do not reject the installed host app before harness launch |
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
   only after its Mac Development trust model is explicit
4. Linux FUSE lane proves the scriptable reference behavior independently
5. parity evidence expands from read/hydrate into lifecycle, mutation,
   conflict, status, badges, and recovery

## Immediate Work Items

1. Install the generated PZM `SystemPolicyRule` profile from the
   `lab_gatekeeper_override` run artifact, then rerun the same smoke against
   `dist-testing-mode-pkg` from run `25456290021`. If it allows the installed
   host and extension to launch, continue lifecycle proof under that clearly
   marked non-production lane. If it does not, pivot to Xcode-style local
   development launch or an Apple-approved distribution shape that can still
   carry testing mode.
2. Keep `spctl`, `syspolicy_check`, xattr, codesign, embedded-profile,
   `taskgated-helper`, `amfid`, and AppleSystemPolicy diagnostics attached to
   every FileProvider lab failure.
3. Decide the next PZM trust experiment now that the installed host is proven
   blocked before Swift entry at `_dyld_start`: Xcode-style development launch,
   explicit lab Gatekeeper bypass, or an Apple-approved distribution shape that
   can still carry FileProvider testing mode.
4. Expand the successful read/hydrate proof into Linux/Finder parity follow-on
   gates.
