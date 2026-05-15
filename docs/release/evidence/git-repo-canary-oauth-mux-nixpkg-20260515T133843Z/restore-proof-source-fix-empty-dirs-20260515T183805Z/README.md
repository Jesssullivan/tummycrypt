# TCFS Git Repo Fresh-Tree Restore Proof

Created: 2026-05-15T18:46:35Z

This proof restores an already-pushed git-repo canary prefix into a fresh local
tree with `tcfs reconcile --execute`, then compares restored regular-file
SHA-256 hashes and symlink targets against the archived shadow tree.

- Packet: `/Users/jess/git/tummycrypt/docs/release/evidence/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/oauth-mux-nixpkg-shadow-20260515T133844Z`
- Restore root: `/Users/jess/TCFS Pilot/restore-proofs/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z-restore-20260515T183806Z`
- Config: `/Users/jess/git/tummycrypt/docs/release/evidence/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/state/tcfs-git-repo-canary.toml`
- Remote prefix: `git-repo-canary-oauth-mux-nixpkg-20260515T133843Z`
- tcfs: `target/debug/tcfs`
- Status: `passed`
- Proof: `fresh-tree-restore-files-symlinks-empty-dirs`
- Reason: `regular files, symlink targets, and empty directories restored exactly`

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
