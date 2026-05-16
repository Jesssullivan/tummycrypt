# macOS FileProvider Package Notarization Proof - 2026-05-16

This packet records a real GitHub-hosted macOS package proof for TCFS
FileProvider packaging. It is not a local assessment of an already-built
candidate and not a release-publication run.

## Result

- Workflow: `macOS FileProvider Package Notarization Proof`
- Run: `25973109986`
- URL: `https://github.com/Jesssullivan/tummycrypt/actions/runs/25973109986`
- Commit: `102729927abc2c535aff6b31faa5ee4f9dc7a77a`
- Runner image: `macos-14-arm64`
- Package: `tcfs-0.12.12-macos-aarch64.pkg`
- Stapled package SHA-256:
  `c6fd1a6fd18638c53f0d0b88bc79249e65d08766d99853bef6896ee69bcd6d45`
- Outcome: success

## What Passed

- Built `tcfs`, `tcfsd`, `tcfs-tui`, `tcfs-mcp`, and the FileProvider bridge
  from source on the macOS runner.
- Imported Developer ID Application and FileProvider provisioning profiles.
- Built `TCFSProvider.app` and ran strict signing-only production preflight.
- Imported Developer ID Installer identity and built a signed `.pkg`.
- Submitted the `.pkg` to Apple notary service with `xcrun notarytool submit
  --wait`; Apple returned `Accepted`.
- Stapled the package and validated the stapled ticket.
- Ran Gatekeeper install policy assessment with `spctl --assess --type install`.
- Ran strict package smoke with `--require-signature`,
  `--require-gatekeeper-install`, and `--require-stapled-ticket`.

## Key Evidence

- `notarytool-submit.json`: `status=Accepted`
- `notarytool-log.json`: `statusSummary=Ready for distribution`
- `stapler-staple.log`: staple and validate action worked
- `stapler-validate.log`: stapled ticket lookup and validation passed
- `spctl-install-assessment.log`: `source=Notarized Developer ID`
- `strict-package-smoke.log`: signature, Gatekeeper install assessment, and
  stapled ticket checks passed
- `pkgutil-check-signature-after-staple.log`: Developer ID Installer signature
  and Apple notary trust passed
- `pkg-sha256-after-staple.txt`: final package SHA-256

## Boundaries

- This is a workflow artifact proof, not a GitHub Release publication.
- The package was not installed into `/Applications`.
- No PlugInKit cleanup or Finder/FileProvider lifecycle smoke ran.
- Production Finder readiness remains open until a notarized package is
  intentionally installed on a known-clean host, stale registrations are
  handled after inventory, strict installed preflight passes, and Finder
  lifecycle evidence is archived.
