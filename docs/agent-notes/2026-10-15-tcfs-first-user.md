# 2026-10-15 tcfs first-user code pass

Branch `feat/tcfs-first-user-20261015`, based on `origin/main` `f64eadc` (it
includes #609 and #610). The changes are staged and not committed. Rulings:
TIN-4543 (Q153, Q155, Q156). Built and tested on `sting` (dev-seat role) in
`nix develop`. No deployment, host or cluster change was made.

## What changed

1. **Single-read ingest** (`crates/tcfs-sync/src/ingest.rs`, `engine.rs`).
   This is bulkload's `capture_file`/`StatIdentity` discipline: one read
   between two `fstat`s of the same descriptor, plus a path re-stat. A torn
   read is retried up to 3 times, then refused with a typed `SourceChanged`.
   Every later check is a stat-identity check. A racy capture (mtime or
   ctime within 2 s of the read) keeps the content re-hash. Settled reads:
   in-memory 3 to 1, streaming 4 to 2. `tcfs-chunks` gains
   `chunk_reader_streaming_metadata`, which takes an open reader.
2. **Walker refusals** (`collect_files_inner`). Names that are not UTF-8,
   unreadable directories (including a listing that fails part way) and
   unreadable entry types are now `CollectResult::refusals`, and the walk
   continues. An unreadable root is still an error. A refused directory is
   never published as an empty-dir marker. Reconcile fences refused rel
   paths: no delete or pull is inferred under them, and an authoritative
   Git-workspace capture with any refusal fails closed.
3. **Hydration** (`crates/tcfs-vfs/src/hydrate.rs`, `cache.rs`,
   `driver.rs`). The fetch now has a bounded, parallel, ordered window
   (default 8 chunks), with incremental size and BLAKE3 checks.
   - `hydrate_cached_file_from_manifest_bytes` streams into a uniquely named
     cache temp file and returns the descriptor. A cache hit is re-verified
     by streaming.
   - `fetch_range_from_manifest_bytes` fetches only the covering chunks.
     Its layout comes from chunk `stat`s.
   - New `VirtualFilesystem::open_streaming` (default falls back to `open`).
     `TcfsVfs` serves range reads from the cache descriptor and loads content
     into memory only on write or truncate. FUSE open and NFS READ use it.
   - `DiskCache::put` uses unique temp names, which fixes a race between
     concurrent puts.
4. **Thrash**:
   - `tcfsd` periodic reconcile backs off (interval doubling to 1 h) while
     the backend fails (`crates/tcfsd/src/backoff.rs`).
   - New `tcfs health` exits 0 `UP`, 2 `DAEMON_DOWN` or 3 `BACKEND_DOWN`.
   - `scripts/tcfsd-health-check.sh` is a reference supervisor check: it
     never restarts on `BACKEND_DOWN`, backs off restarts exponentially,
     and treats an old CLI as UNKNOWN. Its harness is
     `scripts/test-tcfsd-health-check.sh`, wired into `lazy:check`.
5. **Docs**: `docs/ops/first-user-mac-cohort.md` is new and is linked from
   `current.md` and `tcfs-bulkload-boundary.md`.

## Failing-first evidence

- Items 1 and 2: the tests were run against the unchanged code. The
  read-count tests read 3x and 4x the file size (294912 vs 98304, and
  655360 vs 327680). The retry test saw 0 hook calls. The typed-refusal
  tests failed. EACCES aborted collection and reconcile.
- Item 3: the backoff and health tests panicked on `todo!()` stubs. The
  shell harness failed because the script did not exist yet.
- Item 2 (hydration): the tests target new APIs, so the failing-first
  evidence is a compile failure only. No behavioral test was run first.

## Validation

- `cargo fmt --check`: clean.
- `cargo clippy --workspace --all-targets`: one warning, which predates
  this branch (a `tcfs-core` test). `cargo clippy --workspace -- -D warnings`:
  clean.
- `cargo test`: tcfs-chunks 33, tcfs-sync 626 (658 with
  `--features nats,crypto`), tcfs-vfs 115, tcfs-nfs 5, tcfs-cli 164,
  tcfsd 182. All pass.
- `cargo deny check`: ok.
- `lychee --offline` on the changed docs: ok.

## Not done / follow-ups

- The lab `tcfsd-health` script still needs to adopt the reference script.
  That is a lab change; this repo was not edited for it.
- NFS READ still opens a handle per RPC. Each open now re-verifies the
  cached file, so a large sequential read costs O(size) per RPC. Reusing
  handles with a TTL is next.
- The ranged primitive is not yet wired into a surface. VFS writes are still
  whole-file in memory.

## Review pass (same day)

Fifteen reviewer findings were checked against the code. Ruling: TIN-4543
(Q153, Q155, Q156, Q174). Cites R-HOOK-CONVERGENCE-20261004 (R-N13). Still
staged and not committed. Nothing was posted, deployed or changed on a host.

Code fixes. Each has a test that fails on the code as it was staged before
this pass:

- **Cache temp files** (`cache.rs`, `hydrate.rs`, `driver.rs`). A dropped
  hydration used to leave its `.tmp` file behind. `CacheTmp` now removes the
  file on drop unless it was committed. `TcfsVfs::new` sweeps `.tmp` files
  older than 1 h. Streaming opens now share one detached hydration per
  manifest and key (single-flight), so a 10 s protocol timeout stops the
  wait without aborting the download.
- **gRPC push and pull.** These two RPCs held the operator mutex for the
  whole transfer, which blocked `status` and therefore `tcfs health`. They
  now clone the operator inside a block. `tcfs health` has a new verdict,
  `DAEMON_BUSY` (exit 4): the daemon accepted the connection but did not
  answer in time. The health check does not restart on it.
- **Upload state** (`engine.rs`). The SyncState size and mtime, and the
  manifest's mode and mtime, now come from the capture `fstat`. If the
  path's identity has moved since capture, the state records mtime 0, which
  makes the next `needs_sync` re-hash the file.
- **In-memory capture.** The capture reads at most the fstat size + 1 bytes.
  Each capture attempt chooses streaming or in-memory from its own fstat.
- **Write and truncate on a cached handle.** The content is loaded in
  `spawn_blocking` without holding the handle lock. A handle larger than
  the in-memory limit gets `EFBIG` instead of being loaded. The new
  `with_max_write_bytes` setting supports the test.
- **Ranged hydration.** Legacy v1 manifests are refused. The doc comment now
  says the function must stay unwired until the manifest binds each
  chunk's length.
- **Reconcile backoff.** It now backs off only when a storage health probe
  also fails. While backing off it probes once per tick and resumes as soon
  as storage answers.
- **`scripts/tcfsd-health-check.sh`.** It restarts only on an explicit
  `DAEMON_DOWN` (or a stale mount while the daemon reports UP). Exits 1,
  101, 124, an exit 4 without the busy verdict, and an old CLI all return
  UNKNOWN or BUSY (exit 4). Probe deadlines are now pure bash, because stock
  macOS has neither `timeout` nor `gtimeout`. The harness covers exits 1,
  101 and 4, a hung CLI, and a hung `ls`, with neither timeout tool on PATH.
- **Tests.**
  - New: a same-size in-place rewrite with the mtime restored, at both
    capture time and publish time.
  - Root skips are now printed visibly.
  - A storage-boundary layer counts chunk reads. It checks that the chunks
    fetched ahead of the writer stay within the window.

Doc fixes:

- `first-user-mac-cohort.md`:
  - FileProvider is read-only since `5bc2175`.
  - There is no writable macOS surface.
  - Step 8 now has two preconditions: a second copy of the backend data and
    the cohort becoming read-only.
  - The `v0.12.12` Fleet row is marked historical.
  - Health-restart stays disabled until the lab check adopts the reference
    logic.
- Private lane drafts (outside this repo):
  - The P9 ask-5 claim no longer cites a `v0.12.16` tag, which does not
    exist. It is now grepped by resolved commit for every `v0.12.*` tag and
    the untagged 15/16 release cuts.
  - Backend plan P-G re-enables only reconcile. Health-restart now waits
    for a release that ships `tcfs health` and for lab adoption of the
    reference check.

Follow-ups that were not done:

- `docs/platform-support.md` still cites PZM FileProvider mutation evidence
  that predates `5bc2175`.
- `tcfs status` does not show the reconcile retry time.
- A daemon that stays `DAEMON_BUSY` needs an operator. There is no escalation.

## Re-review fix round

An adversarial re-review of `cad5c5d` found one blocker, three majors and
five minors. All nine are fixed in the working tree (staged, not committed).

- **Flaky cancel test (blocker).** The test now holds chunk reads in an
  OpenDAL layer (`ChunkGate`) and drops the open while a read is held, so
  the outcome no longer depends on scheduling. 20 of 20 runs passed.
- **Stalled hydration wedges the flight.** Each chunk download now has a
  deadline (default 120 s). The wait for the next chunk to complete has a
  no-progress deadline (default 180 s). A miss fails the flight, aborts its
  stuck reads, and removes the in-flight entry, so the next open retries.
  `TcfsVfs::with_hydration_timeouts` sets both. The S3 operator also gets
  an OpenDAL `TimeoutLayer` below the retry layer (120 s per operation,
  60 s per IO step). That wiring has no network test.
- **Re-hash on every NFS READ.** `DiskCache` keeps a bounded LRU memo
  (4096 entries) of entries already verified against a manifest. The key is
  the cache key plus the manifest `file_hash`. The memo also checks device,
  inode, size, mtime and ctime. A memo hit skips the re-hash. A changed file
  is re-hashed, and a corrupt one is repaired. Content verified while it
  streamed in is memoized at commit. FUSE benefits too, because its open
  also goes through `open_streaming`. Residual: each READ still reads the
  index entry and the manifest (two small GETs). That is the manifest
  binding, kept on purpose.
- **Racy capture recorded as synced.** `make_sync_state_from_snapshot`
  now records mtime 0 when the capture is racy, as it already did when the
  source moved.
- **The mtime-0 sentinel never cleared.** The new
  `StateCache::needs_sync_refreshing` records the current mtime after a
  matching re-hash. It does this only when the stat identity is the same
  before and after the hash and is outside the racy window. It is used by
  `upload_file_with_device` and by both `push_tree` loops. `needs_sync`
  itself stays pure.
- **Uncapped background hydrations.** A per-mount semaphore caps hydrations
  at 4 by default. `TCFS_VFS_MAX_CONCURRENT_HYDRATIONS` or
  `with_max_concurrent_hydrations` overrides it. The permit is taken only
  for a fetch, after the cache check, so a cache hit never waits behind
  downloads.
- **Spawn under the std mutex.** The open now inserts the flight entry,
  releases the lock, then spawns. A test opens a file on a runtime that has
  shut down. Before the fix it deadlocked. Now it fails cleanly.
- **Backoff settings.** `TCFS_HEALTH_BACKOFF_{BASE,CAP}_SECS` must be
  positive integers, otherwise the default applies. Before the fix, `5m`
  caused a bash arithmetic error, exit 1, and a restart on every tick. A
  cap below the base is raised to the base.
- **Leaked mount probes.** A mount probe abandoned at its deadline now
  records its PID (`mount_hung`) in the state file. While that marker is
  set and the restart backoff is active, the check does not probe the mount
  again. It reports `DOWN ... probe skipped` and exits 2. After the backoff
  expires the mount is probed again. A successful probe clears the marker.

Tests that failed on the old code with a real assertion: the cap test (6
reads at the gate instead of 4), the shutdown test (deadlock), the racy
capture test, and the harness cases for `5m` and for skipping the probe.
Tests that need the new APIs could not compile against the old code: the
stall/retry test, the memo counter test, the cap-parse test, and both
sentinel tests.
