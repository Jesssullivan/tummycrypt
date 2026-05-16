# macOS FileProvider neo Cleanup Packet

Created: 2026-05-16T02:38:57Z

This packet archives divergence before cleanup: binary versions, PATH
resolution, app bundle locations, PlugInKit records, signing/profile state,
CloudStorage roots, configs, sockets, launchd labels, and a bounded
`~/tcfs` inventory. Sensitive-looking config paths are redacted from the
bounded listings by name.

The package source is the published `.pkg` when `--pkg` is provided. Stale
`~/Applications` or build-tree apps are moved only when
`--quarantine-stale` is explicitly set, after this inventory exists.

Install status: `not-run`.

Key findings:

- PlugInKit registered `io.tinyland.tcfs.fileprovider(0.2.0)` from
  `/Users/jess/Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex`.
- Gatekeeper rejected the local user app, and codesign reported ad-hoc
  signing shape rather than a production package install.
- Ambient `tcfs` was workspace `target/debug/tcfs` version `0.12.12`.
- Ambient `tcfsd` was `/Users/jess/.nix-profile/bin/tcfsd` version `0.12.2`.

Strict production-adjacent Finder smoke remains blocked unless
`TCFS_REQUIRE_PRODUCTION_SIGNING=1 task lazy:macos-finder-preflight` passes.
This run's strict preflight status: `201`.

Strict preflight failed on missing host/extension Keychain access-group
entitlements and embedded provisioning profiles. No production Finder claim is
made from this packet.
