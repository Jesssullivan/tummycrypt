# Storage Posture Live Observations

Run: `home-canary-linux-xr-storage-posture-20260513T174944Z`

This packet is the post-PR #367 release-binary storage-posture rerun. It used
the merged `tcfs 0.12.12` release binary from `main` commit `9428513`, enabled
fresh-prefix file upload concurrency, and targeted a disposable SeaweedFS prefix:

`seaweedfs://100.64.48.53:8333/tcfs/home-canary-linux-xr-storage-posture-20260513T174942Z`

Verdict: blocker evidence. This is not a scoped project-tree parity pass and
not production S3 posture.

What the packet proves:

- The live `/Users/jess/git/linux-xr` source was inventoried read-only and copied
  into an isolated shadow under `~/TCFS Pilot/real-canaries/`.
- Source and shadow symlink target manifests matched for all 85 symlinks.
- The release binary and config recorded `file_upload_concurrency=8`,
  `chunk_upload_concurrency=8`, `chunk_write_timeout_secs=300`,
  `fresh_prefix_publish=true`, `remote_conflict_check=false`, and
  `chunk_exists_check=false` for fresh-prefix rows.
- Timeout and retry instrumentation is now observable: `push-storage-summary.env`
  records 47 warning rows, 43 retry warning rows, and 31 timeout retry warning
  rows before the run was intentionally terminated.
- The 6.2 GB raw Git pack began streaming and reached only 853 of 70,856 chunks
  after about 10.8 minutes of pack elapsed time. At that rate the run was no
  longer a practical same-session acceptance proof.
- Socket sampling recorded up to 9 established S3 sockets while the configured
  upload concurrency was 8. Treat that as an observation of the current HTTP
  client/process behavior, not a product guarantee.

Why the packet is a blocker:

- The push was intentionally stopped with SIGTERM after the endpoint cycled
  through timeout and transport retry rows and the large pack remained far from
  complete.
- `result.env` records `status=1`,
  `proof=linux-lifecycle-companion-failed`, and
  `parity_reason=shadow push failed`.
- Mounted symlink verification failed because the push was incomplete.
- The endpoint is tailnet HTTP SeaweedFS, not a production TLS endpoint.

Next storage work should treat this as a backend/object-model decision point,
not as a request to keep increasing client concurrency. The likely next product
questions are multipart/native SeaweedFS writes for multi-GB pack objects,
batching or packaging strategy for large repo trees, TLS/endpoint posture, and
whether raw `.git` is still the right default for broad project-tree sync.
