# TCFS linux-xr S3 Storage Posture Packet

This packet is a storage-facing canary for the isolated `linux-xr` shadow.
It is separate from the scoped project-tree correctness claim and is not, by
itself, a production S3 posture claim.

Required claim boundary:

- use a release or packaged `tcfs` binary, not an unlabelled debug build
- use a fresh disposable remote prefix
- preserve `chunk_exists_check=false` when fresh-prefix mode is enabled
- preserve chunk progress rows, concurrency, retry/warning counts, object
  counts, endpoint posture, and push wall-clock/memory evidence where available
- keep production Finder, broad home-directory takeover, and on-prem cutover out
  of this packet

The underlying inventory/shadow/push mechanics are delegated to
`scripts/home-canary-linux-xr-shadow.sh`; this wrapper records the
storage-posture defaults in `storage-posture.env`.
