# TCFS neo Mounted Reverse-Read Evidence

Created: 2026-05-10T03:58:27Z

This packet targets M4: honey publishes bytes, neo removes its physical copy
with `tcfs unsync`, honey publishes newer bytes, then neo reads the same
relative path through a mounted clean-name view. The physical neo sync root
should remain stub-only after the mounted `cat`; this proves mounted
on-demand read behavior rather than physical `tcfs pull` rehydrate.

Remote:

```text
seaweedfs://100.64.48.53:8333/tcfs/neo-mounted-reverse-read-20260510T035826Z
```

Important files:

- `honey-mounted-reverse-commands.txt`: manual honey and neo commands
- `honey-initial-push.log`: honey initial publish transcript, when run
- `neo-initial-pull.log`: neo physical pull transcript, when run
- `neo-unsync.out`: neo physical unsync transcript, when run
- `neo-sync-status-after-unsync.out`: neo physical status after unsync
- `honey-mutated-push.log`: honey updated publish transcript, when run
- `neo-mount.log`: neo mount transcript, when mount startup runs
- `neo-mounted-read.log`: neo mounted `ls`/`find`/`cat` transcript, when mounted read runs
- `neo-physical-stub-after-mounted-read.env`: physical root state after mount read
- `tcfs-mounted-reverse-read.toml`: disposable neo config copied from the state dir
- `result.env`: pass/plan-only/failure status

Claimability note: this helper stages the M4 mounted reverse-read row. It does
not prove production Finder, broad home-directory management, or clean
delete/rename semantics.
