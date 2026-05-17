# macOS FileProvider neo Finder Release Smoke Attempt

Created: 2026-05-17T01:32:41Z

This smoke attempt used the normal `open`-based host app launch.

Result: `status=201`

The packet proves:

- strict installed preflight passed
- `/usr/local/bin/tcfs` and `/usr/local/bin/tcfsd` reported `0.12.12`
- daemon storage reported `[ok]`
- one PlugInKit registration pointed at `/Applications/TCFSProvider.app`

The run stalled at host-app launch and was terminated before a useful
FileProvider lifecycle result. It is superseded by the later
`--direct-host-launch` packets.

Claim boundary: retained as the evidence that normal LaunchServices polling was
not enough for deterministic local proof.
