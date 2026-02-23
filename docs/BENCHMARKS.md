# Benchmarks

Performance characteristics of tcfs operations, measured with [divan](https://github.com/nvzqz/divan).

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

| File Size | Push (chunk + upload) | Pull (download + reassemble) | Notes |
|-----------|----------------------|------------------------------|-------|
| 1 KiB | TBD | TBD | Single chunk |
| 1 MiB | TBD | TBD | ~128 chunks |
| 100 MiB | TBD | TBD | ~12,800 chunks |
| 1 GiB | TBD | TBD | ~128,000 chunks |

> Push/pull latencies depend on SeaweedFS deployment topology and will be measured in a future sprint with the local dev stack running.

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
- **Rust**: 1.93+ (edition 2021, `opt-level = 3`, `lto = "thin"`)
- **Benchmark framework**: divan 0.1

## Running Benchmarks

```bash
# All benchmarks
task bench

# Individual suites
~/.cargo/bin/cargo bench -p tcfs-chunks --bench chunks
~/.cargo/bin/cargo bench -p tcfs-crypto --bench crypto
```
