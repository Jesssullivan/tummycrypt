# macOS FileProvider Signed App Preflight - 2026-05-16

This packet records a non-installing local source build of
`TCFSProvider.app` on `neo` using the repo's production-signing path.

It proves:

- `cargo build -p tcfs-file-provider --release --no-default-features --features grpc`
  produced the Rust static library and generated C header.
- `swift/fileprovider/build.sh` auto-detected the local Developer ID
  Application identity:
  `Developer ID Application: John Sullivan (QP994XQKNH)`.
- The build auto-detected and embedded the compatible local Developer ID
  provisioning profiles for `io.tinyland.tcfs` and
  `io.tinyland.tcfs.fileprovider`.
- Production signing mode disabled embedded FileProvider config, so the app and
  extension fall back to Keychain/XDG/App Group runtime config.
- Strict signing-only preflight passed for the assembled app: host and
  extension codesign validation passed, App Group entitlements were present,
  concrete `QP994XQKNH.group.io.tinyland.tcfs` keychain-access-groups
  entitlements were present, embedded profiles decoded, and both profiles
  contained the signing certificate.

It does not prove:

- published `.pkg` install into `/Applications/TCFSProvider.app`
- LaunchServices or PlugInKit registration cleanup
- FileProvider domain registration
- CloudStorage/Finder enumeration, hydration, evict/rehydrate, mutation, or
  conflict/status UX
- production clean-host Finder readiness

The assembled bundle lives outside the evidence packet at:

```text
build/fileprovider-signed-app-preflight-20260516T183213Z/TCFSProvider.app
```

That build output is local/generated and is not committed. This evidence packet
keeps the command transcripts and signing metadata needed to advance the next
package/install proof.

## Artifacts

- `context.env` - timestamp, build path, Rust header path, and wrapper note
- `build.log` - source app build transcript; the app build and strict preflight
  reached `==> Done`, then the zsh wrapper failed while trying to read
  Bash-only `PIPESTATUS`
- `profile-inventory.log` - strict local host/extension profile inventory
- `signing-preflight.log` - direct strict signing-only preflight rerun
- `host-codesign.txt` / `extension-codesign.txt` - codesign metadata
- `host-entitlements.plist` / `extension-entitlements.plist` - signed
  entitlements as observed by `codesign`
- `host-info-plist.txt` / `extension-info-plist.txt` - decoded bundle plists
- `bundle-sha256.txt` / `bundle-size.txt` - generated bundle file hashes and
  size

## Next Gate

Build a candidate `.pkg` from this signed app path, install it into
`/Applications/TCFSProvider.app` with admin authorization, inventory and clean
stale user/build PlugInKit registrations intentionally, rerun strict production
preflight against the installed app, and only then run a Finder/FileProvider
lifecycle smoke.
