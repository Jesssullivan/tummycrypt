# TCFS Git Repo Fresh-Tree Restore Proof

Created: 2026-05-16T06:12:41Z

This proof restores an already-pushed git-repo canary prefix into a fresh local
tree with `tcfs reconcile --execute`, then compares restored regular-file
SHA-256 hashes and symlink targets against the archived shadow tree.

- Packet: `/Users/jess/git/tummycrypt/docs/release/evidence/git-repo-canary-linux-xr-fast-sourcefix-index-20260516T045054Z`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/linux-xr-fast-nixpkg-shadow-20260516T005236Z`
- Restore root: `/Users/jess/TCFS Pilot/restore-proofs/git-repo-canary-linux-xr-fast-sourcefix-index-20260516T045054Z-restore-20260516T053935Z`
- Config: `/Users/jess/git/tummycrypt/docs/release/evidence/git-repo-canary-linux-xr-fast-sourcefix-index-20260516T045054Z/state/tcfs-git-repo-canary.toml`
- Remote prefix: `git-repo-canary-linux-xr-fast-sourcefix-index-20260516T045054Z`
- tcfs: `/Users/jess/git/tummycrypt/target/debug/tcfs`
- Status: `failed`
- Proof: `fresh-tree-restore-files-symlinks-empty-dirs`
- Reason: `regular file hash manifest mismatch`

Files:

- `restore-proof.env`: machine-readable result
- `reconcile-dry-run.log`: restore plan before mutation
- `reconcile-execute.log`: restore execution transcript
- `shadow-regular-sha256.tsv` / `restored-regular-sha256.tsv`: regular-file
  hash manifests
- `shadow-symlink-targets.tsv` / `restored-symlink-targets.tsv`: symlink
  target manifests
- `restored-state.tsv`: restored sync-state entries when the state JSON and
  `jq` are available
- `shadow-empty-dirs.txt` / `restored-empty-dirs.txt`: recorded empty-dir
  manifests. Empty directories are a known separate gate unless
  `--require-empty-dirs` is used.
