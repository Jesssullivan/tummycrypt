# TCFS linux-xr S3 Storage Posture Packet

This packet is a storage-facing canary for the isolated `linux-xr` shadow.
It is separate from the scoped project-tree correctness claim and is not, by
itself, a production S3 posture claim.

Required claim boundary:

- use a release or packaged `tcfs` binary, not an unlabelled debug build
- use a fresh disposable remote prefix
- preserve `chunk_exists_check=false` when fresh-prefix mode is enabled
- preserve chunk progress rows, concurrency, retry/warning counts, object
  counts, chunk timeout posture, endpoint posture, S3 HTTP client limits,
  heartbeat rows, and push wall-clock/memory evidence where available
- record the object-model decision: raw Git `.pack` and `.rev` files use the large
  sequential FastCDC profile (1MiB minimum, 4MiB average, 16MiB maximum)
  while `.idx` files stay on the moderate pack-index profile
- keep production Finder, broad home-directory takeover, and on-prem cutover out
  of this packet

The underlying inventory/shadow/push mechanics are delegated to
`scripts/home-canary-linux-xr-shadow.sh`; this wrapper records the
storage-posture defaults in `storage-posture.env`.

Mounted follow-up: `mounted-followup.env` and `honey-linux-xr-shadow.log`
reuse this completed prefix and prove honey mounted traversal, selected
hydration, and all 85 symlink target checks. The Linux lifecycle companion,
production TLS endpoint posture, and storage socket/object-count closure remain
outside this packet.
