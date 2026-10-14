# Darwin Bazel Package Contract

TCFS macOS packaging is still owned by the existing release scripts and
workflows. The Bazel surface added here gives GloriousFlywheel a finite
downstream target to classify before Darwin RBE dispatch work starts.

## Current Targets

- `//build/macos:darwin_package_fixture_contract`
- root alias: `//:darwin_package_fixture_contract`
- `//build/macos:darwin_package_release_artifacts_unsigned`
- root alias: `//:darwin_package_release_artifacts_unsigned`
- rule: `tcfs_macos_pkg` in `build/macos/darwin_pkg.bzl`

The fixture target builds a package from declared fake CLI and FileProvider
artifacts. It exists to validate the Bazel rule, target shape, and script
wiring. It is intentionally named as a fixture contract so it cannot be
mistaken for a production release package.

The release-artifact target builds from the published `v0.12.14` macOS CLI
tarball and FileProvider zip through pinned `http_file` repositories. It is
non-fixture, but it is still an unsigned package-assembly target. It does not
rebuild current source, submit to Apple notarization, staple a ticket, or prove
Developer ID installer signing custody.

## Rule Contract

`tcfs_macos_pkg` wraps `scripts/macos-build-pkg.sh` and requires:

- a version string
- a declared macOS CLI tarball label
- a declared FileProvider zip label
- the package postinstall script
- optional installer signing identity

The rule produces one `.pkg` output and passes
`TCFS_PKG_STRUCTURE_SMOKE` as a declared tool. It does not discover unpinned
release artifacts, notarize, staple, or discover signing credentials.

## Promotion Boundary

The release-artifact target is the first non-fixture package target, but it is
not by itself a signed/notarized Darwin RBE candidate. Signed, notarized, or
stapled claims still require GloriousFlywheel Darwin signing-custody evidence
and a target or proof lane that runs under executor-side signing custody without
public/shared action-cache writes for secret-bearing steps.

The existing blocked `//:darwin_package` placeholder should stay blocked until
a non-fixture production target exists.
