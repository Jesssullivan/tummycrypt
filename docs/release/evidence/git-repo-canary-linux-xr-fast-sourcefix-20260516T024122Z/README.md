# TCFS Git Repo Canary linux-xr-fast Source-Fix Evidence

Created: 2026-05-16T02:41:24Z

This bundle inventories the live source read-only, copies it to an isolated
shadow, and roots TCFS state/config at that shadow. It does not mutate the live
`/Users/jess/git/linux-xr-fast` tree and does not claim `~/Documents`, `~/.local`,
dotfiles, or broad `~/git` takeover.

- Source: `/Users/jess/git/linux-xr-fast`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/linux-xr-fast-nixpkg-shadow-20260516T005236Z`
- Remote: `seaweedfs://100.64.48.53:8333/tcfs/git-repo-canary-linux-xr-fast-sourcefix-20260516T024122Z`
- Config: `/Users/jess/git/tummycrypt/docs/release/evidence/git-repo-canary-linux-xr-fast-sourcefix-20260516T024122Z/state/tcfs-linux-xr-shadow.toml`
- State JSON: `/Users/jess/git/tummycrypt/docs/release/evidence/git-repo-canary-linux-xr-fast-sourcefix-20260516T024122Z/state/push-state.json`

Truth gate: scoped project-tree parity is claimable only when
`parity-gates.env` reports `scoped-project-tree-parity-evidence-complete`.
Symlink-enabled packets must prove source symlinks rehydrate as symlinks with
matching targets, and `push.log` must not contain skipped symlink rows. See
`source-inventory/symlink-targets.tsv`,
`source-inventory/unsupported-special-files.txt`, and
`parity-gates.env`.

Status: blocker evidence only. The source-built pack-index fix reduced the two
largest Git pack indexes to 75 and 51 chunks, then exposed extensionless
`.git/objects/pack/tmp_pack_*` files as the next object-count hotspot. See
`blocker-notes.md`.

Contents:

- `source-inventory/`: branch, remotes, dirty status, counts, hidden dirs,
  symlinks with targets, unsupported special files, and bounded tree listing
- `shadow-inventory/`: same inventory after the isolated copy
- `symlink-shadow-compare.log`: local source/shadow symlink target comparison
- `tcfs-linux-xr-shadow.toml` under `state/`: disposable config with
  `sync_git_dirs = true`, `sync_hidden_dirs = true`,
  `git_sync_mode = "raw"`, `sync_symlinks = true`, and
  `sync_empty_dirs = true`
- `push.log` or `push.log.gz`: shadow push transcript when `--push` ran
- `blocker-notes.md`: raw-Git metadata object-count findings and claim
  boundaries
- `push-run-metadata.env`: push-time run metadata, preserved before later
  resume/honey/lifecycle passes update `run-metadata.env`
- `push-storage-summary.env` and `push-storage-summary.md`: storage-facing
  totals extracted from `push.log` when push evidence is present
- `honey-linux-xr-shadow-commands.txt`: honey mounted traversal/hydration
  commands for the selected file, `.git` traversal, and mounted symlink
  target verification
- `linux-lifecycle-companion.log` and `linux-lifecycle/`: optional mounted
  write/readback, cache clear/rehydrate, dirty safe-unsync refusal, clean
  recursive unsync, and exact rehydrate companion evidence
