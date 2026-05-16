# macOS FileProvider neo Cleanup Packet

Created: 2026-05-16T02:40:09Z

This packet archives divergence before cleanup: binary versions, PATH
resolution, app bundle locations, PlugInKit records, signing/profile state,
CloudStorage roots, configs, sockets, launchd labels, and a bounded
`~/tcfs` inventory. Sensitive-looking config paths are redacted from the
bounded listings by name.

The package source is the published `.pkg` when `--pkg` is provided. Stale
`~/Applications` or build-tree apps are moved only when
`--quarantine-stale` is explicitly set, after this inventory exists.

Install status: `1`.

Key findings:

- The selected published `.pkg` passed checksum/signature/notarization checks.
- The stale user app was quarantined:
  `/Users/jess/Applications/TCFSProvider.app` ->
  `quarantined-stale-apps/Applications-TCFSProvider.app`.
  The local app-bundle copy is intentionally gitignored; `quarantine-actions.log`
  is the committed evidence record.
- `sudo -n installer` failed with `sudo: a password is required`, so the
  package was not installed into `/Applications`.
- `/usr/local/bin/tcfs` and `/Applications/TCFSProvider.app` should not be
  treated as installed/verified by this packet.

Strict production-adjacent Finder smoke remains blocked unless
`TCFS_REQUIRE_PRODUCTION_SIGNING=1 task lazy:macos-finder-preflight` passes.
This run's strict preflight status: `201`.

No production Finder claim is made from this packet.
