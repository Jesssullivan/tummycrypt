# macOS FileProvider Candidate Package Assessment - 2026-05-16

This packet records a non-installing assessment of the local candidate package
from `macos-fileprovider-candidate-pkg-20260516T190702Z/`.

It proves:

- `pkgutil --check-signature` accepts the package as Developer ID Installer
  signed with a trusted timestamp.
- `pkgutil --payload-files` and `pkgutil --expand-full` confirm the expected
  package payload shape:
  - `/usr/local/bin/tcfs`
  - `/usr/local/bin/tcfsd`
  - `/Applications/TCFSProvider.app`
  - `/Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex`
  - `Scripts/postinstall`
- Gatekeeper install assessment rejects the candidate package:
  `source=Unnotarized Developer ID`, exit code 3.
- `xcrun stapler validate -v` reports that the package has no stapled ticket,
  exit code 65.

It does not prove:

- notarization submission success
- stapling success
- installation into `/Applications/TCFSProvider.app`
- LaunchServices/PlugInKit cleanup
- Finder/CloudStorage lifecycle readiness

## Artifacts

- `context.env` - package and expansion paths
- `spctl-install-assessment.txt` - Gatekeeper install assessment
- `stapler-validate.txt` - stapled-ticket validation
- `pkg-signature.txt` - package signature output
- `pkg-payload-files.txt` - package payload listing
- `pkg-expand-full.txt` - expansion command result
- `pkg-expanded-tree.txt` - expanded package tree
- `pkg-expanded-file-sha256.txt` - hashes of expanded package files
- `policy-structure-smoke.txt` - the repo package smoke with
  `--require-signature`, `--require-gatekeeper-install`, and
  `--require-stapled-ticket`; it fails at Gatekeeper install assessment with
  exit code 3

## Next Gate

Submit this candidate package or the next generated package to Apple's notary
service, staple the accepted ticket, rerun `spctl --assess --type install` and
`xcrun stapler validate`, then proceed to the intentional install/preflight
lane.
