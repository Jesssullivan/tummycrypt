# Mounted Follow-Up: Real `.tc` Extension Fix

This packet reruns honey mounted traversal against the existing storage-posture
prefix after changing the VFS lookup order so exact remote filenames win before
legacy physical-stub fallback.

- Base prefix:
  `seaweedfs://100.64.48.53:8333/tcfs/home-canary-linux-xr-storage-posture-20260514T021513Z`
- Original storage packet:
  `docs/release/evidence/home-canary-linux-xr-storage-posture-20260514T021513Z/`
- Earlier directory-prefix-only rerun:
  `docs/release/evidence/home-canary-linux-xr-storage-posture-vfsfix-20260514T193104Z/`
- This rerun:
  `docs/release/evidence/home-canary-linux-xr-storage-posture-tc-extfix-20260514T202343Z/`

## Result

| Run | Honey binary | `NoSuchKey` warnings | WARN rows | ERROR rows | Outcome |
| --- | --- | ---: | ---: | ---: | --- |
| Original mounted follow-up | release-derived `tcfs 0.12.12` | 274 | 274 | 0 | Traversal passed, warning root cause open |
| Directory-prefix VFS rerun | `73944d98906d75713f6beaad886f56ee12c1fee408a59220f98898f16d8a335f` | 274 | 274 | 0 | Traversal passed, warning count unchanged |
| Exact `.tc` filename rerun | `dc9b1758274b9c19d4ed470537486e989a23a07bb78e2f004d91eac56e946e43` | 0 | 0 | 0 | Traversal passed, warning count closed |

The warning root cause was not the directory-prefix case alone. The linux-xr
tree contains real ftrace selftest files ending in `.tc`. The mounted VFS was
stripping `.tc` during lookup before trying the exact key, which made FUSE
probe missing keys such as
`index/tools/testing/selftests/ftrace/samples/fail`. The patched run keeps
those files visible as real `.tc` names, for example:

```text
mount/tools/testing/selftests/ftrace/samples/fail.tc
mount/tools/testing/selftests/ftrace/samples/pass.tc
mount/tools/testing/selftests/ftrace/test.d/00basic/basic1.tc
```

## Claim Boundary

This closes the mounted traversal warning/noise follow-up for this prefix. It
does not upgrade the storage packet to production S3 posture because the packet
still used plaintext tailnet HTTP SeaweedFS and still carries the measured
`.idx`, generated-header, and socket accounting follow-ups from the original
storage-posture evidence. A later companion,
`docs/release/evidence/home-canary-linux-xr-storage-posture-lifecycle-20260514T213826Z/`,
reused this prefix and closed the scoped Linux lifecycle row without changing
those production storage posture blockers.
