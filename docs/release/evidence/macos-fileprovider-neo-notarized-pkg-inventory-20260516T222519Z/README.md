# macOS FileProvider neo Cleanup Packet

Created: 2026-05-16T22:25:24Z

This packet archives divergence before cleanup: binary versions, PATH
resolution, app bundle locations, PlugInKit records, signing/profile state,
CloudStorage roots, configs, sockets, launchd labels, and a bounded
`~/tcfs` inventory. Sensitive-looking config paths are redacted from the
bounded listings by name.

The package source is the published `.pkg` when `--pkg` is provided. Stale
`~/Applications` or build-tree apps are moved only when
`--quarantine-stale` is explicitly set, after this inventory exists.

Install status: `not-run`.

Local strict package validation was rerun against the downloaded notarized
workflow artifact and captured in `local-strict-pkg-structure-smoke.out` plus
`local-strict-pkg-structure-smoke.err`. The downloaded artifact SHA-256 is
captured in `downloaded-artifact-sha256.txt`.

Strict production-adjacent Finder smoke remains blocked unless
`TCFS_REQUIRE_PRODUCTION_SIGNING=1 task lazy:macos-finder-preflight` passes.
This run's strict preflight status: `not-run`.
