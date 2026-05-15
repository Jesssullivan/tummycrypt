# TCFS Git Repo Canary Symlink Push Blocker

This packet is not scoped project-tree parity evidence.

It records a live `oauth-mux` shadow push attempt against a disposable
SeaweedFS prefix:

- Source: `/Users/jess/git/oauth-mux`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/oauth-mux-shadow-20260515T003543Z`
- Remote: `seaweedfs://100.64.48.53:8333/tcfs/git-repo-canary-oauth-mux-concurrent-20260515T003542Z`
- Local binary: `/opt/homebrew/opt/tcfs/bin/tcfs` reporting `tcfs 0.12.12`
- Honey binary staged at: `/tmp/tcfs-0.12.12-linux-x86_64-canary/tcfs-0.12.12-linux-x86_64/tcfs`

The push transcript begins with nine `skipping symlink
(follow_symlinks=false)` rows for the source symlinks. That blocks the live
repo dogfood lane because repo parity requires symlink entries to publish and
rehydrate as symlinks with exact matching targets.

This run was intentionally stopped after the blocker was observed. It does not
claim push completion, honey traversal, Linux lifecycle, Finder readiness,
broad `~/git`, or home-directory management.
