# Apple Surface Status

As of May 8, 2026, Apple support is a buildable and partially proven lane. The
macOS FileProvider testing-mode lab now has green enumerate, hydrate, evict,
rehydrate, mutation upload/readback, and deterministic conflict/status content
preservation proof on PZM, but Apple surfaces are still not a full
release-grade desktop or iOS product.

## macOS: Proven Today

- CI proves the Rust staticlib and Swift build surfaces needed for FileProvider
  packaging.
- Release automation can build Apple Silicon artifacts, package
  `TCFSProvider.app`, and publish `.pkg` plus tarball assets.
- The repo contains real macOS daemon, launchd, NFS loopback, and FileProvider
  code paths.
- The `petting-zoo-mini` lab lane can build a non-production testing-mode
  package with Mac App Development profiles and Apple's
  `com.apple.developer.fileprovider.testing-mode` entitlement.
- PZM smoke run `25446601375` on `v0.12.11` proved package install,
  signing/profile checks, shared-Keychain config, live S3/E2EE access, daemon
  startup, FileProvider registration, CloudStorage enumeration,
  `requestDownload`, and exact-content hydration.
- PZM smoke run `25562087555` on `v0.12.12` with the installed
  `TCFS FileProvider Lab Gatekeeper Rules` profile proved installed host
  policy launch, shared-Keychain config, live S3/E2EE access, daemon startup,
  FileProvider registration, CloudStorage enumeration, `requestDownload`,
  `evict`, re-`requestDownload`, and exact-content hydration.
- PZM package run `25565895586` and smoke run `25565943781` extended the same
  testing-mode lane into mutation proof: write through CloudStorage, exact
  remote pull of the 68-byte mutated file, and post-mutation `tcfs status`
  showing storage `[ok]`.
- PZM package run `25569345240` and smoke run `25569596910` extended the lane
  into deterministic conflict/status proof: CLI status reported
  `sync state: conflict` and FileProvider readback preserved exact content.
  Finder badges/progress remain observational.
- GitHub Actions links for the current PZM runs are indexed in
  [Release Evidence Index](../release/evidence/README.md).

## macOS: Not Yet Proven As A Release-Grade Desktop Surface

- There is no continuously exercised production Finder/FileProvider acceptance
  lane from Developer ID package install through user enablement, enumerate,
  hydrate, mutate, and conflict handling.
- Finder badges, progress UI, and notification behavior are not release gates.
- The green PZM lane is intentionally non-production testing-mode evidence; it
  does not mean arbitrary clean production Macs will auto-enable the provider.
- Packaged macOS artifacts still require explicit post-cut smoke even when CI
  and packaging are green.

## iOS: Current Posture

- The repo carries real iOS FileProvider and UniFFI code plus CI Swift
  type-check coverage.
- There is still no continuously exercised simulator or device acceptance lane.
- There is no repeatable TestFlight or App Store delivery path.
- Treat iOS as proof-of-concept and read-only in practice until stronger
  end-to-end evidence exists.

## Working Wording

Use:

- `macOS: CLI/daemon plus lab-proven experimental FileProvider lifecycle`
- `iOS: proof-of-concept FileProvider direction`

Avoid:

- `macOS: full` or `production-ready`
- `iOS: active release target`
- claims that production Finder badges, mutation, conflict UX, or arbitrary
  clean-host enablement are release-verified

## Validation Path

- Keep the Apple CI lanes green.
- Run post-release distribution smoke from
  [Distribution Smoke Matrix](distribution-smoke-matrix.md).
- Use [macOS Finder and FileProvider Reality](macos-fileprovider-reality.md) for
  the current desktop acceptance path and proof gaps.
- Keep extending the named macOS Finder/FileProvider smoke path, but do not
  upgrade the public desktop posture until production Developer ID clean-host
  acceptance is green.
- Add simulator or device-backed iOS acceptance before claiming an active iOS
  product surface.

## Posture

Treat Apple surfaces as buildable and manually explorable, but experimental.

That means:

- keep the Swift and Rust Apple code paths compiling
- keep macOS packaging and codesigning flows functional
- allow manual TestFlight or local FileProvider experiments
- avoid claiming production-ready Finder or iOS parity until stronger evidence
  exists

## Why `swift/fileprovider` And `swift/ios` Both Exist

- `swift/fileprovider` is the macOS packaging lane: FileProvider bundle
  assembly, Finder-related integration, notarization helpers, and macOS app
  artifacts
- `swift/ios` is the iOS lane: host app, iOS FileProvider extension, xcodegen
  project spec, and manual TestFlight or upload tooling

They are related, but they do not represent the same shipping surface.

## Exit Criteria To Become An Active Release Target

- A production macOS Finder/FileProvider smoke path for clean-host enablement
  plus mutate/conflict/status behavior
- Simulator or device-backed acceptance coverage for iOS
- A repeatable TestFlight or equivalent Apple distribution lane
- Docs that can point to those validation surfaces directly
