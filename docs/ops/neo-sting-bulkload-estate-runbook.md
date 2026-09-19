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