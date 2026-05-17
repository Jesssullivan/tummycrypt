# macOS FileProvider neo Finder Release Smoke Direct-Host Attempt

Created: 2026-05-17T01:54:11Z

This smoke attempt used the direct host-app executable path to make domain and
download-request logging deterministic.

Result: `status=201`

The packet proves:

- strict installed preflight passed
- daemon storage reported `[ok]`
- the host app added the FileProvider domain
- CloudStorage enumeration returned remote-backed entries
- the host app requested download for `shared/alpha-test.txt`

The run was terminated before the read/hydration result became claimable. It is
superseded by the later coordinated-read and raw-`cat` packets.

Claim boundary: retained as intermediate direct-host diagnostic evidence only.
