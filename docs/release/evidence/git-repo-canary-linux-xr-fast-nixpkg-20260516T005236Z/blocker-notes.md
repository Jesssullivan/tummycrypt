# linux-xr-fast Nix Package Canary Blocker Notes

Created: 2026-05-16T01:31:00Z

This packet is a blocker packet, not parity evidence.

The source repo was inventoried cleanly:

- Source: `/Users/jess/git/linux-xr-fast`
- Branch/head: `xr/main` at `dbfcd3938a2f38cd1020716e98aad245452f51e1`
- Regular files: 2,038
- Symlinks: 0
- Unsupported special files: 0
- Shadow size: about 8.2 GB, dominated by `.git`

The package-backed push used the current Darwin Nix flake package binary:

- `tcfs 0.12.12`
- SHA-256: `1e7ce7bf2d07102be166188f03d8da923605186a02c70793f78b52cf8c4c4d09`

The push was intentionally interrupted after it reached:

```text
.git/objects/pack/pack-0eef814cc0cff33f75a42f5de61a3fedefea1cbc.idx
size=387536244
```

The process was alive and slowly sending data, but the stress run was no
longer useful as a green proof lane. It was spending the canary budget on a
known raw-Git `.idx` object-count problem instead of proving cross-host
traversal/hydration/lifecycle. No live source repo was mutated.

Do not use this packet to claim:

- scoped project-tree parity
- live repo move safety
- Homebrew readiness
- production Finder/FileProvider readiness
- broad `~/git`, `~/Documents`, dotfile, `.local`, or home-directory takeover

