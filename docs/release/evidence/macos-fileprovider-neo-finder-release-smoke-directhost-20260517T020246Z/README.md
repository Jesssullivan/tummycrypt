# macOS FileProvider neo Finder Release Smoke Coordinated-Read Blocker

Created: 2026-05-17T02:02:46Z

This production-signed local smoke used the direct host-app executable path.

Result: `status=201`

Proven in this packet:

- strict production preflight passed for `/Applications/TCFSProvider.app`
- `/usr/local/bin/tcfs` and `/usr/local/bin/tcfsd` reported `0.12.12`
- daemon storage reported `[ok]`
- one PlugInKit registration pointed at `/Applications/TCFSProvider.app`
- host app domain add succeeded
- CloudStorage enumeration returned remote-backed entries
- host app `requestDownload` for `shared/alpha-test.txt` returned `OK`

Blocked before hydration proof:

- the Swift coordinated-read helper selected a mismatched Nix SDK/toolchain and
  failed with `no such module 'SwiftShims'`
- `fileproviderctl check` reported reconciliation failures on `1/129` files

Claim boundary: this is real production package/Finder-path progress, but it
does not prove FileProvider read/hydration.
