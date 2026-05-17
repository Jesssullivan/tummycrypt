# macOS FileProvider neo Stale User-App Quarantine

Created: 2026-05-17T01:07:07Z

This packet archives the intentional cleanup step after the authenticated
notarized-package install packet existed.

The stale user app was moved under this evidence packet's
`quarantined-stale-apps/` directory. Preflight still failed because PlugInKit
continued to report the quarantined bundle as a registration target alongside
the canonical `/Applications/TCFSProvider.app` extension.

Install mode: `sudo-n`

Install status: `not-run`

Strict preflight status: `201`

Claim boundary: this proves the stale user-app bundle was quarantined only
after the install evidence existed. It does not prove PlugInKit cleanup or
Finder/FileProvider lifecycle.
