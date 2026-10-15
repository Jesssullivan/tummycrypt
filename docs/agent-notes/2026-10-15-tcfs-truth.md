# 2026-10-15 tcfs truth pass

Branch `docs/tcfs-truth-20261015` (off `origin/main` `ee3b92c`). Docs-only.
Rulings: TIN-4543 (Q153, Q155, Q156); TIN-5136 / R-C672 (pause).

## What changed

- `docs/ops/current.md`: added a "Status 2026-10-15" block at the top. It
  states that the backend has been 0/0 since 2026-09-29. It gives both
  client lists: the pause record's (`tcfsd` on honey, bumble, neo plus the
  honey FUSE mount) and the estate check's (neo, yoga, mbp-13, honey, with
  yoga FUSE returning EIO), the latter marked not re-verified. It gives the
  Q153 preconditions (unordered, all required) and then the `s3_hold` flip,
  links the deferred `v0.12.18` ruling, and marks the 2026-08-22 content
  historical.
- `README.md`: the status note now covers the pause, CI state (build and
  tests green; cargo-deny pending #609 and Nix CI red), what works, and the
  limits on whole-home, linked-worktree and SQLite use.
- `docs/ARCHITECTURE.md` and `crates/tcfs-crypto/src/lib.rs`: removed the
  claim that zstd is in the data path. `seekable_zstd` exists but has no
  callers. Crate count corrected from 19 to 18. Chunk-key statement made
  conditional on encryption being enabled.
- New `docs/ops/tcfs-bulkload-boundary.md`: which data each product owns,
  the walker reuse, why there is no cross-product dedup (encrypted roots),
  retiring the v4/v2 capture dependency, and the Q156 cohort (unnamed).

## Review corrections applied (2026-10-15)

- Removed the private cohort names from the public boundary doc.
- JWT: #608 (`516e9241`) says nothing consumed the old literal, so no
  rotation was required by #608 itself. The docs now say Q153 requires
  verifying blahaj signs with a fresh, non-published key, and that #608 made
  `seaweedfs_jwt_signing_key` and `seaweedfs_jwt_filer_signing_key`
  mandatory in both `security.toml.j2` copies.
- EACCES: an unreadable directory aborts the root (`read_dir`/`file_type()`
  `?` in `collect_files_inner`); per-file upload failures are warned and
  skipped in the push loop.
- Non-UTF-8: main walker at `engine.rs` ~4872; `find_git_repos` and
  `find_git_bundles` skip such entries during repo/bundle discovery.
- Git history row: tcfs does carry history as a bundle/ref transport
  (`collect_git_bundles`, `collect_one_git_bundle`).
- Linked-worktree and SQLite rows: bulkload ownership marked unverified,
  because its v4/v2 producer was deleted (`14384f40`) and the local bulkload
  checkout at that commit has no code path for either.

## Verified in source

- zstd: only `tcfs-auth/src/enrollment.rs` calls it. Outside the crate
  itself, `tcfs-chunks::seekable_zstd` has no callers.
- `tcfs-sync/src/blacklist.rs` refuses gitfile pointers,
  `.git/worktrees/`, `.sqlite*` and `.db*` by design.
- `tcfs-sync/src/git_workspace.rs` decodes bulkload
  `agent-capture.v4`/`git-workspace.v2`.
- Chunk keys are the BLAKE3 hash of the ciphertext when the `crypto` feature
  is on and a file key exists; otherwise the plaintext chunk hash
  (`engine.rs` around lines 2560–2579).

## Not verified here (estate-check facts, taken as given)

- The read count (3–5 per file), the yoga/mbp-13 client list and yoga EIO.

## Follow-ups not done

- `docs/tex/architecture.tex` (lines 92 and 142) still describes zstd
  compression and decompression.
- The pause doc's Restore section still reads as a live procedure gated on a
  TIN-5136 ruling. `current.md` now says Q153 rules it out; the pause doc
  itself was left alone because `just tcfs-backend-paused-test` checks its
  restore order.
- Reconcile the client list (bumble vs yoga/mbp-13) on TIN-5136.

## GitHub and Linear actions (2026-10-15, operator-approved plan)

- Closed PRs #576 and #577 (superseded by #580), #573 and #579 (stale docs
  refreshes) and #572 (product fixes already on main; TIN-2538 continued via
  #583/#584/#585). Each has a one-line reason comment.
- Closed issues #327 and #298 (premise dropped by Q153) and #312 (stale since
  2026-05-14) as not planned.
- Kept open: #565 → #567 → #568 (about 45k lines of unique unmerged TIN-2864
  work; TIN-2864 is In Progress) and #574 (honey preflight evidence packets
  not on main). Both need an operator keep-or-retire call.
- #609 was not merged.
- Linear M11 (P-TIN-143) status update: off track, re-baselined on the pause
  and the Q153 path. Proposed dates were posted but not set.
