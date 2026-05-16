# neo Notarized Package Install Blocker - 2026-05-16

This packet attempted the next production Finder gate with the notarized
workflow artifact:

- package: `/tmp/tcfs-notarized-pkg-25973109986/tcfs-0.12.12-macos-aarch64.pkg`
- install command: `sudo -n installer -pkg "$pkg" -target /`
- install status: `1`
- installer stderr: `sudo: a password is required`

No package payload was installed into `/Applications` by this run. The strict
preflight that followed failed at the correct first gate:

- `/Applications/TCFSProvider.app` was not present
- `TCFS_REQUIRE_PRODUCTION_SIGNING=1 task lazy:macos-finder-preflight` exited
  non-zero before Finder/FileProvider lifecycle smoke

This is blocker evidence only. It preserves the host state before the install
attempt and confirms the next required operation is an elevated installer run
against the same notarized package, followed by PlugInKit inventory/cleanup,
strict production preflight, and then Finder/FileProvider lifecycle smoke.
