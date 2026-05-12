# TCFS Fleet Parity Pilot Evidence

Created: 2026-05-11T07:52:47Z

This bundle is an isolated fleet-pilot packet. It does not target real
`~/Documents` or `~/git` unless `--allow-real-roots` was explicitly used.

Remote:

```text
seaweedfs://100.64.48.53:8333/tcfs/home-canary-linux-xr-shadow-20260511T040100Z/linux-lifecycle
```

Pilot root:

```text
/Users/jess/TCFS Pilot/runs/fleet-parity-20260511T075241Z-20286
```

Contents:

- `desktop-honey/`: output from the existing desktop-to-honey lazy helper
- `fleet-pilot-tree.txt`: local isolated pilot tree
- `fleet-documents-expected.txt`: exact content for honey hydration smoke
- `honey-fleet-commands.txt`: extra honey commands for Documents/git traversal
- `honey-fleet-run.log`: extra honey smoke transcript, when run
- `honey-linux-lifecycle-commands.txt`: optional companion commands for
  honey-side mounted write/readback, cache rehydrate, and safe-unsync proof
- `honey-linux-lifecycle.log`: companion lifecycle transcript, when run
- `linux-lifecycle/`: companion lifecycle evidence copied back from honey,
  when run
- `linux-lifecycle-status.env`: whether the companion lifecycle ran
- `neo-honey-smoke.log`: live backend smoke transcript, when requested
- `neo-honey-status.env`: whether `just neo-honey-smoke` ran

Current proof boundary:

- plan-only bundles prove command shape and safe isolated fixture generation
- bundles with `push=1` prove remote seed command execution
- bundles with `run_honey=1` prove honey mounted traversal/hydration for the
  generated pilot fixture
- bundles with `run_neo_honey=1` also include live SeaweedFS/NATS sync proof
- bundles with `run_linux_lifecycle=1` also include a honey-side Linux
  lifecycle companion under a nested disposable prefix; this proves mounted
  write/readback, cache clear/rehydrate, and recursive safe-unsync, but it is
  still not a real `~/Documents` or `~/git` takeover
