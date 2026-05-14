# TCFS linux-xr Storage Posture Live Observations

This packet was run from `main` `74ac016` with a rebuilt release
`target/release/tcfs` binary:

- `tcfs_version=tcfs 0.12.12`
- `tcfs_sha256=92a456cb810850f76a6cd2bdd88582ff1b795b8b7b042e6d1e33c5170b1697cc`
- `remote=seaweedfs://100.64.48.53:8333/tcfs/home-canary-linux-xr-storage-posture-20260513T220441Z`
- `transport_tls=false`
- `file_upload_concurrency=8`
- `chunk_upload_concurrency=8`
- `chunk_write_timeout_secs=300`
- `fresh_prefix_publish=true`
- `chunk_exists_check=false`

## Result

The push completed:

- `result.env`: `status=0`, `proof=shadow-push`
- `parity_status=full-project-parity-not-claimed`
- `parity_reason=mounted honey traversal and symlink target verification were not run`
- `push-storage-summary.env`: `upload_rows=92969`, `total_file_bytes=8233794656`,
  `total_chunks=335831`, `error_rows=0`, `retry_warning_rows=0`

This is storage-shape progress, not scoped project-tree parity, production
Finder, broad home-directory, or production S3 posture evidence.

## Object Model

The raw Git `.pack` profile fix worked:

- Previous packet: 6,216,046,897-byte `.pack` reached 70,856 chunks under the
  older profile.
- This packet: the same dominant `.pack` completed as 1,211 chunks.

The moderate `.idx` profile also stayed tractable:

- `.idx` rows: 4,599 chunks for 395,849,892 bytes.

The next leak is Git `.rev` reverse-index files:

- `max_upload_elapsed_path` was `.git/objects/pack/pack-cca8376c73bfef8a16038f348b63402f7ed78f01.rev`.
- The `.rev` file was 45,641,304 bytes and produced 8,405 chunks.
- Current code after this packet routes `.rev` through the same large
  sequential profile as `.pack`; the next storage-posture rerun must prove the
  reduced `.rev` object count.

## Remaining Blockers

- Honey traversal, mounted hydration, symlink target verification, and Linux
  lifecycle were intentionally disabled for this push-only rerun.
- The endpoint was plaintext tailnet HTTP, so this is not production TLS
  storage posture.
- Socket sampling reached highwater 11 while upload concurrency was 8. The S3
  HTTP client/socket accounting needs another pass before claiming bounded
  storage concurrency.
- The live source had 85 symlinks. Source/shadow target manifests matched, but
  this packet did not verify mounted remote symlink targets.
