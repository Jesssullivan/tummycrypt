# iOS Surface Status

As of April 15, 2026, iOS is an experimental FileProvider proof-of-concept,
not an active release target.

## What Exists In The Repo Today

- `swift/ios/` contains a host app, FileProvider extension, generated UniFFI
  bindings, `project.yml`, and manual signing or upload scripts.
- `tcfs-file-provider` exposes UniFFI bindings used by the iOS extension.
- CI runs `bash swift/ios/Scripts/build-ios.sh --simulator` on macOS runners.

## What CI Actually Proves

- the Rust staticlib builds for `aarch64-apple-ios-sim`
- generated Swift bindings and the Swift sources type-check against the iOS
  simulator SDK
- an Xcode project can be generated from `project.yml` when `xcodegen` is
  present

It does not prove:

- simulator UI automation
- device-backed Files.app behavior
- provisioning or signing reality on a real Apple account
- TestFlight processing
- App Store readiness

## Near-Term Product Scope

- Public posture: read-only FileProvider direction
- Maintain browsing, enumeration, and hydration as the documented intent
- Do not advertise upload, modify, delete, background sync, or conflict UX as
  supported iOS features, even though experimental hooks exist in code

## Why Read-Only Remains The Posture

- `swift/ios/Extension/FileProviderExtension.swift` contains create, modify, and
  delete hooks, but they are not backed by a continuously exercised acceptance
  lane.
- There is no simulator or device-backed end-to-end validation for iOS write
  operations.
- There is no repeatable TestFlight or App Store delivery lane.
- Release and support decisions would otherwise be based on scaffolded code
  rather than proof.

## Maintenance Expectation

- Keep the Rust and Swift iOS surfaces compiling.
- Keep `build-ios.sh --simulator` green in CI.
- Keep manual signing and upload tooling available for experiments.
- Treat iOS write paths and distribution tooling as experimental, not release
  blockers.

## Related Documents

- [Apple Surface Status](apple-surface-status.md) — cross-Apple posture
- [RFC 0003: iOS File Provider](../rfc/0003-ios-file-provider.md) — design and
  scaffold details

## Exit Criteria For A Stronger Public Posture

- simulator or device-backed acceptance coverage
- an explicit decision on whether write support is in or out of near-term scope
- a repeatable TestFlight or equivalent Apple distribution lane
- docs that can point to those validation surfaces directly
