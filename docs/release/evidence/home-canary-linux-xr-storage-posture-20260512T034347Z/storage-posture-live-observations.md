# TCFS linux-xr Storage Posture Live Observations

This packet is partial storage-posture blocker evidence. It is not a scoped
project-tree parity packet and not a production S3 posture claim.

Run boundary:

- Source: `/Users/jess/git/linux-xr`, inventoried read-only.
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/linux-xr-storage-posture-20260512T034347Z`.
- Remote: `seaweedfs://100.64.48.53:8333/tcfs/home-canary-linux-xr-storage-posture-20260512T034347Z`.
- Binary: release `tcfs 0.12.12`, SHA-256
  `ae106d6d52432723f68e56dec765fa8b8f8c8ae27d8c963c4512ad0eb2e67f67`.
- Fresh-prefix mode: `chunk_exists_check=false`, upload concurrency `8`.
- Honey traversal and Linux lifecycle were intentionally not run.

Observed result:

- The run was stopped after the storage bottleneck was captured. `result.env`
  records `status=130`, `proof=push-failed`, and
  `parity_status=full-project-parity-not-claimed`.
- Source/shadow symlink target manifests matched: 85 source symlinks and 85
  shadow symlinks.
- `push-storage-summary.env` records 4,046 completed upload rows,
  6,685,710,398 uploaded bytes, 91,724 chunks, no dedupe bytes, no error rows,
  and no retry-warning rows.
- The dominant `.pack` completed: 6,216,046,897 bytes, 70,856 chunks,
  `streaming=true`, `chunk_upload_concurrency=8`, and
  `chunk_exists_check=false`.
- The adjacent `.rev` also completed: 45,641,304 bytes and 8,405 chunks.
- The packet then moved into the normal project file walk and was stopped at
  4,046 uploaded rows after 5,277.06 seconds of wall-clock time.

Storage posture blockers:

- Endpoint was plaintext HTTP over the lab Tailscale address; production storage
  requires TLS enforcement or an accepted private transport story.
- Credentials were loaded from AWS environment variables. The packet records
  credential presence only, not secret values.
- The upload path had multi-minute periods with no progress row and no retry row
  while TCP connections stayed established.
- The release binary used for this packet did not include
  `TCFS_UPLOAD_CHUNK_TIMEOUT_SECS`, so slow or wedged chunk writes could occupy
  concurrency slots indefinitely.
- The wrapper scripts were edited during the long run; after the interrupted
  `tcfs` child exited, the already-running bash scripts reported parse errors
  from the changed on-disk files. Treat those tail errors as packet hygiene
  noise, not as TCFS upload-engine errors.

Follow-up:

- PR #364 adds bounded per-attempt chunk upload timeouts, timeout retry logging,
  and storage-posture summary fields so the next fresh-prefix run can distinguish
  slow storage from a wedged write.
- A future packet must rerun from a fresh prefix with the timeout-enabled binary
  before claiming production storage posture or a complete release-binary
  `linux-xr` storage proof.
