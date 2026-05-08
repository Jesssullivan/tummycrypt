# Linux FUSE Lazy Lifecycle Evidence

Captured: 2026-05-08T17:08:50Z, completed 2026-05-08T17:10:06Z.

Host: `honey`

Source: current workspace staged onto honey at
`/tmp/tcfs-parity-proof-20260508T170556Z`, then run through the repo dev shell
so the pinned Rust/tooling surface was used.

Command shape:

```bash
nix develop --accept-flake-config --command task lazy:linux-lifecycle-demo
```

Remote:

```text
seaweedfs://100.64.48.53:8333/tcfs/lazy-linux-20260508T170825Z
```

Result:

- `status=0`
- mounted the disposable remote prefix through FUSE3
- proved `find` traversal before file-body hydration
- hydrated exact 77-byte content with `cat`
- wrote and edited `docs/deep/mounted-write.txt` through the mounted view
- pulled the mounted-write fixture back from remote and matched exact edited content
- cleared the VFS cache and rehydrated exact content
- refused recursive `tcfs unsync` while `docs/README.md` was dirty
- converted clean tracked descendants to `.tc` stubs and persisted
  `sync state: not_synced`

Files:

- `run-metadata.env`: endpoint, bucket, prefix, backend, and command shape
- `transcript.log`: full harness transcript
- `mount.log`: FUSE mount log copy
- `tcfs.toml`: generated temp config for this run
- `mounted-write-remote-pull.log`: exact remote pullback proof for mounted edit
- `unsync-dirty.out`: dirty-descendant refusal proof
- `unsync-success.out`: recursive clean unsync conversion proof
- `unsync-status.out`: persisted `NotSynced` status proof
- `result.env`: completion timestamp and exit status

No S3 credential values are archived in this bundle.
