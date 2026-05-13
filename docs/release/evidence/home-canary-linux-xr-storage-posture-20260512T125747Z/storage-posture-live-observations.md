# Storage Posture Live Observations

Run: `home-canary-linux-xr-storage-posture-20260512T125747Z`

This packet is partial storage-posture evidence, not a parity or production S3
posture pass. It used the release `tcfs 0.12.12` binary against a disposable
SeaweedFS prefix and preserved the source/shadow inventories, redacted endpoint
and credential-presence metadata, and a long partial `push.log`.

Useful truth from the packet:

- The live source `/Users/jess/git/linux-xr` was inventoried read-only and copied
  into an isolated shadow.
- Source and shadow symlink target manifests matched for all 85 symlinks.
- The endpoint was the tailnet SeaweedFS S3 endpoint over plaintext HTTP:
  `http://100.64.48.53:8333`.
- AWS credential presence was recorded, but secret values are not included.
- The push remained incomplete and `result.env` stayed at
  `proof=pending-home-canary-shadow`.

This packet should be retained as partial pre-file-concurrency context only. It
does not prove honey traversal, mounted hydration, mounted symlink verification,
recursive unsync, or production storage posture.
