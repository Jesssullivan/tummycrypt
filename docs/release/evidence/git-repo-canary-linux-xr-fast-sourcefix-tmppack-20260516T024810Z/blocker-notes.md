# linux-xr-fast Temp-Pack Source Fix Blocker Notes

This packet is source-built evidence for the second raw-Git metadata fix. It is
not a green canary.

## What It Proved

- Source repo: `/Users/jess/git/linux-xr-fast`
- Source branch/head: `xr/main`
  `dbfcd3938a2f38cd1020716e98aad245452f51e1`
- Source and shadow were clean, with 2,038 regular files, 0 symlinks, and 0
  unsupported special files.
- The live source repo was not mutated; the run reused the isolated shadow
  under `~/TCFS Pilot/real-canaries/`.
- Source-built `target/debug/tcfs` kept the earlier pack-index improvement:
  - `pack-0eef814cc0cff33f75a42f5de61a3fedefea1cbc.idx`: 75 chunks
  - `pack-c21744675e9a7f27dbe22914f954ed37b6e2f336.idx`: 51 chunks
- The same binary reduced extensionless Git temp packs to the large sequential
  profile:
  - `tmp_pack_DGh0Fb`: 284,962,654 bytes, 51 chunks
  - `tmp_pack_g1tYHE`: 93,069,311 bytes, 18 chunks
  - `tmp_pack_JYlurk`: 34,774,710 bytes, 8 chunks

## Blocker Exposed

The run was intentionally stopped with `status=143` before honey/lifecycle
proof. At stop time, `.git/index` was the max-chunk path:

- `.git/index`: 10,181,944 bytes, 1,767 chunks, 1,343,299 ms upload elapsed

Current source now maps the exact `.git/index` file to the pack chunk profile,
but that fix has not yet been proven by a large `linux-xr-fast` canary rerun.

## Claims Not Made

- no completed shadow push
- no honey traversal or hydration
- no Linux lifecycle companion
- no fresh-tree restore or rollback proof
- no live repo move safety
- no broad `~/git`, `~/Documents`, dotfile, `.local`, or home takeover
- no Homebrew readiness
- no production Finder/FileProvider readiness
- no production S3 posture
