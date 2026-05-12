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
| Raw Git `.idx` | TBD | TBD | TBD | Must use the large-file profile; small-profile regressions create excessive S3 object counts |
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
| Queue/concurrency | engine-level file/chunk fanout, OpenDAL concurrency limit, retry counts, and backoff behavior |
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

The follow-up work routes `.idx` files through the large-file profile, keeps
streaming upload snapshots to chunk metadata plus whole-file hash, and adds
bounded chunk-upload fanout via `TCFS_UPLOAD_CHUNK_CONCURRENCY` (default 4, cap
64). Fresh-prefix bulk proof can now opt in to
`TCFS_UPLOAD_ASSUME_FRESH_PREFIX=1` to skip per-chunk remote existence checks,
and `TCFS_UPLOAD_PROGRESS_EVERY_CHUNKS=N` records bounded chunk progress for
objects, including a terminal progress row once the object reaches at least `N`
chunks. The fresh-prefix shortcut is only valid for a new disposable prefix;
evidence should preserve the `chunk_exists_check=false` upload log field when it
is enabled. These changes still need a fresh release-build host rerun before any
production throughput, object-count, or memory claim is made.

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
