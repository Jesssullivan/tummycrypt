# macOS FileProvider Local Evidence - 2026-04-30

This records the April 30, 2026 local `neo` Finder/FileProvider proof. It is a
source-tree workstation smoke, not clean-host release acceptance.

## Scope

The run proves that the current direct FileProvider backend can:

1. register one installed FileProvider extension,
2. launch the host app and re-add the `io.tinyland.tcfs` domain,
3. pass strict Developer ID signing/profile preflight for both host app and
   FileProvider extension,
4. provision FileProvider runtime config through the shared Keychain,
5. enumerate a remote-backed CloudStorage tree,
6. hydrate a known file through the FileProvider path, and
7. verify exact hydrated content.

It does not prove a pristine `.pkg` install, notarization, Finder badges, write
flows, conflict UX, or a clean-host run.

## Fixture

- Host: `neo`
- App: `/Users/jess/Applications/TCFSProvider.app`
- CloudStorage root: `/Users/jess/Library/CloudStorage/TCFSProvider-TCFS`
- Fixture path: `finder-smoke-20260430T0305Z/finder-smoke.txt`
- Expected content source:
  `/Users/jess/tcfs/finder-smoke-20260430T0305Z/finder-smoke.txt`
- Expected content size: 120 bytes
- Smoke logs:
  `/tmp/tcfs-finder-smoke-20260430T1436Z/log-bounded-default-timeout`
- Host provisioning profile UUID:
  `8e93c5be-685f-4503-bf0a-d647a2062149`
- FileProvider provisioning profile UUID:
  `fa455f84-5e7d-4a14-9d4f-68a26c6a9939`
- Developer ID Application certificate SHA-1:
  `61A8E77C4F3D678921FF0A7DC9D7E317F7754F50`

## Command

```bash
LOG_DIR=/tmp/tcfs-finder-smoke-20260430T1436Z/log-bounded-default-timeout \
EXPECTED_FILE=finder-smoke-20260430T0305Z/finder-smoke.txt \
EXPECTED_CONTENT_FILE=/Users/jess/tcfs/finder-smoke-20260430T0305Z/finder-smoke.txt \
APP_PATH=/Users/jess/Applications/TCFSProvider.app \
CLOUD_ROOT=/Users/jess/Library/CloudStorage/TCFSProvider-TCFS \
TCFS_BIN=target/debug/tcfs \
TCFSD_BIN=target/debug/tcfsd \
task lazy:macos-finder-release-smoke
```

## Result

The smoke passed.

Important observed lines:

```text
tcfsd version: tcfsd 0.12.2
tcfs version: tcfs 0.12.2
pluginkit registration:
     io.tinyland.tcfs.fileprovider(0.2.0)
            Path = /Users/jess/Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex
host app keychain access group entitlement: QP994XQKNH.group.io.tinyland.tcfs
host app provisioning profile contains signing certificate: 61A8E77C4F3D678921FF0A7DC9D7E317F7754F50
FileProvider extension keychain access group entitlement: QP994XQKNH.group.io.tinyland.tcfs
FileProvider extension provisioning profile contains signing certificate: 61A8E77C4F3D678921FF0A7DC9D7E317F7754F50
host app log confirmed domain re-add
CloudStorage root: /Users/jess/Library/CloudStorage/TCFSProvider-TCFS
hydrated file: /Users/jess/Library/CloudStorage/TCFSProvider-TCFS/finder-smoke-20260430T0305Z/finder-smoke.txt
  size: 120 bytes
hydrated file content matched expected content file
FileProvider extension config source: shared Keychain
macOS post-install FileProvider smoke passed
```

The `tcfs status` calls talked to the already-running workstation daemon, which
reported `tcfsd v0.12.0` with storage `[ok]`. The smoke still verified that the
workspace binaries used by the harness were `0.12.2`. Treat this as local
workstation state, not release-package proof.

`fileproviderctl domain list` was not available in the expected form on this
host. The harness therefore relied on host-app logs plus the CloudStorage root,
which matched the documented fallback behavior.

## Debugging Notes

The first diagnostic run with the existing FileProvider config failed during
fetch with:

```text
key unwrapping failed: invalid master key or corrupted data
```

That exposed a real configuration mismatch: the FileProvider config's
passphrase-derived key did not match the master key used by the CLI/daemon.
The direct backend now supports explicit `master_key_base64`, `master_key_file`,
and mnemonic-derived recovery keys so this mismatch can be diagnosed and
configured intentionally.

An earlier diagnostic app embedded raw master-key material into the app bundle
to isolate backend behavior. That was useful for debugging but is no longer the
current proof. The active local app for this evidence was rebuilt with
`TCFS_REQUIRE_PRODUCTION_SIGNING=1`, which disables build-time embedded
FileProvider config by default. The strict smoke required extension logs proving
`loadConfig: loaded from shared Keychain`.

Follow-up no-embedded-config attempts produced sharper production truth:

- `swift/fileprovider/provision-config.sh` now emits `master_key_file` into the
  FileProvider JSON so the host app can derive the Keychain payload from the
  same master key used by the CLI/daemon.
- The host app now enriches the Keychain copy with `master_key_base64` when it
  can read a valid 32-byte `master_key_file`.
- Ad-hoc builds must not carry `keychain-access-groups`; macOS rejects them as
  restricted entitlements before launch.
- A Developer ID build without an associated provisioning profile also failed
  to launch the FileProvider extension: `amfid` reported "No matching profile
  found" for the app/extension entitlements.
- The App Group config-file fallback is not a viable substitute on this host:
  the extension resolves the App Group path but fileproviderd reports that it
  does not have permission to view `config.json`.
- The build script now supports
  `TCFS_HOST_PROVISIONING_PROFILE` and
  `TCFS_EXTENSION_PROVISIONING_PROFILE` so those profiles can be embedded
  before signing.
- The non-mutating preflight now has a strict production mode:
  `TCFS_REQUIRE_PRODUCTION_SIGNING=1 task lazy:macos-finder-preflight`.
  That mode fails unless both the host app and FileProvider extension codesign
  cleanly, include `keychain-access-groups`, and contain embedded provisioning
  profiles. It now decodes embedded profiles and verifies the App Group,
  concrete Keychain group, bundle identifier, and Apple team prefix. On the
  hygiene-reinstalled local ad-hoc app, strict preflight reports all four
  missing production-signing facts in one run: host-app Keychain entitlement,
  host-app profile, extension Keychain entitlement, and extension profile.
- `task lazy:macos-finder-profile-inventory` now finds the installed
  Developer ID profile pair under
  `~/Library/MobileDevice/Provisioning Profiles`.
- Apple's Developer ID profiles expose `keychain-access-groups` as
  `QP994XQKNH.*`; strict preflight treats that as covering the concrete signed
  entitlement `QP994XQKNH.group.io.tinyland.tcfs` only when the team prefix and
  bundle identifiers also match.
- Production-signed FileProvider builds now disable build-time embedded config
  by default when `TCFS_REQUIRE_PRODUCTION_SIGNING=1` is set. That keeps future
  release evidence on the intended host-app Keychain provisioning path unless a
  diagnostic override explicitly opts back into embedding.
- The post-install smoke can now require runtime config-source evidence with
  `TCFS_REQUIRE_KEYCHAIN_CONFIG=1` or `--require-keychain-config`. That mode
  requires extension logs proving `loadConfig: loaded from shared Keychain` and
  fails if the extension reports build-time embedded config.
- `swift/fileprovider/build.sh` now resolves the host Xcode SDK, `swiftc`, and
  `clang` through system `xcrun` by default. That avoids Nix dev-shell
  `DEVELOPER_DIR` / `SDKROOT` pollution, where a Nix macOS 14.4 SDK could be
  paired with Apple Swift 6.3.1 and fail before signing.
- The post-install smoke bounds `log show` calls so a stuck unified-log query
  cannot hang the release smoke indefinitely.

After those diagnostics, raw key material was removed from the temporary config
and App Group config. The local production-signed app/extension pair now passes
the strict signing/profile and shared-Keychain smoke on `neo`.

An earlier exact-content run also reached backend fetch successfully but hit a
FileProvider parent-propagation race on the immediate `cat`. The smoke harness
now retries the content read for the same timeout window and persists the final
stderr if hydration still fails. This keeps transient Finder/FileProvider
propagation from masking backend correctness while still requiring exact
content before the smoke can pass.

## Remaining Bar

Clean macOS desktop acceptance still requires:

1. a non-diagnostic `.pkg` install or app install on a known-clean macOS host,
2. exactly one extension registration,
3. shared-Keychain config load proof,
4. CloudStorage enumeration,
5. exact-content hydrate/open proof,
6. observable unsync/dehydrate behavior, and
7. a recorded clean-host run linked from GitHub issue `#309`.
