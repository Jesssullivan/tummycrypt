# TCFS neo/honey Reverse Unsynced Rehydrate Evidence

Created: 2026-05-10T04:22:07Z

This packet targets the reverse same-fixture permutation:

1. neo creates and pushes `Projects/shared/reverse-notes.md` to a disposable prefix.
2. honey pulls that file into a physical sync root and runs `tcfs unsync`, so
   honey keeps only `Projects/shared/reverse-notes.md.tc`.
3. neo mutates and pushes the same relative path.
4. honey either runs `tcfs pull Projects/shared/reverse-notes.md` and must receive neo's exact
   content, or, with `--honey-mounted-read`, reads the latest bytes through a
   mounted clean-name view while the physical root remains stub-only.
5. In pull mode, honey's adjacent `.tc` stub must be gone after rehydrate. In
   mounted-read mode, the physical stub must remain present after mounted
   `cat`.

Remote:

```text
seaweedfs://100.64.48.53:8333/tcfs/honey-mounted-reverse-read-20260510T042203Z
```

Important files:

- `neo-tree.txt`: isolated neo fixture tree
- `tcfs-reverse-unsynced-rehydrate.toml`: disposable neo config copied from the state dir
- `honey-reverse-commands.txt`: manual honey commands
- `honey-prepare-unsync.log`: honey pull/unsync transcript, when run
- `honey-rehydrate.log`: honey rehydrate transcript, when run
- `honey-mounted-read.log`: honey mounted clean-name read transcript, when
  `--honey-mounted-read` is used
- `honey-evidence/`: detailed remote transcripts, copied back when available
- `neo-initial-push.log`: neo initial push transcript, when pushed
- `neo-mutated-push.log`: neo mutation push transcript, when honey was run
- `result.env`: pass/plan-only status

This helper uses an isolated neo root under `/Users/jess/TCFS Pilot/runs/reverse-unsynced-rehydrate-20260510T042204Z-92360/neo` and a honey root
`/tmp/tcfs-reverse-unsynced-rehydrate-20260510T042204Z-92360-honey/root`; it does not target real `~/Documents`, `~/git`, dotfiles,
or broad home-directory paths unless `--allow-real-roots` is explicitly
supplied.
