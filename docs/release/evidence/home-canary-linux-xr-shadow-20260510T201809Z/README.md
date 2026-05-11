# TCFS Home Canary linux-xr Shadow Evidence

Created: 2026-05-11T00:51:44Z

This bundle inventories the live source read-only, copies it to an isolated
shadow, and roots TCFS state/config at that shadow. It does not mutate the live
`/Users/jess/git/linux-xr` tree and does not claim `~/Documents`, `~/.local`,
dotfiles, or broad `~/git` takeover.

- Source: `/Users/jess/git/linux-xr`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/linux-xr-shadow-20260510T201809Z`
- Remote: `seaweedfs://100.64.48.53:8333/tcfs/home-canary-linux-xr-shadow-20260510T201807Z`
- Config: `/Users/jess/git/tummycrypt/docs/release/evidence/home-canary-linux-xr-shadow-20260510T201809Z/state/tcfs-linux-xr-shadow.toml`
- State JSON: `/Users/jess/git/tummycrypt/docs/release/evidence/home-canary-linux-xr-shadow-20260510T201809Z/state/push-state.json`

Truth gate: full project parity is not claimed until a fresh host packet proves
source symlinks rehydrate as symlinks with matching targets. See
`parity-gates.env`, `source-inventory/symlink-targets.tsv`, and
`source-inventory/unsupported-special-files.txt`.

Packet outcome: partial/blocking evidence. The isolated shadow preserved all 85
source symlinks locally and `push.log` completed against the disposable prefix
with symlink uploads enabled, but honey mounted symlink verification failed at
`Documentation/Changes` and the Linux lifecycle companion failed during mounted
`cat`. This packet is useful as canary/storage posture evidence, not as full
`linux-xr` parity.

Compatibility note: this packet predates the explicit
`mounted_symlink_verification_status` field. Its
`mounted_symlink_verification=1` value is the failing shell return code, not a
pass bit.

Contents:

- `source-inventory/`: branch, remotes, dirty status, counts, hidden dirs,
  symlinks with targets, unsupported special files, and bounded tree listing
- `shadow-inventory/`: same inventory after the isolated copy
- `symlink-shadow-compare.log`: local source/shadow symlink target comparison
- `tcfs-linux-xr-shadow.toml` under `state/`: disposable config with
  `sync_git_dirs = true`, `sync_hidden_dirs = true`,
  `git_sync_mode = "raw"`, `sync_symlinks = true`, and
  `sync_empty_dirs = true`
- `push.log`: shadow push transcript when `--push` ran
- `honey-linux-xr-shadow-commands.txt`: honey mounted traversal/hydration
  commands for the selected file, `.git` traversal, and mounted symlink
  target verification
- `linux-lifecycle-companion.log` and `linux-lifecycle/`: optional mounted
  write/readback, cache clear/rehydrate, dirty safe-unsync refusal, clean
  recursive unsync, and exact rehydrate companion evidence
- `storage-posture-observations.md`: pre-fix S3/SeaweedFS large-object
  observations from the raw `.git` push
