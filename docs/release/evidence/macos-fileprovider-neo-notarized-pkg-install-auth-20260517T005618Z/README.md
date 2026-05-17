# macOS FileProvider neo Notarized Package Install

Created: 2026-05-17T01:02:50Z

This packet archives the first authenticated local install of the notarized
workflow artifact from GitHub Actions run `25973109986` on `neo`.

The package source was:

- `/tmp/tcfs-notarized-pkg-25973109986/tcfs-0.12.12-macos-aarch64.pkg`

Install mode: `osascript`

Install status: `0`

The install placed `TCFSProvider.app` under `/Applications` and strict
preflight verified the installed host app and extension signing/profile
material. The packet is still a blocker for Finder lifecycle because preflight
then found two visible PlugInKit registrations: the new `/Applications`
extension plus the stale `~/Applications/TCFSProvider.app` extension. Cleanup
is intentionally not automatic.

Claim boundary: this proves authenticated local install of the notarized
artifact. It does not prove clean PlugInKit state or Finder/FileProvider
hydration.
