# macOS FileProvider neo Finder Release Smoke Raw-Read Blocker

Created: 2026-05-17T02:04:17Z

This production-signed local smoke reran the direct host-app lane with the
coordinated Swift helper disabled, so the final read used plain `cat`.

Result: `status=201`

Proven in this packet:

- strict production preflight passed for `/Applications/TCFSProvider.app`
- `/usr/local/bin/tcfs` and `/usr/local/bin/tcfsd` reported `0.12.12`
- daemon storage reported `[ok]`
- one PlugInKit registration pointed at `/Applications/TCFSProvider.app`
- host app domain add succeeded
- CloudStorage enumeration returned remote-backed entries
- host app `requestDownload` for `shared/alpha-test.txt` returned `OK`

Current blocker:

- `cat /Users/jess/Library/CloudStorage/TCFSProvider-TCFS/shared/alpha-test.txt`
  failed with `Operation timed out`
- `fileproviderctl check` reported reconciliation failures on `1/129` files

Claim boundary: this is the current production Finder/FileProvider blocker on
`neo`. Do not claim production Finder lifecycle or hydration readiness until a
follow-up packet proves exact bytes through this installed package path.
