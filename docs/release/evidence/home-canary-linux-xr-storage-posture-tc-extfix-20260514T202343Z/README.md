# TCFS Home Canary linux-xr Shadow Evidence

Created: 2026-05-14T20:09:40Z

This bundle inventories the live source read-only, copies it to an isolated
shadow, and roots TCFS state/config at that shadow. It does not mutate the live
`/Users/jess/git/linux-xr` tree and does not claim `~/Documents`, `~/.local`,
dotfiles, or broad `~/git` takeover.

- Source: `/Users/jess/git/linux-xr`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/linux-xr-storage-posture-20260514T021513Z`
- Remote: `seaweedfs://100.64.48.53:8333/tcfs/home-canary-linux-xr-storage-posture-20260514T021513Z`
- Config: `/Users/jess/git/tummycrypt/docs/release/evidence/home-canary-linux-xr-storage-posture-20260514T021513Z/state/tcfs-linux-xr-shadow.toml`
- State JSON: `/Users/jess/git/tummycrypt/docs/release/evidence/home-canary-linux-xr-storage-posture-20260514T021513Z/state/push-state.json`

Mounted follow-up outcome:

- This run reused the original pushed prefix and did not rerun the 7.7 GB push.
  The original push transcript remains archived in
  `../home-canary-linux-xr-storage-posture-20260514T021513Z/push.log.gz`.
- Honey ran the patched Linux binary
  `/tmp/tcfs-vfs-tc-ext-20260514T2000Z/tcfs-cli/bin/tcfs` with SHA-256
  `dc9b1758274b9c19d4ed470537486e989a23a07bb78e2f004d91eac56e946e43`.
- Mounted `find -maxdepth 8`, exact `.clang-format` hydration, and all 85
  mounted symlink target checks passed.
- `honey-mount.log` recorded 0 `NoSuchKey`, 0 WARN, and 0 ERROR rows.
- See `mounted-followup-tc-extension-fix.md` for old/new warning counts and
  claim boundaries.

Truth gate: scoped project-tree parity is claimable only when
`parity-gates.env` reports `scoped-project-tree-parity-evidence-complete`.
Symlink-enabled packets must prove source symlinks rehydrate as symlinks with
matching targets. See `source-inventory/symlink-targets.tsv` and
`source-inventory/unsupported-special-files.txt`.

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
- `push-storage-summary.env` and `push-storage-summary.md`: storage-facing
  totals extracted from `push.log` when push evidence is present
- `honey-linux-xr-shadow-commands.txt`: honey mounted traversal/hydration
  commands for the selected file, `.git` traversal, and mounted symlink
  target verification
- `linux-lifecycle-companion.log` and `linux-lifecycle/`: optional mounted
  write/readback, cache clear/rehydrate, dirty safe-unsync refusal, clean
  recursive unsync, and exact rehydrate companion evidence
