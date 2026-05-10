# TCFS neo/honey Delete/Rename While Peer Unsynced Evidence

Created: 2026-05-10T04:05:02Z

This packet targets the M8 permutation:

1. neo creates and pushes `Projects/shared/delete-me.md` and `Projects/shared/rename-old.md`.
2. honey pulls both files into a physical sync root and runs `tcfs unsync`, so
   honey keeps only adjacent `.tc` stubs.
3. neo deletes `Projects/shared/delete-me.md` using `tcfs rm`.
4. neo renames `Projects/shared/rename-old.md` to `Projects/shared/rename-new.md` by deleting the old
   remote index entry, then publishing the new path.
5. honey verifies current behavior: old paths fail to rehydrate, the renamed new
   path hydrates exact bytes, and stale old stubs are recorded as an open product
   cleanup/tombstone gap.

Remote:

```text
seaweedfs://100.64.48.53:8333/tcfs/neo-honey-delete-rename-unsynced-20260510T040456Z
```

Important files:

- `neo-tree.txt`: isolated neo fixture tree before delete/rename
- `neo-tree-after-delete-rename.txt`: neo fixture tree after peer operations
- `tcfs-delete-rename-unsynced.toml`: disposable neo config copied from the state dir
- `honey-delete-rename-commands.txt`: manual honey commands
- `honey-prepare-unsync.log`: honey pull/unsync transcript, when run
- `neo-delete.log`: neo `tcfs rm` delete transcript, when run
- `neo-rename-push.log`: neo new-path publish transcript, when run
- `neo-rename-delete-old.log`: neo old-path remote delete transcript, when run
- `honey-verify-delete.log`: old-path pull failure and delete-stub status
- `honey-verify-rename.log`: old-path pull failure, new-path hydrate, and stale old-stub status
- `honey-evidence/`: detailed remote transcripts, copied back when available
- `result.env`: plan/current-behavior status

This helper uses an isolated neo root under `/Users/jess/TCFS Pilot/runs/delete-rename-unsynced-20260510T040456Z-56713/neo` and a honey root
`/tmp/tcfs-delete-rename-unsynced-20260510T040456Z-56713-honey/root`; it does not target real `~/Documents`, `~/git`, dotfiles,
or broad home-directory paths unless `--allow-real-roots` is explicitly
supplied.

Claimability note: this packet does not by itself make a user-facing
"renames/deletes clean stale peer placeholders" claim. TCFS currently lacks a
durable tombstone/stale-stub cleanup protocol for physical unsynced roots, so
that stronger claim remains open until product semantics and QA assertions are
accepted.
