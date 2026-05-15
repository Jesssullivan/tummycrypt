# TCFS Symlink Config Probe

This packet isolates the git-repo canary symlink blocker with a tiny disposable
source tree containing `target.txt` and `link.txt -> target.txt`.

Result: the installed Homebrew `tcfs 0.12.12` binary skipped the symlink even
with `sync_symlinks = true`, while the freshly built source binary at
`target/codex-verify/debug/tcfs` preserved the symlink and uploaded two entries.
Both binaries report `0.12.12`; `result.env` records their distinct SHA-256s so
the blocker is traceable as packaged-binary divergence.

This does not claim production readiness, Finder readiness, or broad home/repo
takeover. It narrows the blocker to packaged-binary divergence until a rebuilt
package is published and re-proven.

Files:

- `homebrew.toml` / `homebrew.log`: installed Homebrew binary config and output.
- `debug.toml` / `debug.log`: source-built debug binary config and output.
- `result.env`: machine-readable verdict and proof boundaries.
