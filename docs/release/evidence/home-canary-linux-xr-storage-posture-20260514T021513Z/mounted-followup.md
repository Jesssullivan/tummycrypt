# TCFS linux-xr Mounted Follow-up

This follow-up reuses the completed release-binary storage prefix from
`home-canary-linux-xr-storage-posture-20260514T021513Z` and proves mounted
large-tree behavior on `honey` without pushing the 7.7 GB shadow again.

- `honey-linux-xr-shadow.log`: `tcfs 0.12.12` from the pinned Nix store path,
  SHA-256 `1cde5b3563487999c94780c5cf0d7834487ef8acb27fe96a77868669aa08c361`,
  mounted `find -maxdepth 8`, all 85 mounted symlink targets verified, and
  exact `.clang-format` hydration.
- `honey-mount.log`: mount transcript for the same remote prefix.
- `honey-mounted-smoke.log`: first failed attempt, retained as a stale-client
  and harness bug record. It selected honey's ambient `tcfs 0.12.2` and tried
  to hash the unresolved `tcfs` token.

Claim boundary: this closes the storage packet's mounted traversal and symlink
verification gap. It still does not claim production S3 posture because the
endpoint is plaintext tailnet HTTP, socket accounting remains open, `.idx` and
generated large headers remain object-count follow-ups, and the Linux lifecycle
companion was not run in this original follow-up.

Operational note: `honey-mount.log` contains 274 S3 `NoSuchKey` warnings while
the mounted traversal probes index paths. The smoke still completed with
`honey_status=0`. Follow-up VFS work now treats list-returned prefix entries as
directories instead of speculative readable file index objects and records
short-lived directory hints for the immediate lookup path. This packet should be
rerun on honey before claiming the production browse-before-hydrate warning
count is closed.

Superseding notes: `home-canary-linux-xr-storage-posture-tc-extfix-20260514T202343Z/`
reran the mounted smoke with the exact `.tc` filename fix and dropped the
warning count to zero. `home-canary-linux-xr-storage-posture-lifecycle-20260514T213826Z/`
then reused the same prefix and closed the scoped lifecycle row.
