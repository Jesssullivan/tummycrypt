# tcfs / bulkload boundary (2026-10-15)

Status: operator-ruled positioning (TIN-4543: Q153, Q155, Q156). The
engineering boundary below follows from the evidence as of 2026-10-15.
Live state is in [current.md](current.md#status-2026-10-15).

## Positioning

- **tcfs is the long-run product** (Q155). It is expected to take years. It
  is an encrypted, remote-first filesystem that you mount, hydrate on
  demand and roam between machines.
- **bulkload is a publishable migration tool** (Q155). It moves an estate
  once: a bounded capture, a transfer and a verification. It does not need
  to become a filesystem, and tcfs does not need to become a migration tool.
- **The first real tcfs user is a Mac-only cohort on neo** (Q156). That
  cohort is left out of bulkload's full run. tcfs mounts it and does not
  copy it.
- **tcfs's home is the blahaj SeaweedFS 4.48 deployment** (Q153). The
  preconditions, all required, are: the P9 identity questions answered,
  `filer_backup` on, the deployment verified to sign with a fresh,
  non-published JWT key, and NATS `sync_always` on. Then `s3_hold` flips.
  The old honey pod is not revived (TIN-5136, R-C672).

## Who owns what

| Data class | Owner today | Why |
| --- | --- | --- |
| Working-tree files: ordinary tracked and untracked files under an enrolled root | **tcfs** | This is the core mechanism: FastCDC, encrypt, upload, then hydrate on open. |
| Agent transcripts and session files: bounded, append-mostly subtrees | **tcfs** | A bounded Claude project subtree has been demonstrated roaming between neo and honey. |
| Git history: objects, refs, packs | **tcfs, as a bundle/ref transport (never raw `.git` admin paths); git push/fetch; bulkload for migration** | Bundle mode snapshots each repository into a `.git-tcfs-bundle` and uploads that (`collect_git_bundles` and `collect_one_git_bundle` in `crates/tcfs-sync/src/engine.rs`). tcfs does not roam raw `.git` admin paths for registered roots (see `current.md`, Strategy A item 5: use bundle/ref transport). |
| Linked worktrees: gitfile pointers and `.git/worktrees/` admin areas | **Not tcfs.** bulkload ownership is unverified (see below). | `crates/tcfs-sync/src/blacklist.rs` refuses these by design (`GitFilePointer`, `GitWorktreesAdmin`), because roaming them would destroy the origin host's live worktree. bulkload deleted its v4/v2 capture producer (`14384f40`), so no current bulkload code path for linked worktrees has been confirmed. |
| SQLite and other live databases | **Not tcfs.** bulkload ownership is unverified. | `blacklist.rs` refuses `.sqlite`, `.sqlite3`, `.db` and related files, because copying a live database file risks corrupting it. |

tcfs takes over the last two rows only after it has built the matching
features:

1. **Worktree reconstruction.** Rebuild linked worktrees from a logical
   topology on the destination host. Copying admin files is not
   reconstruction.
2. **SQLite snapshots.** Take a consistent copy with the online backup API or
   after a WAL checkpoint, and restore it as a whole file. Syncing the live
   file chunk by chunk is not acceptable.

Until both exist, the blacklist entries stay as they are.

## Reuse bulkload's walker in tcfs

The 2026-10-15 estate check found that tcfs's scan and upload path costs
more than it should, and drops data at the edges:

- **The estate check measured 3 to 5 reads of each file** across the scan,
  hash, chunk and freshness steps. This count was not re-verified against
  `crates/tcfs-sync/src/engine.rs` for this document.
- **It skips non-UTF-8 file names.** The main walker (`collect_files_inner`,
  around `engine.rs` line 4872) filters names through `OsStr::to_str()`, so a
  file whose name is not valid UTF-8 is never synced. Git repository and
  bundle discovery (`find_git_repos`, `find_git_bundles`) skip non-UTF-8
  entries the same way.
- **One unreadable directory (`EACCES` on `read_dir`) aborts the whole
  root.** `collect_files_inner` propagates `read_dir` and `file_type()`
  errors with `?`. Per-file upload failures are different: `push_tree` logs
  "upload failed" and continues. The directory case should be recorded and
  skipped.

bulkload's walker already solves these problems. tcfs should adopt its
single-read discipline. Take a stat identity, read the file once, feeding
the hash, chunker and encryptor from that one read. Then stat again and
reject the file if its identity changed. tcfs already has a `StatIdentity`
type in `crates/tcfs-sync/src/freshness.rs`, which it uses for freshness.
The missing part is single-read ingest, not the type.

Large-file hydration is a separate gap. It assembles the whole file in RAM,
one chunk after another (TIN-1419, streaming large-file IO).

## No cross-product chunk dedup

When encryption is enabled (the `crypto` feature on and a file key present),
tcfs stores each chunk under the BLAKE3 hash of its ciphertext (the
`chunk_key` built in `engine.rs`, around lines 2560–2579). Without them the
key is the plaintext chunk hash. The ciphertext depends on a per-file key and
a random nonce. For encrypted roots, plaintext that bulkload and tcfs both
hold therefore never produces the same chunk key, so the two products cannot
share a chunk store. Do not design for cross-product dedup unless tcfs
changes its key scheme, which would be a separate security decision.

## Retire the bulkload v4/v2 capture dependency

`crates/tcfs-sync/src/git_workspace.rs` is a strict adapter for bulkload's
`dev.tinyland.bulkload.agent-capture.v4` envelope and its embedded
`dev.tinyland.bulkload.git-workspace.v2` workspaces. Git-workspace roots
need a complete capture in that format (`current.md`, Strategy A item 5).
The code that produced that capture was deleted from bulkload (commit
`14384f40`). No new captures can be made, so the dependency is a dead end.

Choose one of two ways to retire it:

- Replace the adapter with tcfs's own worktree reconstruction (see "Who owns
  what" above).
- Freeze it as a read-only importer for captures that already exist, and say
  in `current.md` that no new captures will be produced.

Until one of these lands, do not plan any work that needs a new GitWorkspaceV2
capture.

## Compression

Compression is not in the tcfs data path today. `tcfs-chunks` has a
`seekable_zstd` module, but nothing calls it (see
[ARCHITECTURE.md](../ARCHITECTURE.md)). Compression could be added later,
before the encrypt step, but it is a product decision. It is not a boundary
question.

## References

- Linear: TIN-4543 (rulings Q153, Q155, Q156), TIN-5136 (pause, R-C672),
  TIN-1419 (streaming large-file IO).
- Pause record: [tcfs-honey-cluster-pause-2026-09-29.md](tcfs-honey-cluster-pause-2026-09-29.md).
- PR #608 made the Ansible SeaweedFS `security.toml.j2` templates
  (`infra/ansible/roles/` and the legacy `roles/` copy) fail closed:
  `seaweedfs_jwt_signing_key` and `seaweedfs_jwt_filer_signing_key` are now
  mandatory, replacing a weak literal that had been in the public templates
  since 2026-02-21.
