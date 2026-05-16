# linux-xr-fast Tuned Nix Package Canary Blocker Notes

Created: 2026-05-16T01:31:00Z

This packet is a second blocker packet, not parity evidence.

It reused the shadow from
`git-repo-canary-linux-xr-fast-nixpkg-20260516T005236Z/` and moved to a fresh
remote prefix with the storage-posture knobs that previously worked for large
raw-Git pushes:

- `TCFS_UPLOAD_FILE_CONCURRENCY=8`
- `TCFS_UPLOAD_CHUNK_CONCURRENCY=8`
- `TCFS_UPLOAD_PROGRESS_EVERY_CHUNKS=128`
- `TCFS_UPLOAD_CHUNK_TIMEOUT_SECS=300`
- `TCFS_UPLOAD_PROGRESS_HEARTBEAT_SECS=60`
- `TCFS_STORAGE_MAX_CONCURRENT_OPS=8`
- bounded S3 connect/idle pool settings

The tuned package-backed push still became dominated by the same 387 MB Git
pack index:

```text
.git/objects/pack/pack-0eef814cc0cff33f75a42f5de61a3fedefea1cbc.idx
size=387536244
```

The TCP socket kept making slow forward progress, but `push.log` did not emit
chunk progress/heartbeat rows before the run was intentionally interrupted.
That preserves two useful facts:

1. The larger clean stress canary is not green on the current package.
2. `.git/objects/pack/*.idx` still needs a large-object chunk profile before a
   raw `.git`-heavy canary is a reasonable proof lane.

The follow-up source fix in `crates/tcfs-chunks/src/fastcdc.rs` moves only Git
pack indexes under `.git/objects/pack/` to the large sequential profile while
leaving generic `.idx` files on the existing index profile. Focused
`tcfs-chunks` tests passed after that change, but this evidence packet itself
was not rerun green.

Do not use this packet to claim:

- scoped project-tree parity
- live repo move safety
- packaged Nix/Homebrew restore readiness
- production Finder/FileProvider readiness
- broad `~/git`, `~/Documents`, dotfile, `.local`, or home-directory takeover

