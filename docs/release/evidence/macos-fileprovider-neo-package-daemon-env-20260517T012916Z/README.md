# macOS FileProvider neo Package Daemon Environment

Created: 2026-05-17T01:29:16Z

This packet records the local daemon divergence and remediation after the
package install/preflight packets.

Before:

- stale user daemon process:
  `/Users/jess/Applications/TCFSDaemon.app/Contents/MacOS/tcfsd`
- package daemon process:
  `/usr/local/bin/tcfsd --config /Users/jess/.config/tcfs/config.toml --mode daemon`
- `tcfs status` reported storage `[UNREACHABLE]`
- launchd did not have the file-based S3 credential environment variables

Action:

- copied the existing key-file launch environment into the daemon's primary
  file-variable names without archiving secret values
- booted out the stale `dev.tinyland.tcfsd` daemon
- kickstarted the package `io.tinyland.tcfsd` daemon

After:

- only `/usr/local/bin/tcfsd` remained in the bounded process inventory
- `tcfs status` reported storage `[ok]`
- credentials loaded from `file:TCFS_S3_ACCESS_FILE`

Claim boundary: this proves the package daemon can reach storage on this host
when launchd receives file-backed credentials. It does not prove Finder
hydration.
