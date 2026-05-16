# linux-xr-fast Source Fix Blocker Notes

This packet is source-built evidence for the first raw-Git metadata fix. It is
not a green canary.

## What It Proved

- Source repo: `/Users/jess/git/linux-xr-fast`
- Source branch/head: `xr/main`
  `dbfcd3938a2f38cd1020716e98aad245452f51e1`
- Source and shadow were clean, with 2,038 regular files, 0 symlinks, and 0
  unsupported special files.
- The live source repo was not mutated; the run reused the isolated shadow
  under `~/TCFS Pilot/real-canaries/`.
- Source-built `target/debug/tcfs` reduced large Git pack indexes under
  `.git/objects/pack/` to the large sequential chunk profile:
  - `pack-0eef814cc0cff33f75a42f5de61a3fedefea1cbc.idx`: 387,536,244 bytes,
    75 chunks
  - `pack-c21744675e9a7f27dbe22914f954ed37b6e2f336.idx`: 259,636,668 bytes,
    51 chunks

## Blocker Exposed

The same run exposed extensionless Git temp-pack files as the next raw-Git
object-count hotspot:

- `tmp_pack_DGh0Fb`: 284,962,654 bytes, 52,372 chunks
- `tmp_pack_g1tYHE`: 93,069,311 bytes, 17,057 chunks
- `tmp_pack_JYlurk`: 34,774,710 bytes, 6,395 chunks

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
