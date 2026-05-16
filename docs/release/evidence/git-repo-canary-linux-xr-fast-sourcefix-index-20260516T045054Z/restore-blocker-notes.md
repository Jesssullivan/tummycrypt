# linux-xr-fast Fresh-Tree Restore Blocker

Created: 2026-05-16T06:12:41Z

This packet is green for the isolated shadow push, honey mounted traversal and
hydration, mounted symlink verification, and Linux lifecycle companion. It is
not green for fresh-tree restore.

`restore-proof/restore-proof.env` records:

- status: `failed`
- reason: `regular file hash manifest mismatch`
- regular files: `2036` restored out of `2038`
- symlinks: `0` source, `0` restored, targets matched
- empty directories: `6` source, `6` restored, matched with
  `REQUIRE_EMPTY_DIRS=1`
- unsupported special files: `0` source, `0` restored
- state entries: `2036`

The two missing regular files are both large raw Git pack files:

- `.git/objects/pack/pack-0eef814cc0cff33f75a42f5de61a3fedefea1cbc.pack`
  (`4,956,598,234` bytes, `950` chunks in the push transcript)
- `.git/objects/pack/pack-c21744675e9a7f27dbe22914f954ed37b6e2f336.pack`
  (`2,572,805,952` bytes, `502` chunks in the push transcript)

`restore-proof/reconcile-execute.log` shows repeated transient S3/OpenDAL chunk
read failures for these large pulls, then finishes with:

```text
Done: 0 pushed, 2036 pulled, 6 dirs-created, 0 deleted, 0 conflicts, 2 errors
```

The log does not record an `ENOSPC` failure. The host did have tight free disk
space during the run, so a repeat proof should start with enough free space for
the full 8.7 GB restore plus temporary overhead before using this packet to
make any live-repo move claim.

Claim boundary: this packet remains scoped project-tree traversal/hydration and
Linux lifecycle evidence only. It does not prove live repo move safety,
production Finder/FileProvider readiness, Homebrew readiness, package-backed
restore/rollback, broad `~/git`, `~/Documents`, dotfiles, `.local`, or
home-directory takeover.
