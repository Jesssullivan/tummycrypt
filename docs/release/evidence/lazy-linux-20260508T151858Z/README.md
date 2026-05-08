# Linux FUSE Lazy Hydration Evidence

Captured: 2026-05-08T15:18:58Z prefix, completed 2026-05-08T15:21:55Z

Host: `honey`

Source: current `main` staged onto honey with `git archive`, then run through
the repo dev shell so the pinned Rust/tooling surface was used.

Command shape:

```bash
nix develop --accept-flake-config --command task lazy:linux-demo
```

Remote:

```text
seaweedfs://100.64.48.53:8333/tcfs/lazy-linux-20260508T151858Z
```

Result:

- `status=0`
- seeded 2 files into a disposable SeaweedFS prefix
- mounted the prefix through FUSE3
- proved `find`/`ls` traversal before content hydration
- read `docs/deep/remote.txt` through `cat` and matched exact 77-byte content
- observed cache fill after first `cat`
- cleared the mounted-surface cache and observed cache entries return to 0
- re-read the same file and observed rehydration

Files:

- `run-metadata.env`: endpoint, bucket, prefix, backend, and command shape
- `transcript.log`: full harness transcript
- `mount.log`: FUSE mount log copy
- `tcfs.toml`: generated temp config for this run
- `result.env`: completion timestamp and exit status

No S3 credential values are archived in this bundle.
