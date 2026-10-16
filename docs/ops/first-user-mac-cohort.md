# First real user: the Mac-only cohort on neo (2026-10-15)

Status: plan and acceptance test. Nothing here is live. The backend is
paused (see [current.md](current.md#status-2026-10-15)), so no step below
can run end to end yet.

Rulings (Linear TIN-4543): **Q156** makes the Mac-only cohort on `neo` tcfs's
first real user. ("Mounted, not copied" below is this page's reading of Q156,
not the ruling's text. Bulkload's ruling C1, OI-1003-Q194 on 2026-10-16,
narrows Q156 to placement: bulkload carries the cohort to the new seat now,
and it is still tcfs's first user later.) **Q155** makes tcfs the
long-run product and bulkload the migration tool; the boundary is in
[tcfs-bulkload-boundary.md](tcfs-bulkload-boundary.md). **Q153** names
tcfs's home: the blahaj SeaweedFS 4.48 deployment, once its preconditions
hold. The cohort itself is named in Q156; this public page does not list it.

## What "mounted, not copied" means

Bulkload's copy (C1) is a migration copy. tcfs does three things on top of
it:

1. It ingests the cohort's files once, from `neo`, into the tcfs backend
   (chunked, encrypted, content-addressed).
2. `neo` then sees the cohort through a tcfs surface (FileProvider or NFS
   loopback). Files hydrate on open and can be evicted again. **Both
   surfaces are read-only today** (see step 5): the cohort can read its
   files through tcfs but cannot change them there.
3. The local originals are removed only after the acceptance test below
   passes, the backend keeps a second copy (see step 8), and the operator
   decides to do it.

## End-to-end path

| # | Step | Owner | State on 2026-10-15 |
| --- | --- | --- | --- |
| 1 | Backend: the blahaj SeaweedFS 4.48 deployment meets the Q153 preconditions (P9 identity answers, `filer_backup` with an off-site copy, a fresh unpublished JWT signing key, NATS `sync_always`), then `s3_hold` flips | operator + lab | Not done. The P9 answers are drafted for operator approval (Q174) |
| 2 | Client config on `neo`: `storage.endpoint` in `config.toml` points at the new backend, with S3 credentials from the existing credential chain. No tcfsd version reads an endpoint-override environment variable, so the config file is the only place to change | lab (Home Manager) | Not done |
| 3 | A tcfs release that contains this branch's ingest, hydration and health work. No release has shipped since `v0.12.17`. As of the 2026-08-22 snapshot, `neo`'s interactive `PATH` selected an unmanaged `v0.12.12` ([current.md](current.md), historical Fleet row; not re-verified), so `neo` needs a known build before ingest | release + lab | Not done |
| 4 | Ingest: enroll the cohort root and push it once from `neo` | operator, attended | Blocked on 1 to 3 |
| 5 | Surface: mount the cohort on `neo` through the chosen surface (see below) | operator decision, then lab | Decision open |
| 6 | Supervision: the lab `tcfsd-health` check adopts `scripts/tcfsd-health-check.sh` (or its logic) so a backend outage reports DOWN instead of restarting tcfsd. Health-restart stays disabled on `neo` until this lands and `neo` runs a release with `tcfs health` (step 3) | lab | Not done; the script ships in this repo |
| 7 | Acceptance test (below) passes | operator, attended | Blocked on 1 to 6 |
| 8 | Operator decides whether to remove the local originals. Preconditions: step 7 passed; the backend replicates to a second failure domain and an off-site restore drill has passed; and the operator accepts that, with no writable macOS surface yet, the cohort can then only read its files | operator | After 7 and the preconditions |

### Step 4: what ingest now guarantees

The branch that adds this page ports bulkload's single-read discipline into
the tcfs walker and upload path (`crates/tcfs-sync/src/ingest.rs`):

- **One read per file.** A settled file in the in-memory path (below
  `STREAMING_THRESHOLD`, 64 MiB) is read once, down from three reads. A
  large file is read twice, down from four. The second pass is required by
  the manifest format (the whole-file hash is the encryption `file_id`), and
  it re-verifies every chunk against the first pass.
- **Changed files are retried, then refused.** Each read sits between two
  stat-identity checks of the same descriptor. A torn read is retried up to
  three times, then refused with a typed `SourceChanged` error, and nothing
  is published for that file. A file modified within two seconds of the read
  ("racy", as in Git and bulkload) is also re-verified by content.
- **No silent drops, no root aborts.** A name that is not UTF-8 is reported
  as a typed refusal (APFS names are always UTF-8, so the cohort should see
  none). An unreadable directory refuses only its own subtree, and the walk
  continues. Reconcile never infers a delete, and never plans a pull, for a
  path under a refused subtree.

On macOS, an unreadable directory is most likely a TCC privacy denial
(Desktop, Documents, Downloads or removable volumes, read by a launchd agent
without Full Disk Access). It now shows up as a refusal in the push log. Fix
the grant and push again; do not exclude the directory to make the warning
go away.

The ingest still never carries live SQLite files, linked-worktree admin
paths or raw `.git` admin paths (see the "Who owns what" table in the
boundary doc). If the cohort holds any of those, they need bulkload or a
later tcfs feature, not this path.

### Step 5: surface choice (operator decision)

FUSE is not an option on `neo`, because macFUSE needs a kernel extension.
That leaves two surfaces:

| | FileProvider (Finder, `~/Library/CloudStorage`) | NFS loopback (`tcfs mount --nfs`) |
| --- | --- | --- |
| Proven | Developer ID lane on PZM: install, enumerate, hydrate, evict and rehydrate ([platform-support.md](../platform-support.md)) | No release evidence yet |
| Writes | **Read-only by design** since `5bc2175` (TIN-2853), until exact version-token conditional publication lands: every Apple mutation entry point rejects before I/O ([hardening status](tcfs-fileprovider-hardening-status-2026-08-28.md)). The PZM mutation evidence predates that commit | Read-only today: the adapter returns `NFS3ERR_ROFS` |
| Large files | Hydrates to a destination file through the sync engine | As of this branch, each READ is served by range from a verified cache file, with bounded memory |
| Operational cost | Extension toggle after domain resets; first-run UX still open | No extension; needs a loopback NFS mount on `neo` |

Both surfaces read only master-wrapped content and fail closed on a
per-device-only (v3) manifest. That matches the fleet today, where every host
is still `wrap_mode = master`.

There is no writable macOS surface today. Whichever surface is chosen, the
cohort reads through tcfs and keeps editing its local originals, which tcfs
ingests again on the next push.

Proposal for the operator (not a ruling): use FileProvider as the cohort's
daily read surface, and NFS loopback as the acceptance and fallback surface.
Keep the local originals as the editable copy until a writable surface
exists (step 8).

## Gaps that remain

| Gap | Where | Note |
| --- | --- | --- |
| Backend not live | Q153 preconditions | Everything below waits on it |
| No writable macOS surface | FileProvider (`5bc2175`, TIN-2853), NFS (`NFS3ERR_ROFS`) | Both surfaces are read-only. Removing the local originals (step 8) would leave the cohort unable to change its files |
| Backend keeps a single on-site copy | backend replication | The backend currently keeps one on-site copy. Local originals stay until replication to a second failure domain exists and an off-site restore drill has passed. Files in the cohort roots should not be evicted until then |
| No release with this work | release lane | `v0.12.17` is the latest release |
| Surface decision | operator | See step 5 |
| NFS READ opens a handle per RPC | `crates/tcfs-nfs/src/adapter.rs` | Memory is now bounded, but each open re-verifies the cached file, so a sequential read of a large file costs O(size) per RPC. Next step: reuse handles across READs for a short TTL |
| VFS writes are whole-file in memory | `crates/tcfs-vfs/src/driver.rs` | A range-read handle loads the whole file into memory on its first write or truncate. The load runs off the handle-table lock, and a file above the in-memory limit (10 GiB) is refused with `EFBIG` instead of loaded |
| Ranged hydration layout is not bound to the manifest | `fetch_range_from_manifest_bytes` | Manifests do not store chunk lengths, so the layout comes from one `stat` per chunk and only the manifest's total size checks it. Legacy v1 manifests are refused. Not wired into any surface, and should not be until per-chunk lengths are in the manifest |
| Health check adoption | lab Home Manager module | The lab `tcfsd-health` script still restarts on a failed mount probe and says HEALTHY when the socket exists. Keep health-restart disabled on a host until it runs a release with `tcfs health` and the lab check has adopted the reference logic |
| Large-file upload reads twice | manifest format | Changing it is a key-scheme decision |

## Supervision: `tcfs health` and the reference check

`tcfs health` prints one line and exits `0` (`UP`), `2` (`DAEMON_DOWN`: no
socket, connection refused, or the status RPC failed), `3` (`BACKEND_DOWN`)
or `4` (`DAEMON_BUSY`: the daemon accepted the connection but did not answer
within 10 s). A push or pull no longer holds the lock the status RPC needs,
so a long transfer does not make a healthy daemon look down.
`scripts/tcfsd-health-check.sh` uses it:

- **Backend down, daemon up:** the script reports DOWN and does not restart
  tcfsd. The 2026-10-15 estate check counted about 115 restarts a day from
  the old check while the backend was paused.
- **Daemon unreachable (`DAEMON_DOWN`), or the mount is stale while the
  daemon reports `UP`:** the script restarts tcfsd, with backoff of base,
  2x, 4x and so on up to a cap. The backoff resets once the daemon answers.
- **Anything else:** a busy daemon, a CLI without `tcfs health`, a CLI or
  config error, a CLI crash, or a probe that times out. The script reports
  BUSY or UNKNOWN (exit 4) and does not restart: it never restarts on a
  verdict nobody gave.
- **Bounded probes on stock macOS:** the probes run under a pure-bash
  deadline (no `timeout`/`gtimeout` needed), and a probe stuck on a dead
  mount is abandoned at the deadline, so the check cannot wedge.

Inside tcfsd, periodic reconcile backs off while storage is unreachable
(from the reconcile interval up to one hour), and it probes storage with one
cheap LIST on each tick so it resumes within one interval of the backend
coming back. A reconcile that fails while storage answers (a local refusal
or collision) is retried at the normal interval and never backs off.

Regression test: `task lazy:test-tcfsd-health-check`.

## Acceptance test

Run this on `neo`, attended, after steps 1 to 6 are done. Every check must
pass before step 8.

1. **Health.** `tcfs health` prints `UP` and exits 0.
2. **Source inventory (read-only).** Record the cohort's relative path list,
   sizes and per-file BLAKE3, without modifying the source.
3. **Ingest.** Push the cohort root once. The push log has no
   `upload failed` lines. Every refusal is listed and explained; the expected
   count is zero, and any `PermissionDenied` refusal is resolved with a TCC
   grant and a re-push.
4. **Enumerate.** Through the chosen surface, the visible relative path set
   equals the inventory from step 2, minus the documented exclusions (live
   SQLite files, raw `.git` admin paths, linked-worktree admin paths).
5. **Hydrate and verify.** Every file read through the surface has the BLAKE3
   recorded in step 2. During hydration of the largest file, tcfsd's resident
   memory stays bounded and does not grow with the file's size.
6. **Evict and rehydrate.** Evict a sample that includes the largest file,
   read it again, and check that the hashes still match.
7. **Outage drill.** With the backend made unreachable, operator-attended
   (for example by re-pausing it), `tcfs health` exits 3 and the health check
   restarts tcfsd zero times in one hour. When the backend comes back,
   `tcfs health` returns to `UP` without a manual restart, and periodic
   reconcile resumes within one reconcile interval.
8. **Record.** File the evidence packet under `docs/release/evidence/` and
   add a dated row to [current.md](current.md).
