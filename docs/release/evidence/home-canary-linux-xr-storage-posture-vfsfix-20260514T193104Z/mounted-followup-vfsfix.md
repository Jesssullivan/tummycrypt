# Mounted Follow-Up: Directory-Prefix VFS Rerun

This intermediate packet reran honey mounted traversal against
`home-canary-linux-xr-storage-posture-20260514T021513Z` using the
directory-prefix VFS fix from `844db86`.

Result: traversal, `.clang-format` hydration, and mounted symlink target
verification passed, but the S3 `NoSuchKey` warning count remained unchanged:

- Original mounted follow-up: 274 `NoSuchKey` warnings.
- This directory-prefix-only rerun: 274 `NoSuchKey` warnings.

The preserved logs showed the remaining warnings clustered on legitimate Linux
source files ending in `.tc`, such as
`tools/testing/selftests/ftrace/samples/fail.tc`. That root cause is closed by
the later exact-filename packet:

`docs/release/evidence/home-canary-linux-xr-storage-posture-tc-extfix-20260514T202343Z/`.

This packet is retained as negative evidence for the first hypothesis; it is
not the final mounted warning-closure claim.
