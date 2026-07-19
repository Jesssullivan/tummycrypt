# TCFS documentation

TCFS is a self-hosted encrypted filesystem and sync fabric with content-addressed
storage, vector-clock conflict detection, selective hydration, and native or
userspace client surfaces. Linux is the strongest runtime today.

## Product spine

Read these in order:

1. [README](../README.md) — concise capability and development entry point.
2. [Vision](VISION.md) — hydratable-anywhere north star and program fit.
3. [Product](PRODUCT.md) — accepted A → B → C sequence, root identity, and
   proof ladder.
4. [Current workstream](ops/current.md) — living blockers and next operator
   ceremony.

These are the only general documents allowed to call themselves current.
Dated plans are evidence or historical context unless the current workstream
explicitly promotes them.

## Architecture and protocol

- [Architecture](ARCHITECTURE.md) — system and crate overview.
- [Protocol](PROTOCOL.md) — manifests, chunks, and gRPC contract.
- [Security](SECURITY.md) — threat model and crypto architecture.
- [RFC 0001: fleet sync](rfc/0001-fleet-sync-integration.md)
- [RFC 0002: Darwin/FileProvider strategy](rfc/0002-darwin-fuse-strategy.md)
- [RFC 0003: iOS FileProvider](rfc/0003-ios-file-provider.md)
- [RFC 0004: FUSE-free architecture](rfc/0004-fuse-free-architecture.md)

## Build and contribute

- [Contributing](CONTRIBUTING.md)
- [Benchmarks](BENCHMARKS.md)
- [Changelog](../CHANGELOG.md)
- [Platform support](platform-support.md)

Use the repository's [`AGENTS.md`](../AGENTS.md) as the operator and
development instruction source.

## Daily-driver and roam operations

- [Stable root routing ADR](design/stable-root-routing-2026-07-14.md) — narrow
  daemon-trusted conflict routing seam; not broad root adoption.
- [Versioned root registry/status ADR](design/versioned-root-registry-status-b0a-2026-07-19.md)
  — B0a authorized immutable V1 inventory; no reconcile, mutation, MCP, or
  live deployment.
- [Divergent Git keep-both design](design/git-divergent-keep-both-2026-07-02.md)
- [Git repository dogfood canary](ops/git-repo-canary-dogfood.md)
- [Repo-roam acceptance](ops/git-roam-daily-driver-acceptance-2026-06-08.md)
- [Repo-roam test plan](ops/repo-roam-test-plan-2026-06-08.md)
- [Large-workdir sequencing](ops/large-workdir-daily-driver-sequencing-2026-05-30.md)
  — historical 2026-05-30 sequencing snapshot, not the current work queue.
- [Neo/honey acceptance](ops/neo-honey-acceptance.md)
- [Operator decisions](ops/operator-decision-record-2026-07-01.md)
- [Remote governance](ops/remote-governance.md)

## Security and recovery

- [Per-device crypto migration](ops/per-device-crypto-migration-2026-06-06.md)
- [Per-device identity design](ops/per-device-crypto-identity-design-2026-05-18.md)
- [Raw .git corruption analysis](ops/dotgit-as-files-conflict-corruption-2026-06-08.md)
- [Ghost-device revocation safety](ops/ghost-device-revocation-safety-2026-07-02.md)
- [On-prem authority recovery](ops/onprem-authority-recovery.md)

## Packaging and clients

- [Distribution smoke matrix](ops/distribution-smoke-matrix.md)
- [Packaged install to first use](ops/packaged-install-first-use.md)
- [Lab host acceptance](ops/lab-host-acceptance-matrix.md)
- [Apple surface status](ops/apple-surface-status.md) — historical client-state
  snapshot, not a current readiness claim.
- [macOS FileProvider reality](ops/macos-fileprovider-reality.md) — historical
  Finder/FileProvider proof snapshot.
- [iOS surface status](ops/ios-surface-status.md) — historical proof-of-concept
  boundary.
- [odrive behavior horizon](ops/odrive-parity-product-horizon.md) — historical
  product-horizon comparison.

## Evidence and history

- [Evidence index](release/evidence/README.md) — immutable proof packets.
- [Release evidence](release/) — release-specific matrices and results.
- [Archive](archive/README.md) — history pointers, not active instructions.

The Git repository is the full archive. Removed obsolete design and instruction
files remain available by commit without occupying the active documentation or
agent-instruction namespace.
