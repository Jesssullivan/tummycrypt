# TCFS Git Repo Canary Summary

This packet is a shadow-first git repo canary. It is safe to use for planning
and evidence gathering because it snapshots the selected worktree into an
isolated shadow before any TCFS push or remote proof.

- Source: `/Users/jess/git/linux-xr-fast`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/linux-xr-fast-nixpkg-shadow-20260516T005236Z`
- Remote: `seaweedfs://100.64.48.53:8333/tcfs/git-repo-canary-linux-xr-fast-sourcefix-index-20260516T045054Z`
- Branch: `xr/main`
- Dirty status entries: `0`
- Tracked files: `93055`
- Untracked files: `0`

Boundaries:

- This packet does not mutate the live source repo.
- This packet does not claim Finder/FileProvider production readiness.
- This packet does not claim broad `~/git`, `~/Documents`, dotfile, or home
  directory takeover.
- A live repo should not be physically moved into TCFS until a shadow packet
  proves restore-from-remote, cross-host rehydrate, and safe-unsync behavior.
