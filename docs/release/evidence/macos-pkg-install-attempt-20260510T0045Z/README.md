# macOS Published Package Install Attempt

Created: 2026-05-10T00:45Z

This packet records a non-interactive attempt to install the published
`v0.12.12` macOS package into `/Applications/TCFSProvider.app` on neo.

Package:

```text
/tmp/tcfs-published-pkg-v0.12.12/tcfs-0.12.12-macos-aarch64.pkg
```

Result: blocked. `sudo -n installer ...` exited with status 1 because a
password is required in this shell. `/Applications/TCFSProvider.app` remained
absent after the attempt. No stale user app was quarantined.

This is not Finder readiness. It only records that the published package cannot
be installed from this non-interactive session without privilege elevation.
