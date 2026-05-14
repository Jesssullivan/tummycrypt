# TCFS linux-xr Storage Posture Live Observations

This packet was run from `main` `c0c2c0c` with a rebuilt release
`target/release/tcfs` binary:

- `tcfs_version=tcfs 0.12.12`
- `tcfs_sha256=0cacfac3ab32adecf471a4b8ebea4450aa9763033d8c9ef1dad52e4098e86856`
- `remote=seaweedfs://100.64.48.53:8333/tcfs/home-canary-linux-xr-storage-posture-20260514T021513Z`
- `transport_tls=false`
- `file_upload_concurrency=8`
- `chunk_upload_concurrency=8`
- `chunk_write_timeout_secs=300`
- `fresh_prefix_publish=true`
- `chunk_exists_check=false`

## Result

The release-binary push completed:

- `push-storage-summary.env`: `upload_rows=92969`, `total_file_bytes=8233794656`,
  `total_chunks=327482`, `error_rows=0`, `retry_warning_rows=0`

The follow-up mounted honey smoke also passed against the same remote prefix:

- `mounted-followup.env`: `honey_status=0`,
  `proof=shadow-push-honey-traversal-symlink-targets`
- `honey-linux-xr-shadow.log`: pinned honey
  `/nix/store/h0b39zzhmk54n0ixbl8jq66pk55sbdhr-tcfs-cli-0.12.12/bin/tcfs`,
  `tcfs 0.12.12`, SHA-256
  `1cde5b3563487999c94780c5cf0d7834487ef8acb27fe96a77868669aa08c361`,
  mounted `find -maxdepth 8`, 85 mounted symlink target checks, and exact
  `.clang-format` hydration.
- `result.env`: `status=0`,
  `proof=shadow-push-honey-traversal-symlink-targets`,
  `parity_status=full-project-parity-not-claimed`,
  `parity_reason=Linux lifecycle companion was not run`.

This is storage-shape plus mounted traversal progress, not production Finder,
broad home-directory, or production S3 posture evidence.

## Object Model

The raw Git `.pack` and `.rev` large sequential profile fixes both worked:

- Earlier blocker packet: the 6,216,046,897-byte `.pack` reached 70,856 chunks.
- `20260513T220442Z`: the same dominant `.pack` completed as 1,211 chunks.
- Earlier packet: the adjacent 45,641,304-byte `.rev` produced 8,405 chunks.
- This packet: the same `.rev` completed as 8 chunks.

The moderate `.idx` profile remains the largest Git-pack-family object count:

- `.idx` rows: 4,599 chunks for the dominant 395,849,892-byte index.

The next object-count hotspot is not raw Git pack metadata. Large generated
source headers in the Linux tree still use the default small-file profile:

- `drivers/gpu/drm/amd/include/asic_reg/dcn/dcn_3_2_0_sh_mask.h` was
  23,949,786 bytes and produced 2,986 chunks.
- `drivers/gpu/drm/amd/include/asic_reg/nbio/nbio_7_2_0_sh_mask.h` was
  16,414,003 bytes and produced 2,121 chunks.

That is acceptable for this proof packet because the run completed without
retry or error rows, but it keeps production storage posture open until TCFS has
an intentional policy for generated large source/data files.

## Remaining Blockers

- At original packet time, the Linux lifecycle companion was not run and the
  older project-tree correctness packet remained the lifecycle reference.
- Superseding note: the later companion
  `docs/release/evidence/home-canary-linux-xr-storage-posture-lifecycle-20260514T213826Z/`
  reused this same prefix and closed the scoped lifecycle row, while preserving
  the production storage posture blockers below.
- The endpoint was plaintext tailnet HTTP, so this is not production TLS
  storage posture.
- Socket sampling reached highwater 11 while upload concurrency was 8. The S3
  HTTP client/socket accounting needs another pass before claiming bounded
  storage concurrency.
- The honey mount log contains 274 S3 `NoSuchKey` warnings during traversal
  probe paths. The smoke passed, but the miss path is too noisy for a polished
  production browse-before-hydrate experience. Follow-up VFS work now avoids
  treating list-returned directory prefixes as readable file index entries and
  keeps a short-lived positive directory hint for the lookup that follows
  `readdir`; rerun honey against a fresh build before claiming the archived
  warning count is closed.
- Total chunks are still high at 327,482 because normal project files and
  generated headers remain on the default profile. That is now a measured
  productionization follow-up, not a blocker to the `.pack`/`.rev` fix.
