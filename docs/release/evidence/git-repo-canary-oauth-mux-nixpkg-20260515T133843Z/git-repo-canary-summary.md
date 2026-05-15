# TCFS Git Repo Canary Summary

This packet is a shadow-first git repo canary. It is safe to use for planning
and evidence gathering because it snapshots the selected worktree into an
isolated shadow before any TCFS push or remote proof.

- Source: `/Users/jess/git/oauth-mux`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/oauth-mux-nixpkg-shadow-20260515T133844Z`
- Remote: `seaweedfs://100.64.48.53:8333/tcfs/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z`
- Branch: `main`
- Dirty status entries: `0`
- Tracked files: `190`
- Untracked files: `0`

Boundaries:

- This packet does not mutate the live source repo.
- This packet does not claim Finder/FileProvider production readiness.
- This packet does not claim broad `~/git`, `~/Documents`, dotfile, or home
  directory takeover.
- A live repo should not be physically moved into TCFS until a shadow packet
  proves restore-from-remote, cross-host rehydrate, and safe-unsync behavior.
  This packet now has a source-built restore proof under
  `restore-proof-source-fix-20260515T1657Z/` for regular files and symlinks, with
  empty directories still recorded as a reconcile gap. The newer
  `restore-proof-source-fix-symlink-state-20260515T171712Z/` proof also records
  restored sync state for all 4,610 restored paths, including 9 symlinks.
  Packaged Nix/Homebrew restore remains unproven until rebuilt from the source
  fix.
