# TCFS Git Repo Canary Summary

This packet is a shadow-first git repo canary. It is safe to use for planning
and evidence gathering because it snapshots the selected worktree into an
isolated shadow before any TCFS push or remote proof.

- Source: `/Users/jess/git/oauth-mux`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/oauth-mux-shadow-20260515T003543Z`
- Remote: `seaweedfs://100.64.48.53:8333/tcfs/git-repo-canary-oauth-mux-concurrent-20260515T003542Z`
- Branch: `codex/runtime-identity-status-artifact`
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
