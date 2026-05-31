# TCFS Agentic-Flow Static Mirror Packet - 2026-05-31

Status: prepare-only static mirror evidence for `TIN-1740`.

This packet records the first clean manifest-consuming local staging pass for
the static-first agentic-flow mirror profile. It uses
`agentic-flow-mirror-inventory.py --sqlite-mode deny`, so live agent SQLite is
explicitly excluded rather than snapshotted.

No files were sent to `honey`, S3, or TCFS. The full manifest and staged copy
remain under `/private/tmp`; this repo stores only aggregate evidence.

Safety result:

- raw env/auth paths allowed: 0
- raw live DB/WAL/SHM paths allowed: 0
- repo-local worktree paths allowed: 0
- volatile tmp/backup paths allowed: 0
- snapshot rows: 0
- prepare errors: 0
- denied rows copied: 0
- non-transcript allowed files copied locally: 13,460
- transcript rows skipped by default: 1,193
- SQLite rows excluded by static profile: 6

Claim boundary: local prepare-only evidence. This is the first automatic mirror
profile for static config/repo pickup only. It does not transfer to honey, does
not enroll TCFS, does not mirror live SQLite, and does not prove active JSONL
writer handling.
