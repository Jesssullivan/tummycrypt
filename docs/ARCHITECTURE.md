# tcfs Architecture

**tcfs** (TummyCrypt FileSystem) is a FOSS, self-hosted, S3-first synchronization system that
replaces proprietary cloud storage clients (odrive, Dropbox, etc.) with a FUSE-based mount
that transparently hydrates files on demand.

## System Overview

```
User Machine                              Cloud / Self-hosted
┌─────────────────────────────────┐      ┌───────────────────────────────────┐
│                                 │      │                                   │
│  /mnt/tcfs/                     │      │  SeaweedFS Cluster                │
│  ├── project/                   │      │  ┌──────────┐ ┌──────────────┐   │
│  │   ├── main.rs.tc  (stub)     │◄────►│  │ Masters  │ │ Volume Svrs  │   │
│  │   ├── lib.rs      (hydrated) │      │  │ (3x raft)│ │ (Drobo 5C)   │   │
│  │   └── docs.tcf    (dir stub) │      │  └──────────┘ └──────────────┘   │
│  └── photos/                    │      │         ↑                         │
│      └── 2024-01-01.jpg.tc      │      │  ┌──────────────────────────┐    │
│                    │            │      │  │ Filer + S3 Gateway        │    │
│  ┌─────────────┐   │            │      │  │ dees-appu-bearts:8333     │    │
│  │  FUSE VFS   │   │ open()     │      │  └──────────────────────────┘    │
│  │  (tcfs-fuse)│───┘            │      │                                   │
│  └──────┬──────┘                │      │  NATS JetStream                   │
│         │ gRPC Hydrate          │      │  ┌──────────────────────────┐    │
│  ┌──────▼──────┐                │      │  │ SYNC_TASKS stream        │    │
│  │   tcfsd     │                │      │  │ HYDRATION_EVENTS stream   │    │
│  │  (daemon)   │────────────────┼─────►│  └──────────────────────────┘    │
│  └──────┬──────┘                │      │         │                         │
│         │                       │      │  ┌───────▼────────────────────┐   │
│  ┌──────▼──────┐                │      │  │ Sync Workers (K8s pods)    │   │
│  │   RocksDB   │                │      │  │ HPA: 1–100 via KEDA        │   │
│  │ (state cache│                │      │  └────────────────────────────┘   │
│  └─────────────┘                │      │                                   │
└─────────────────────────────────┘      └───────────────────────────────────┘
```

## Components

### Client-side

| Component | Binary | Purpose |
|-----------|--------|---------|
| `tcfs-fuse` | (library) | FUSE driver: stubs, hydration, negative cache |
| `tcfsd` | `tcfsd` | Daemon: gRPC server, FUSE mount mgr, cred loader |
| `tcfs-cli` | `tcfs` | CLI: mount, push, pull, sync, status, unsync |
| `tcfs-tui` | `tcfs-tui` | TUI: dashboard, file browser, progress bars |
| `tcfs-sync` | (library) | Sync engine: RocksDB state, NATS workers |
| `tcfs-chunks` | (library) | FastCDC chunking, BLAKE3 hashing, zstd |
| `tcfs-storage` | (library) | OpenDAL abstraction, SeaweedFS native API |
| `tcfs-secrets` | (library) | SOPS/age/KDBX credential chain |
| `tcfs-core` | (library) | Shared types, config schema, proto defs |

### Server-side (K8s)

| Component | Purpose |
|-----------|---------|
| SeaweedFS cluster | Distributed blob storage |
| NATS JetStream | Reliable task queue (SYNC_TASKS, HYDRATION_EVENTS) |
| Sync workers | Stateless NATS consumers, HPA-scaled via KEDA |
| Metadata service | Leader-elected coordinator (Kubernetes Lease API) |
| Prometheus + Grafana | Observability: throughput, queue depth, FUSE latency |

## Stub File Format

Files not yet downloaded appear as `.tc` stubs (zero bytes in POSIX, metadata in stub content):

```
version https://tummycrypt.io/tcfs/v1
chunks 23
compressed 0
fetched 0
oid blake3:4d7a2146...e239
origin seaweedfs://filer.example.com/bucket/path/to/file
size 94371840
```

Directory stubs use the `.tcf` extension and list child entries.

See `docs/PROTOCOL.md` for the full specification.

## Hydration Sequence

```
User process         FUSE VFS         tcfsd           SeaweedFS
     │                  │               │                 │
     │── open("x.tc") ─►│               │                 │
     │                  │── Hydrate ───►│                 │
     │                  │               │── S3 GetObject ►│
     │                  │               │◄── stream ──────│
     │                  │               │ (chunk assembly) │
     │                  │◄──── fd ──────│                 │
     │◄── fd ───────────│               │                 │
     │── read() ────────►│               │                 │
     │◄── data ──────────│               │                 │
```

After hydration, `x.tc` is atomically replaced with `x` (the real file).
`tcfs unsync x` converts it back to a stub.

## Credential Chain

```
tcfsd startup:
  1. $CREDENTIALS_DIRECTORY/age-identity   (systemd LoadCredentialEncrypted)
  2. $SOPS_AGE_KEY_FILE                    (env var path)
  3. $SOPS_AGE_KEY                         (env var literal)
  4. ~/.config/sops/age/keys.txt           (fallback)
  → age identity loaded → SOPS decrypt credentials/*.yaml
  → S3 credentials available to OpenDAL operator
  → mtime watcher: auto-reload on credential file change
```

## Phase Roadmap

| Phase | Scope | Status |
|-------|-------|--------|
| 0 | Repo foundation, SOPS migration, Rust workspace stubs | In Progress |
| 1 | Core daemon + secrets + gRPC | Pending |
| 2 | Sync engine + chunking + NATS | Pending |
| 3 | FUSE driver + .tc stubs + hydration | Pending |
| 4 | K8s backend + HPA + full Tofu deploy | Pending |
| 5 | RemoteJuggler KDBX integration | Pending |
| 6 | Nix packaging + RPM + Homebrew | Pending |
| 7 | Production hardening + chaos tests | Pending |

## Real Infrastructure (Local Network)

| Role | Address |
|------|---------|
| SeaweedFS master-1 | 192.168.101.249:9333 |
| SeaweedFS master-2 | 192.168.101.184:9333 |
| SeaweedFS master-3 | 192.168.101.248:9333 |
| Volume server (Drobo 5C) | 192.168.101.171:8080 |
| Filer / S3 gateway | 192.168.101.146:8333 (dees-appu-bearts) |
| Civo K8s cluster | bitter-darkness-16657317 (namespace: fuzzy-dev) |
