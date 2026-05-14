# TCFS linux-xr Storage Posture Lifecycle Companion

Run: `home-canary-linux-xr-storage-posture-lifecycle-20260514T213826Z`

This packet reuses the completed release-binary storage-posture prefix from
`home-canary-linux-xr-storage-posture-20260514T021513Z` and adds the missing
same-prefix honey traversal plus Linux lifecycle companion in a new evidence
directory. It does not recopy `/Users/jess/git/linux-xr` and does not rerun the
7.7 GB push.

- Base remote:
  `seaweedfs://100.64.48.53:8333/tcfs/home-canary-linux-xr-storage-posture-20260514T021513Z`
- Reused shadow:
  `/Users/jess/TCFS Pilot/real-canaries/linux-xr-storage-posture-20260514T021513Z`
- Reused state:
  `docs/release/evidence/home-canary-linux-xr-storage-posture-20260514T021513Z/state/push-state.json`
- Reused push log:
  `push.log.gz` is a relative symlink to the original storage packet.
- Local wrapper binary:
  `/opt/homebrew/bin/tcfs`, `tcfs 0.12.12`,
  SHA-256 `b93824d91de94ecdd2c8aaf4d3d81555b31ecafa31bb6e6ff87485b49d1e1083`
- Honey binary:
  `/tmp/tcfs-vfs-tc-ext-20260514T2000Z/tcfs-cli/bin/tcfs`,
  `tcfs 0.12.12`,
  SHA-256 `dc9b1758274b9c19d4ed470537486e989a23a07bb78e2f004d91eac56e946e43`

## Result

`parity-gates.env` reports:

```text
status=scoped-project-tree-parity-evidence-complete
reason=shadow push, mounted traversal/hydration, mounted symlink target verification, and Linux lifecycle companion passed for the isolated project-tree canary
```

The honey mounted smoke passed against the large `linux-xr` prefix:

- `find -maxdepth 8` completed.
- Exact `.clang-format` hydration passed.
- All 85 mounted symlink target checks passed.
- `honey-linux-xr-shadow.log` and `honey-mount.log` recorded 0 actual
  `WARN`, `ERROR`, or `NoSuchKey` rows.

The Linux lifecycle companion passed under nested remote prefix
`home-canary-linux-xr-storage-posture-20260514T021513Z/linux-lifecycle/linux-lifecycle`:

- traversal before hydration
- exact `cat` hydration
- mounted write/edit with exact remote pullback
- cache clear and exact rehydrate
- dirty recursive safe-unsync refusal
- clean recursive safe-unsync success with `NotSynced` status

## Claim Boundary

This closes the storage packet's missing lifecycle row for scoped
project-tree parity evidence. It still does not make a production S3 posture
claim:

- endpoint is plaintext tailnet HTTP SeaweedFS
- credentials were forwarded through AWS-style environment variables for this
  lab run
- socket highwater was not rerun and remains from the original storage packet:
  highwater 11 while configured upload concurrency was 8
- `.idx` and generated-header object counts remain measured follow-ups
- the lifecycle sub-fixture naturally records first-write `NoSuchKey` checks
  while publishing new fixture objects
- production Finder/FileProvider and broad home-directory takeover remain
  separate gates
