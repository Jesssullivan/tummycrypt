# Storage Posture Observations

This packet was started with the repo-local `target/debug/tcfs` built before the
storage fixes from this follow-up worktree. Treat these notes as pre-fix
performance evidence and a repro trace, not as proof that the new storage
changes are present in this run.

## Raw Git Index

The large raw Git index
`.git/objects/pack/pack-cca8376c73bfef8a16038f348b63402f7ed78f01.idx`
was 395,849,892 bytes.

- Upload started: `2026-05-10T20:25:20Z`
- Streaming chunker log: `2026-05-10T20:26:06Z`
- Upload completed: `2026-05-10T20:48:29Z`
- Result: `chunks=72598`, `uploaded_bytes=395849892`, `streaming=true`

This exposed that `.idx` files were using the small-file profile, producing an
average chunk size of roughly 5.3 KiB and excessive S3 object traffic.

## Raw Git Pack

The adjacent raw Git pack
`.git/objects/pack/pack-cca8376c73bfef8a16038f348b63402f7ed78f01.pack`
was 6,216,046,897 bytes.

- File selected for upload: `2026-05-10T20:49:22Z`
- Streaming chunker log after snapshot preparation: `2026-05-10T21:36:56Z`
- Upload completed: `2026-05-10T22:16:04Z`
- Result: `chunks=70856`, `uploaded_bytes=6214403348`,
  `bytes=6216046897`, `streaming=true`
- Process sample during snapshot preparation:
  `push-pack-snapshot-sample-2.txt`
- Sampled physical footprint during snapshot preparation: about 6.1 GiB

This exposed that the pre-fix streaming upload snapshot retained owned chunk
bytes for the whole large file before upload. The follow-up code path changes
the snapshot to keep chunk metadata and verify chunk bytes when uploading.

## Claim Boundary

This packet did not complete the mounted correctness proof: honey symlink
verification failed and the Linux lifecycle companion failed during mounted
`cat`. Treat it as pre-fix S3 posture and blocker evidence only. Re-run a fresh
disposable prefix with rebuilt `tcfs` binaries on both neo and honey before
claiming improved `.idx` chunk counts, bounded large-file snapshot memory, or
full `linux-xr` parity.
