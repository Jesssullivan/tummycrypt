# Neo → Sting Bulkload Estate Transport Runbook

This runbook covers ordinary-file and estate-corpus movement for the Neo →
Sting migration. It is a required boundary of the Bulkload acceptance work in
PR #592, not a replacement migration implementation.

## Transport Boundary

Use only the native `tcfs-bulkload-agent pull`/`serve` framed protocol for a
cross-host payload. A same-host controlled test may use `copy`. Do not use
`rsync`, `scp`, rclone, tar pipes, or manual file copying for estate payloads,
completion receipts, or Bulkload benchmark inputs.

The rejected raw corpus has a durable incident receipt at
`/srv/fast-local/jess/state/git-carry/estate-absent-20260918/nonblahaj-rsync-noncompliance-20260918.md`.
Its quarantined content is not an `estate-apply` input. A replacement must use
a fresh, Bulkload-owned Sting destination after revalidating the Neo capture.

## Required Native Pull

Build the same committed agent revision on both hosts. The five positional
paths split across the two hosts; getting the split wrong is how the first
pilot attempt failed (TIN-3692 receipt 2026-09-21T11:56Z).

```text
tcfs-bulkload-agent pull SOURCE_HOST SOURCE DEST SOURCE_STATE DEST_STATE \
  /absolute/path/to/tcfs-bulkload-agent /absolute/path/to/ssh_config
```

| Argument | Lives on | Requirement |
|---|---|---|
| `SOURCE` | the source host (Neo) | the verified capture/corpus root |
| `SOURCE_STATE` | **the source host (Neo)** | an existing directory, mode `0700`, owned by the invoking user |
| `DEST` | the puller (Sting) | a new, empty, Bulkload-owned store |
| `DEST_STATE` | the puller (Sting) | an existing directory, mode `0700`, owned by the invoking user |

`SOURCE_STATE` is **not** a path on the puller. `receive()` in
`crates/tcfs-bulkload-agent/src/transfer.rs` sends the `TransferOpen { root,
state }` request frame to the remote `serve`, which opens its content store at
that path on the source host. Both state stores refuse an existing directory
with any group/other permission bit set (`Store::open` → `private_dir` in
`transfer_store.rs`; surfaces as an `IO`/`PathEscapesRoot` refusal), so
pre-create each one as `0700` on its own host before the first pull. Both
must survive interruption so a rerun can prove reuse. The remote executable
and SSH configuration are optional only when the defaults are the intended
committed build and SSH configuration; the remote executable path must be
absolute.

The pilot command that worked, run on Sting at revision `c3be9d75` on both
ends (TIN-3692, 2026-09-21T11:56Z; `completed=38 reused=0
bytes_received=181,480,309 source_bytes_read=553,892,133 refusals=0` in
13.6 s, warm re-run `completed=0 reused=38 bytes_received=0
source_bytes_read=0`):

```text
~/.local/bin/tcfs-bulkload-agent-c3be9d7 pull neo \
  /Users/jess/state/bulkload-run-20260921 \
  /srv/fast-local/jess/bulkload/run-20260921 \
  /Users/jess/state/bulkload-run-20260921-srcstate \
  /srv/fast-local/jess/bulkload/state-20260921/dst \
  /Users/jess/.local/bin/tcfs-bulkload-agent-c3be9d7
```

The two `/Users/jess/...` paths are Neo paths; the two `/srv/fast-local/...`
paths are Sting paths.

### Exit contract with partial refusals

`pull` and `estate-apply` print every per-item outcome, then exit non-zero
with `CONTRACT_SELF_INCONSISTENT` when **any** item was refused, even though
the other items completed and their receipts are durable
(`report_transfer` in `main.rs`; the apply loop in `estate.rs`). A non-zero
exit therefore does not mean nothing landed: read the per-item lines and the
`refusals=` count, and record both in the receipt. The pilot apply exited
this way with 10 `refs-imported`, 1 `workspace-restored`, and 2 `refused
(IO (errno 2))` for the two items that had no capture record.

Keep `estate-apply` separate from transfer. It accepts only a corpus whose
native transfer receipt and digest verification are complete.

## Receipt Requirements

Retain one durable receipt for each transfer in the state roots and link it
from PR #592 evidence. Include:

- source and destination corpus identities and BLAKE3 manifest/digest result;
- source and destination state paths, agent revision, and binary digest;
- the native command identity without credentials;
- the emitted `completed`, `reused`, `bytes_received`,
  `source_bytes_read`, and typed-refusal count; and
- interruption/resume observations. An unchanged completed warm retry must
  report `source_bytes_read=0` and no content transfer.

Never substitute a raw-transfer receipt for a native receipt. The rclone
comparison remains a benchmark control only: R23 requires three alternating
native/rclone repetitions on the same sealed corpus, with verification outside
the timed transfer, before an initial-copy win is claimed.

## Current R23/R25 Gate

The corrected 2026-09-18 matrix is archived in
[`neo-sting-bulkload-r23-evidence-2026-09-18.md`](neo-sting-bulkload-r23-evidence-2026-09-18.md).
It used revision `c6cead96f325+worktree-d7f9f8d33baf0e99`, rclone 1.75.0,
three alternating native/rclone/native/rclone/native samples, a deterministic
`2,426,057`-byte mutation, and full-BLAKE3 verification outside timing.

| Gate | Native median/result | Control/limit | Verdict |
|---|---:|---:|---|
| Initial copy | 3015.294 ms | rclone 601.010 ms | **R23 fail** |
| 1%-byte delta | 58.842 ms | rclone 127.064 ms | pass |
| Unchanged warm resume | 0 read, 0 received | exactly zero | pass |
| Interrupted resume | 0 read, 0 received | exactly zero | pass |
| Cumulative native peak RSS | 234,400 KiB | below 2 GiB | pass |

The initial-copy loss keeps all estate transport and apply operations blocked.
Do not reinterpret the delta win or the approximately 10.7× improvement over
the prior 32.369-second native result as an R23 pass.