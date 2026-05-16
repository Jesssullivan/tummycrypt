# Benchmarks

Performance characteristics of tcfs operations, measured with [divan](https://github.com/nvzqz/divan).

Status: partial benchmark snapshot. Sections marked `TBD` are backlog items,
not release evidence or current performance claims.

## Chunking Throughput

FastCDC content-defined chunking with BLAKE3 hashing (single-threaded, in-memory):

| Operation | 1 KiB | 64 KiB | 1 MiB | 10 MiB |
|-----------|-------|--------|-------|--------|
| FastCDC split (4 KiB avg) | 434 MB/s | 564 MB/s | 405 MB/s | 533 MB/s |
| BLAKE3 hash | 555 MB/s | 1.58 GB/s | 1.39 GB/s | 701 MB/s |
| zstd compress (level 3) | 23.5 MB/s | 838 MB/s | 1.26 GB/s | 1.24 GB/s |
| zstd decompress | 693 MB/s | 3.94 GB/s | 2.79 GB/s | 2.58 GB/s |
| Full pipeline (chunk + hash + compress) | 19.2 MB/s | 57.2 MB/s | 44.6 MB/s | 40.9 MB/s |

All values are median throughput. The 1 KiB compress result is dominated by frame setup overhead; real-world chunks average 4-8 KiB and compress at much higher throughput.

## Encryption Throughput

XChaCha20-Poly1305 per-chunk encryption (single-threaded):

| Operation | 1 KiB | 64 KiB | 1 MiB |
|-----------|-------|--------|-------|
| Encrypt chunk | 200 MB/s | 461 MB/s | 252 MB/s |
| Decrypt chunk | 199 MB/s | 484 MB/s | 346 MB/s |

All values are median throughput.

## Push / Pull Latency

End-to-end latency for push and pull operations against local SeaweedFS:

| Workload | Push (chunk + upload) | Pull (download + reassemble) | Object count | Notes |
|-----------|----------------------|------------------------------|--------------|-------|
| 1 KiB file | TBD | TBD | TBD | Single small chunk |
| 1 MiB file | TBD | TBD | TBD | Small-profile chunks |
| 100 MiB source file | TBD | TBD | TBD | Streaming path |
| 1 GiB binary/blob | TBD | TBD | TBD | Large-file profile |
| Raw Git `.pack` | TBD | TBD | TBD | Large-file profile, content-addressed chunk objects |
| Raw Git `.idx` | TBD | TBD | TBD | Git pack indexes now use the large-file profile in source; package-backed rerun still pending |
| Full project tree | TBD | TBD | TBD | Record total files, total bytes, chunk objects, manifests, index writes, retries, and endpoint class |

> Push/pull latencies depend on SeaweedFS deployment topology and will be measured in a future sprint with the local dev stack running.

## S3 Storage Posture

TCFS is S3-first, so correctness evidence is not enough for production storage
claims. A live packet that proves exact content can still expose unacceptable
object-count, retry, latency, or endpoint posture. Future evidence packets
should record:

| Dimension | Required evidence |
|-----------|-------------------|
| Endpoint class | local dev SeaweedFS, private Tailscale SeaweedFS, public HTTPS tunnel, or production-like endpoint |
| Transport/security | `enforce_tls` setting, credential source, bucket/prefix isolation, and whether the run is safe for public CI |
| Large-object shape | source path, file size, selected chunk profile, chunk count, uploaded bytes, skipped bytes, manifest size, and index write timing |
| Queue/concurrency | engine-level file/chunk fanout, OpenDAL concurrency limit, per-chunk timeout posture, retry counts, timeout counts, and backoff behavior |
| Hydration latency | list/index latency, cold first-byte read, full-file hydrate, cache-hit read, and cache-clear/rehydrate timing |

The raw-Git project-tree canary is intentionally allowed to expose these
storage bottlenecks, but those observations are performance evidence, not a
production storage posture claim.

Functional follow-up observations from
`docs/release/evidence/home-canary-linux-xr-shadow-20260511T040325Z/`:

- The isolated `linux-xr` correctness packet passed push reuse, honey mounted
  `find -maxdepth 8`, selected hydrate, all 85 mounted symlink `readlink`
  checks, and the Linux lifecycle companion.
- `push-storage-summary.env` records 92,969 upload rows, 8,047,721,728 uploaded
  bytes, 405,519 total chunks, `chunk_upload_concurrency_values=4`, and no push
  errors.
- Raw Git pack/index shape is still the dominant storage load: pack rows
  account for 70,857 chunks and 6,216,112,937 bytes, while pack-index rows now
  account for 4,600 chunks and 395,854,856 bytes.
- The push started before the final fresh-prefix/progress telemetry landed, so
  `chunk_exists_check_absent_rows=92969` and `chunk_upload_progress_rows=0`.
  Treat this packet as functional and storage-observation evidence, not a
  production storage posture proof.

Partial release-binary storage observations from
`docs/release/evidence/home-canary-linux-xr-storage-posture-20260512T034347Z/`:

- The packet used the release `tcfs 0.12.12` binary with a fresh disposable
  prefix, upload concurrency 8, `TCFS_UPLOAD_ASSUME_FRESH_PREFIX=1`, and
  `chunk_exists_check=false`.
- The dominant 6,216,046,897-byte raw-Git `.pack` completed with 70,856 chunks,
  but showed repeated multi-minute gaps with no progress row and no retry row.
- The adjacent 45,641,304-byte `.rev` completed with 8,405 chunks and showed the
  same stall shape.
- The run was stopped during the normal project-file walk at 4,046 uploaded rows
  after 5,277.06 seconds, with no retry rows. Treat this as storage blocker
  evidence only; `result.env` records `proof=push-failed`.

Push-only release-binary storage observations from
`docs/release/evidence/home-canary-linux-xr-storage-posture-20260513T220442Z/`:

- The packet used the rebuilt release `tcfs 0.12.12` binary from `main`
  `74ac016`, with SHA-256
  `92a456cb810850f76a6cd2bdd88582ff1b795b8b7b042e6d1e33c5170b1697cc`.
- The fresh-prefix push completed with 92,969 upload rows, 8,233,794,656 file
  bytes, 335,831 total chunks, `file_upload_concurrency=8`,
  `chunk_upload_concurrency=8`, `chunk_exists_check=false`, and zero error or
  retry-warning rows.
- The dominant 6,216,046,897-byte raw Git `.pack` now completed as 1,211
  chunks instead of 70,856 chunks. The adjacent `.idx` profile completed as
  4,599 chunks for 395,849,892 bytes.
- The adjacent 45,641,304-byte `.rev` still used the small/default profile in
  this packet and produced 8,405 chunks. Current code now routes `.rev` through
  the same large sequential profile as `.pack`; a later packet must prove the
  reduced reverse-index object count.
- Socket sampling reached highwater 11 while configured upload concurrency was
  8, so the S3 HTTP client/socket accounting remains an open storage-posture
  issue. The endpoint was plaintext tailnet HTTP.
- `result.env` records `proof=shadow-push` and
  `parity_status=full-project-parity-not-claimed` because honey traversal,
  mounted lifecycle, and remote symlink-target verification were disabled.

Release-binary storage observations from
`docs/release/evidence/home-canary-linux-xr-storage-posture-20260514T021513Z/`:

- The packet used the rebuilt release `tcfs 0.12.12` binary from `main`
  `c0c2c0c`, with SHA-256
  `0cacfac3ab32adecf471a4b8ebea4450aa9763033d8c9ef1dad52e4098e86856`.
- The fresh-prefix push completed with 92,969 upload rows, 8,233,794,656 file
  bytes, 327,482 total chunks, `file_upload_concurrency=8`,
  `chunk_upload_concurrency=8`, `chunk_exists_check=false`, and zero error or
  retry-warning rows.
- The dominant 6,216,046,897-byte raw Git `.pack` stayed at 1,211 chunks. The
  adjacent 45,641,304-byte `.rev` now completed as 8 chunks instead of 8,405
  chunks.
- The `.idx` row remains 4,599 chunks for 395,849,892 bytes in this packet.
  The later `linux-xr-fast` blocker confirmed Git pack indexes as the next
  raw-Git bottleneck, and current source routes `.git/objects/pack/*.idx`
  through the large sequential profile. A package-backed rerun still needs to
  prove the new shape. Outside raw Git pack metadata, generated AMD register
  headers are now the largest measured object-count hotspots: a 23,949,786-byte
  `dcn_3_2_0_sh_mask.h` produced 2,986 chunks, and a 16,414,003-byte
  `nbio_7_2_0_sh_mask.h` produced 2,121 chunks.
- Socket sampling again reached highwater 11 while configured upload
  concurrency was 8. The endpoint was plaintext tailnet HTTP.
- A follow-up mounted honey smoke reused the same prefix with pinned honey
  `tcfs 0.12.12` and passed `find -maxdepth 8`, 85 mounted symlink target
  checks, and exact `.clang-format` hydration.
- The mounted warning follow-up
  `docs/release/evidence/home-canary-linux-xr-storage-posture-tc-extfix-20260514T202343Z/`
  closed the S3 `NoSuchKey` noise row: the original mounted run and the
  directory-prefix-only rerun each recorded 274 warnings, while the exact
  `.tc` filename fix rerun recorded 0 `NoSuchKey`, 0 WARN, and 0 ERROR rows.
  The root cause was real linux-xr ftrace files ending in `.tc` being treated
  as TCFS stub aliases during mounted lookup.
- `result.env` records `proof=shadow-push-honey-traversal-symlink-targets` and
  `parity_status=full-project-parity-not-claimed` because the Linux lifecycle
  companion was not part of the original packet.

Lifecycle companion observations from
`docs/release/evidence/home-canary-linux-xr-storage-posture-lifecycle-20260514T213826Z/`:

- The packet reused the completed `20260514T021513Z` prefix and shadow; it did
  not recopy `/Users/jess/git/linux-xr` or rerun the 7.7 GB push.
- `parity-gates.env` records
  `status=scoped-project-tree-parity-evidence-complete` and
  `proof=shadow-push-honey-linux-lifecycle-symlink-targets`.
- Honey reused the same large prefix with the exact `.tc` filename fix binary
  and passed `find -maxdepth 8`, exact `.clang-format` hydration, and all 85
  mounted symlink target checks with 0 actual `WARN`, `ERROR`, or `NoSuchKey`
  rows in the honey smoke and mount logs.
- The nested Linux lifecycle companion passed mounted write/readback, exact
  remote pullback, cache clear/rehydrate, dirty recursive safe-unsync refusal,
  and clean recursive safe-unsync success.
- This closes the scoped lifecycle row for the storage packet, but it remains a
  lab storage packet: endpoint TLS, socket highwater, candidate-package proof
  for the Git pack-index large profile, and generated large-file policy are
  still production storage posture follow-ups.

Pre-fix host observations from
`docs/release/evidence/home-canary-linux-xr-shadow-20260510T201809Z/storage-posture-observations.md`:

- A 395,849,892-byte raw Git `.idx` used the old small-file profile and produced
  72,598 chunks, roughly 5.3 KiB per chunk on average.
- The adjacent 6,216,046,897-byte raw Git `.pack` produced 70,856 chunks, and a
  process sample during snapshot preparation showed about 6.1 GiB resident
  footprint before the streaming snapshot memory fix.
- The packet used a disposable Tailscale SeaweedFS endpoint with HTTP transport
  and forwarded AWS-style credentials; that is useful lab evidence, not a
  production storage endpoint proof.

The first follow-up routed `.idx` files through the moderate pack-index profile,
kept streaming upload snapshots to chunk metadata plus whole-file hash, and
added bounded chunk-upload fanout via `TCFS_UPLOAD_CHUNK_CONCURRENCY` (default
4, cap 64). After the `20260513` packet showed that one 6.2 GB raw Git `.pack`
could still require 70,856 chunk writes, `.pack` / `.rev` / `.iso` / `.img`
files moved to the large sequential FastCDC profile: 1 MiB minimum, 4 MiB
average, 16 MiB maximum. The `20260514` packet proves both raw Git `.pack` and
`.rev` reductions on the full `linux-xr` shadow. The later `linux-xr-fast`
package blocker then showed a 387 MB `.git/objects/pack/*.idx` still dominates
raw `.git` stress, so current source also routes Git pack indexes through the
large sequential profile while leaving generic `.idx` files on the moderate
profile. Follow-up source-built `linux-xr-fast` proof covers Git pack indexes,
temp packs, and the exact `.git/index` file in one completed push, but
fresh-tree restore is still blocked on two multi-GB Git pack downloads. The
next object-model proof is a candidate-package rerun with full restore, plus a
decision on whether generated large source/data files need a similar policy or
whether their measured chunk counts are acceptable. Chunk upload attempts are
bounded by
`TCFS_UPLOAD_CHUNK_TIMEOUT_SECS` (default 300, cap 3600, `0` disables) so a
wedged S3 write slot becomes a retry row instead of an unobservable stall.
Chunk download attempts are tunable via `TCFS_DOWNLOAD_CHUNK_RETRIES` (default
3, cap 32) so large restore proofs can increase retry budget without changing
source.
Fresh-prefix bulk proof can now opt in to
`TCFS_UPLOAD_ASSUME_FRESH_PREFIX=1` to skip per-chunk remote existence checks,
and `TCFS_UPLOAD_PROGRESS_EVERY_CHUNKS=N` records bounded chunk progress for
objects, including a terminal progress row once the object reaches at least `N`
chunks. The fresh-prefix shortcut is only valid for a new disposable prefix;
evidence should preserve the `chunk_exists_check=false` upload log field when it
is enabled. The current rerun proves the `.pack`/`.rev` object-count decisions
and same-prefix mounted traversal, but still does not support a production
throughput, endpoint, or full parity claim.

The release-binary rerun path is codified as
`task lazy:home-canary-linux-xr-storage-posture`. That wrapper delegates the
same isolated-shadow mechanics to `scripts/home-canary-linux-xr-shadow.sh`, but
adds a release-binary guard, fresh-prefix guard, upload concurrency, progress,
and timeout defaults, endpoint/TLS and credential-presence metadata, and an explicit
`production_storage_posture_claim=0` boundary.

## Compression Ratios

zstd level 3 compression ratios by file type:

| File Type | Avg Ratio | Notes |
|-----------|-----------|-------|
| Source code (.rs, .go, .py) | TBD | High compressibility |
| JSON / YAML | TBD | High compressibility |
| JPEG / PNG images | TBD | Already compressed, ~1.0x |
| Binary executables | TBD | Moderate compressibility |
| Random data | TBD | ~1.0x (incompressible) |

> Compression ratios are workload-dependent and will be measured with representative file sets in a future sprint.

## FUSE Read Latency

On-demand hydration latency (cold cache, local SeaweedFS):

| Operation | Latency | Notes |
|-----------|---------|-------|
| Stub metadata read | TBD | JSON parse only |
| First-byte (small file, 1 chunk) | TBD | Manifest fetch + chunk fetch |
| First-byte (large file, many chunks) | TBD | Manifest fetch + first chunk |
| Full hydration (1 MiB file) | TBD | All chunks fetched in parallel |
| Cached read (after hydration) | TBD | Direct filesystem read |

> FUSE latencies require a running mount point and will be measured in a future sprint.

## Deduplication Efficiency

Content-addressed storage deduplication across common workloads:

| Workload | Files | Raw Size | Deduplicated | Savings |
|----------|-------|----------|--------------|---------|
| Git repo (10 commits) | TBD | TBD | TBD | TBD |
| Photo library (RAW+JPEG) | TBD | TBD | TBD | TBD |
| Node.js project (with node_modules) | TBD | TBD | TBD | TBD |

> Deduplication efficiency depends on workload characteristics and will be measured with real datasets.

## Test Environment

Benchmarks measured on:
- **CPU**: Intel Core i7-8550U @ 1.80 GHz (4 cores / 8 threads, turbo to 4.0 GHz)
- **RAM**: 16 GB DDR4
- **Storage**: Samsung MZVLW256 NVMe SSD (238.5 GB)
- **OS**: Rocky Linux 10 (kernel 6.12.0)
- **Rust**: 1.93.0 (repo-pinned toolchain, edition 2021, `opt-level = 3`, `lto = "thin"`)
- **Benchmark framework**: divan 0.1

## Running Benchmarks

```bash
# All benchmarks
task bench

# Individual suites
cargo bench -p tcfs-chunks --bench chunks
cargo bench -p tcfs-crypto --bench crypto
```
