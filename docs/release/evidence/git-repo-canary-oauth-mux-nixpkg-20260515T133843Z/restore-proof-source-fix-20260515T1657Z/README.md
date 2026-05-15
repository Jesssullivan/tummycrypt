# TCFS Git Repo Fresh-Tree Restore Proof, Source Fix

Created: 2026-05-15T17:02:55Z

Status: `passed`

Proof: `fresh-tree-restore-files-and-symlinks-empty-dirs-gap`

Reason: regular files and symlink targets restored exactly; empty directories are not restored by reconcile

This proof uses the source-built `target/debug/tcfs` after the remote-index and fresh-pull fixes. It preserves the original blocked `restore-proof/` directory and records this as a separate source-fix packet.

- Packet: `docs/release/evidence/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/oauth-mux-nixpkg-shadow-20260515T133844Z`
- Restore root: `/tmp/tcfs-restore-execute-skip-cleanup.XVFhGI`
- Remote prefix: `git-repo-canary-oauth-mux-nixpkg-20260515T133843Z`
- Execute wall time: `301.84s`
- Restored regular files: `4601`
- Restored symlinks: `9`
- Cleanup: skipped because the plan was pull-only and did not overwrite or delete remote data

Packaged Nix/Homebrew restore remains unproven until rebuilt with this source fix.
