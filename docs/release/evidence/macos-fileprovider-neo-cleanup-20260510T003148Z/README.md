# macOS FileProvider neo Cleanup Packet

Created: 2026-05-10T00:33:38Z

This packet archives divergence before cleanup: binary versions, PATH
resolution, app bundle locations, PlugInKit records, signing/profile state,
CloudStorage roots, configs, sockets, launchd labels, and a bounded
`~/tcfs` inventory. Sensitive-looking config paths are redacted from the
bounded listings by name.

The package source is the published `.pkg` when `--pkg` is provided. Stale
`~/Applications` or build-tree apps are moved only when
`--quarantine-stale` is explicitly set, after this inventory exists.

Strict production-adjacent Finder smoke remains blocked unless
`TCFS_REQUIRE_PRODUCTION_SIGNING=1 task lazy:macos-finder-preflight` passes.
This run's strict preflight status: `not-run`.
