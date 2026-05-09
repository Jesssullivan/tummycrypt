# TCFS Fleet Parity Pilot Evidence

Created: 2026-05-09T19:15:48Z

This bundle is an isolated fleet-pilot packet. It does not target real
`~/Documents` or `~/git` unless `--allow-real-roots` was explicitly used.

Remote:

```text
seaweedfs://100.64.48.53:8333/tcfs/fleet-pilot-20260509T1919Z
```

Pilot root:

```text
/private/tmp/tcfs-fleet-pilot-20260509T1919Z/root
```

Contents:

- `desktop-honey/`: output from the existing desktop-to-honey lazy helper
- `fleet-pilot-tree.txt`: local isolated pilot tree
- `fleet-documents-expected.txt`: exact content for honey hydration smoke
- `honey-fleet-commands.txt`: extra honey commands for Documents/git traversal
- `honey-fleet-run.log`: extra honey smoke transcript, when run
- `neo-honey-smoke.log`: live backend smoke transcript, when requested
- `neo-honey-status.env`: whether `just neo-honey-smoke` ran

Result:

- `push=1`: seeded 7 files to the disposable remote prefix.
- `run_honey=1`: honey mounted the prefix with a current `tcfs 0.12.12` debug
  binary built from commit `17569d445c20`.
- honey mounted traversal showed `Documents`, `git/tcfs-pilot-repo`, `Notes`,
  `Photos`, and `Projects` without physical `.tc`/`.tcf` suffix leakage.
- honey hydrated exact content for `Projects/tcfs-odrive-parity/honey-readme.txt`
  and `Documents/fleet-readiness.md`.
- `run_neo_honey=1`: `just neo-honey-smoke` passed against
  `http://seaweedfs-tcfs:8333`, bucket `tcfs`, and `nats://nats-tcfs:4222`.
- The honey FUSE mount was unmounted after the run.

Current proof boundary:

- plan-only bundles prove command shape and safe isolated fixture generation
- bundles with `push=1` prove remote seed command execution
- bundles with `run_honey=1` prove honey mounted traversal/hydration for the
  generated pilot fixture
- bundles with `run_neo_honey=1` also include live SeaweedFS/NATS sync proof
- this packet does not prove production Finder, mounted write/readback,
  recursive safe-unsync, or real `~/Documents` / `~/git` takeover
