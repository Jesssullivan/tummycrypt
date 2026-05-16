# macOS FileProvider Candidate Package - 2026-05-16

This packet records a non-installing local candidate `.pkg` build on `neo`
using current source-built `tcfs`/`tcfsd` binaries and the source-built
Developer ID signed `TCFSProvider.app` from
`macos-fileprovider-signed-app-preflight-20260516T183213Z/`.

It proves:

- `target/release/tcfs` and `target/release/tcfsd` were built locally from the
  current checkout and reported `0.12.12`.
- A local `tcfs-0.12.12-macos-aarch64.tar.gz` was assembled with those two
  binaries.
- The signed `TCFSProvider.app` was zipped with the release workflow's
  `ditto --keepParent` shape.
- `scripts/macos-build-pkg.sh` produced a `.pkg` containing:
  - `usr/local/bin/tcfs`
  - `usr/local/bin/tcfsd`
  - `/Applications/TCFSProvider.app`
  - `/Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex`
  - the repo-managed `scripts/macos-pkg-postinstall.sh`
- The package was signed with
  `Developer ID Installer: John Sullivan (QP994XQKNH)` using Apple's timestamp
  authority.
- `scripts/macos-pkg-structure-smoke.sh --require-signature` passed.

It does not prove:

- installation into `/Applications/TCFSProvider.app`
- LaunchServices/PlugInKit registration cleanup
- app launch, domain registration, or shared-Keychain runtime config
- CloudStorage/Finder enumeration, hydration, evict/rehydrate, mutation, or
  conflict/status UX
- clean-host production Finder readiness

The generated package remains local/generated at:

```text
build/macos-fileprovider-candidate-pkg-20260516T190702Z/tcfs-0.12.12-macos-aarch64.pkg
```

Only command logs, package metadata, payload listing, signature output, hashes,
and sizes are committed in this packet.

## Artifacts

- `context.env` - generated artifact paths and status
- `package-build.log` - package preparation, `macos-build-pkg.sh`, signature,
  structure-smoke, hash, and size transcript
- `artifact-sha256.txt` - SHA-256 for the local CLI tarball, FileProvider zip,
  and package
- `artifact-size.txt` - artifact sizes
- `pkg-payload-files.txt` - `pkgutil --payload-files` output
- `pkg-signature.txt` - `pkgutil --check-signature` output

## Next Gate

Install this candidate or the next published `.pkg` into
`/Applications/TCFSProvider.app` with admin authorization, inventory and clean
stale user/build PlugInKit registrations intentionally, run full strict
production preflight against the installed app, then run the Finder/FileProvider
lifecycle smoke.
