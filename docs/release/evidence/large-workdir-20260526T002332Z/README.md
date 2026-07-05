# TCFS Home Canary linux-xr Shadow Evidence

Created: 2026-05-26T00:25:03Z

Status: rescued pending evidence. This packet preserves a second
large-workdir shadow run that was not present on `main`; it does not claim
project parity. The run's own gates currently report
`full-project-parity-not-claimed`, and `push.log` records an eventual local
`No space left on device` failure.

This bundle inventories the live source read-only, copies it to an isolated
shadow, and roots TCFS state/config at that shadow. It does not mutate the live
`/Users/jess/git/linux-xr` tree and does not claim `~/Documents`, `~/.local`,
dotfiles, or broad `~/git` takeover.

- Source: `/Users/jess/git/linux-xr-fast`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/linux-xr-fast-shadow-20260526T002332Z`
- Remote: `seaweedfs://localhost:8333/tcfs/large-workdir-20260526T002332Z`
- Config: `/Users/jess/git/tummycrypt/docs/release/evidence/large-workdir-20260526T002332Z/state/tcfs-linux-xr-shadow.toml`
- State JSON: `/Users/jess/git/tummycrypt/docs/release/evidence/large-workdir-20260526T002332Z/state/push-state.json`

Truth gate: scoped project-tree parity is claimable only when
`parity-gates.env` reports `scoped-project-tree-parity-evidence-complete`.
Symlink-enabled packets must prove source symlinks rehydrate as symlinks with
matching targets, and `push.log` must not contain skipped symlink rows. See
`source-inventory/symlink-targets.tsv`,
`source-inventory/unsupported-special-files.txt`, and
`parity-gates.env`.

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
