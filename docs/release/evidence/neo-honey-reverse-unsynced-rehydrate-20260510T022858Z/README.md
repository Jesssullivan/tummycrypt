# TCFS neo/honey Reverse Unsynced Rehydrate Evidence

Created: 2026-05-10T02:29:00Z

This packet targets the reverse same-fixture permutation:

1. neo creates and pushes `Projects/shared/reverse-notes.md` to a disposable prefix.
2. honey pulls that file into a physical sync root and runs `tcfs unsync`, so
   honey keeps only `Projects/shared/reverse-notes.md.tc`.
3. neo mutates and pushes the same relative path.
4. honey runs `tcfs pull Projects/shared/reverse-notes.md` and must receive neo's exact content.
5. honey's adjacent `.tc` stub must be gone after rehydrate.

Remote:

```text
seaweedfs://100.64.48.53:8333/tcfs/neo-honey-reverse-unsynced-rehydrate-20260510T022858Z
```

Important files:

- `neo-tree.txt`: isolated neo fixture tree
- `tcfs-reverse-unsynced-rehydrate.toml`: disposable neo config copied from the state dir
- `honey-reverse-commands.txt`: manual honey commands
- `honey-prepare-unsync.log`: honey pull/unsync transcript, when run
- `honey-rehydrate.log`: honey rehydrate transcript, when run
- `honey-evidence/`: detailed remote transcripts, copied back when available
- `neo-initial-push.log`: neo initial push transcript, when pushed
- `neo-mutated-push.log`: neo mutation push transcript, when honey was run
- `result.env`: pass/plan-only status

This helper uses an isolated neo root under `/Users/jess/TCFS Pilot/runs/reverse-unsynced-rehydrate-20260510T022858Z-50755/neo` and a honey root
`/tmp/tcfs-neo-honey-reverse-unsynced-rehydrate-20260510T022858Z-honey/root`; it does not target real `~/Documents`, `~/git`, dotfiles,
or broad home-directory paths unless `--allow-real-roots` is explicitly
supplied.
