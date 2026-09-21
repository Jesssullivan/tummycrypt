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

Build the same committed agent revision on both hosts. On Sting, create an
empty destination directory and distinct durable state directories for both
ends, then invoke:

```text
tcfs-bulkload-agent pull neo SOURCE DEST SOURCE_STATE DEST_STATE \
  /absolute/path/to/tcfs-bulkload-agent /absolute/path/to/ssh_config
```

`SOURCE` is the verified Neo capture/corpus; `DEST` is a new Bulkload-owned
Sting store. `SOURCE_STATE` and `DEST_STATE` must survive interruption so a
rerun can prove reuse. The remote executable and SSH configuration are
optional only when the defaults are the intended committed build and SSH
configuration.

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