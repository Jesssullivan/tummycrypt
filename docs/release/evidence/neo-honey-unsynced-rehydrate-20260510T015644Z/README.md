# TCFS neo/honey Unsynced Rehydrate Evidence

Created: 2026-05-10T01:56:48Z

This packet targets the same-fixture permutation:

1. neo creates and pushes `Projects/shared/notes.md` to a disposable prefix.
2. neo runs `tcfs unsync` so the local file becomes `Projects/shared/notes.md.tc`.
3. honey opens the same remote file through a mounted view and writes new bytes.
4. neo runs `tcfs pull Projects/shared/notes.md` and must receive honey's exact content.
5. the adjacent `.tc` stub must be gone after rehydrate.

Remote:

```text
seaweedfs://100.64.48.53:8333/tcfs/neo-honey-unsynced-rehydrate-20260510T015644Z
```

Important files:

- `neo-tree.txt`: isolated neo fixture tree
- `tcfs-unsynced-rehydrate.toml`: disposable config copied from the state dir
- `honey-mutator-commands.txt`: manual honey commands
- `honey-mutator.log`: honey mounted traversal and mutation transcript, when run
- `unsync.out`: neo `tcfs unsync` transcript, when pushed
- `sync-status-after-unsync.out`: neo status after local remove
- `rehydrate-pull.log`: neo pull transcript, when honey was run
- `sync-status-after-rehydrate.out`: neo status after rehydrate
- `stub-status.env`: whether the stale `.tc` stub remained
- `result.env`: pass/plan-only status

This helper uses an isolated root under `/Users/jess/TCFS Pilot/runs/neo-honey-unsynced-rehydrate-20260510T015644Z/neo`; it does not target real
`~/Documents`, `~/git`, dotfiles, or broad home-directory paths unless
`--allow-real-roots` is explicitly supplied.
