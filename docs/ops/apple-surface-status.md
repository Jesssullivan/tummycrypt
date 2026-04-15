# Apple Surface Status

As of 2026-04-15, the Apple lane is an experimental surface, not an active
release target.

## Current Evidence

- CI proves Rust staticlib builds plus Swift type-check coverage.
- Release automation can package macOS artifacts, including a FileProvider app
  bundle and Apple Silicon `.pkg`.
- The repo contains real macOS and iOS FileProvider code paths.

## What Is Not Yet Proven

- No continuously exercised iOS simulator or device acceptance lane.
- No automated TestFlight promotion or verification lane.
- No named macOS Finder/FileProvider smoke path comparable to the `neo-honey`
  fleet lane.
- No repo-wide evidence that Finder badges, hydration, and conflict flows are
  exercised end-to-end on every release.

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
  project spec, and manual TestFlight/upload tooling

They are related, but they do not represent the same shipping surface.

## Exit Criteria To Become An Active Release Target

- Simulator or device-backed acceptance coverage for iOS
- A real macOS Finder/FileProvider smoke path
- A repeatable TestFlight or equivalent Apple distribution lane
- Docs that can point to those validation surfaces directly
