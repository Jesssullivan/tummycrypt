# Neo-Honey Device 2 Exact-Byte Proof

Date: 2026-05-31

This packet records a throwaway neo-to-honey TCFS round-trip proof after honey
was enrolled as the second real device and switched to the repo-managed
Home Manager build carrying the registry-sync CLI.

## Result

- Status: complete
- Source: `neo.local`
- Target: `honey`
- Payload bytes: 76
- Payload SHA-256: `9a510bd212f7d8ea515780e1262c79a04e9077ef135eb84db2f62a1968435e74`
- Pulled SHA-256 on honey: `9a510bd212f7d8ea515780e1262c79a04e9077ef135eb84db2f62a1968435e74`
- Manifest:
  `data/proofs/neo-honey-20260531T155450Z/manifests/4930ea4f7b561c7ec3a551fdb008563fa7f872c32f1a1cc13fae1a0a8be5e694`

## Commands

Neo generated a temporary payload and pushed it:

```sh
tcfs push --prefix data/proofs/neo-honey-20260531T155450Z /tmp/tcfs-neo-honey-proof.psG1y2/payload.txt
```

Honey pulled the manifest and verified the bytes:

```sh
tcfs pull data/proofs/neo-honey-20260531T155450Z/manifests/4930ea4f7b561c7ec3a551fdb008563fa7f872c32f1a1cc13fae1a0a8be5e694 /tmp/tcfs-neo-honey-pull.nCb8YC/payload.txt
sha256sum /tmp/tcfs-neo-honey-pull.nCb8YC/payload.txt
wc -c /tmp/tcfs-neo-honey-pull.nCb8YC/payload.txt
```

## Caveats

- This proves exact bytes through the storage-backed manifest/chunk path.
- It does not prove a mounted-file edit/hydrate workflow for an enrolled
  agent-state directory.
- The lab storage and NATS paths still use plaintext transport and remain a
  production hardening item before broad daily-driver enrollment.
