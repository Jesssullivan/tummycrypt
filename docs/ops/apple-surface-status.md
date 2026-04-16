# Apple Surface Status

As of April 15, 2026, Apple support is a buildable and manually explorable lane,
not a continuously proven release target.

## macOS: Proven Today

- CI proves the Rust staticlib and Swift build surfaces needed for FileProvider
  packaging.
- Release automation can build Apple Silicon artifacts, package
  `TCFSProvider.app`, and publish `.pkg` plus tarball assets.
- The repo contains real macOS daemon, launchd, NFS loopback, and FileProvider
  code paths.

## macOS: Not Yet Proven As A Release-Grade Desktop Surface

- There is no continuously exercised Finder/FileProvider acceptance lane from
  install through register, enumerate, hydrate, mutate, and conflict handling.
- Finder badges, progress UI, and notification behavior are not release gates.
- Post-release smoke on April 15, 2026 showed that `v0.12.1` could install on
  Apple Silicon, but the shipped `tcfsd` failed at runtime because it linked
  Homebrew OpenSSL dylibs with an incompatible Team ID.
- Packaged macOS artifacts therefore still require explicit post-cut smoke even
  when CI and packaging are green.

## iOS: Current Posture

- The repo carries real iOS FileProvider and UniFFI code plus CI Swift
  type-check coverage.
- There is still no continuously exercised simulator or device acceptance lane.
- There is no repeatable TestFlight or App Store delivery path.
- Treat iOS as proof-of-concept and read-only in practice until stronger
  end-to-end evidence exists.

## Working Wording

Use:

- `macOS: CLI/daemon plus experimental FileProvider desktop surfaces`
- `iOS: proof-of-concept FileProvider direction`

Avoid:

- `macOS: full` or `production-ready`
- `iOS: active release target`
- claims that Finder badges, hydration, or conflict UX are release-verified

## Validation Path

- Keep the Apple CI lanes green.
- Run post-release distribution smoke from
  [Distribution Smoke Matrix](distribution-smoke-matrix.md).
- Add a named macOS Finder/FileProvider smoke path before upgrading the public
  posture.
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

- A real macOS Finder/FileProvider smoke path
- Simulator or device-backed acceptance coverage for iOS
- A repeatable TestFlight or equivalent Apple distribution lane
- Docs that can point to those validation surfaces directly
