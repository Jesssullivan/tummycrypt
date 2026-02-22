# Benchmarks

Performance characteristics of tcfs operations. All measurements are preliminary
and will be updated as the benchmark harness matures.

## Chunking Throughput

FastCDC content-defined chunking with BLAKE3 hashing:

| Operation | Throughput | Notes |
|-----------|-----------|-------|
| FastCDC split (8 KiB avg) | TBD | Single-threaded, in-memory |
| BLAKE3 hash | TBD | Single-threaded |
| zstd compress (level 3) | TBD | Single-threaded |
| Full pipeline (chunk + hash + compress) | TBD | Single-threaded |

## Push / Pull Latency

End-to-end latency for push and pull operations against local SeaweedFS:

| File Size | Push (chunk + upload) | Pull (download + reassemble) | Notes |
|-----------|----------------------|------------------------------|-------|
| 1 KiB | TBD | TBD | Single chunk |
| 1 MiB | TBD | TBD | ~128 chunks |
| 100 MiB | TBD | TBD | ~12,800 chunks |
| 1 GiB | TBD | TBD | ~128,000 chunks |

Measured on: localhost SeaweedFS (single master, single volume).

## Compression Ratios

zstd level 3 compression ratios by file type:

| File Type | Avg Ratio | Notes |
|-----------|-----------|-------|
| Source code (.rs, .go, .py) | TBD | High compressibility |
| JSON / YAML | TBD | High compressibility |
| JPEG / PNG images | TBD | Already compressed, ~1.0x |
| Binary executables | TBD | Moderate compressibility |
| Random data | TBD | ~1.0x (incompressible) |

## FUSE Read Latency

On-demand hydration latency (cold cache, local SeaweedFS):

| Operation | Latency | Notes |
|-----------|---------|-------|
| Stub metadata read | TBD | JSON parse only |
| First-byte (small file, 1 chunk) | TBD | Manifest fetch + chunk fetch |
| First-byte (large file, many chunks) | TBD | Manifest fetch + first chunk |
| Full hydration (1 MiB file) | TBD | All chunks fetched in parallel |
| Cached read (after hydration) | TBD | Direct filesystem read |

## Deduplication Efficiency

Content-addressed storage deduplication across common workloads:

| Workload | Files | Raw Size | Deduplicated | Savings |
|----------|-------|----------|--------------|---------|
| Git repo (10 commits) | TBD | TBD | TBD | TBD |
| Photo library (RAW+JPEG) | TBD | TBD | TBD | TBD |
| Node.js project (with node_modules) | TBD | TBD | TBD | TBD |

## Test Environment

Benchmarks will be run on:
- **Hardware**: TBD
- **OS**: Rocky Linux 10 / NixOS
- **SeaweedFS**: 3-master Raft cluster, Drobo 5C volume server
- **Network**: Gigabit Ethernet (local) / Civo K8s (remote)

## Running Benchmarks

```bash
# Automated benchmark suite (future)
task bench

# Manual timing
time cargo run -p tcfs-cli -- push /path/to/testfile
time cargo run -p tcfs-cli -- pull testfile -o /tmp/output
```
