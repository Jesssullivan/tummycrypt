# tummycrypt / tcfs

> Under active development. Not yet stable. Expect breaking changes.

Self-hosted encrypted file sync with on-demand hydration. The Linux FUSE mounted
view is host-proven for clean-name traversal, hydrate-on-open, mounted
write/readback, cache clear/rehydrate, and recursive safe-unsync; offline or
dehydrated sync-root copies can be represented by small `.tc`/`.tcf` stubs. FOSS
odrive/Dropbox-style alternative under active development.

## Canonical Home

`Jesssullivan/tummycrypt` is the canonical source repository for tcfs.
If `tinyland-inc/tummycrypt` exists, treat it as a fork or downstream
distribution surface rather than the source of truth for planning, issues,
releases, or contributor workflow.

Operational policy: [`docs/ops/remote-governance.md`](docs/ops/remote-governance.md).

## Reality And Wayfinding

- Current proof posture: [docs/ops/product-reality-and-priority.md](docs/ops/product-reality-and-priority.md)
- Feature/objective matrix: [docs/ops/feature-objective-matrix-2026-05-09.md](docs/ops/feature-objective-matrix-2026-05-09.md)
- Next fleet parity sprint: [docs/ops/fleet-parity-sprint-plan-2026-05-09.md](docs/ops/fleet-parity-sprint-plan-2026-05-09.md)
- Lazy traversal QA matrix: [docs/ops/lazy-traversal-qa-permutation-matrix-2026-05-09.md](docs/ops/lazy-traversal-qa-permutation-matrix-2026-05-09.md)
- Git repo dogfood canary: [docs/ops/git-repo-canary-dogfood.md](docs/ops/git-repo-canary-dogfood.md)
- Real project-tree canary lane: `task lazy:home-canary-linux-xr-shadow`;
  `home-canary-linux-xr-shadow-20260511T040325Z/` is scoped green for the
  isolated `linux-xr` shadow. The storage-posture lane now has a release-binary
  packet that completes the 7.7 GB shadow, proves honey mounted traversal and
  all 85 symlink targets on the same prefix, and proves the raw Git `.pack` and
  `.rev` object-count fixes. The mounted warning follow-up dropped S3
  `NoSuchKey` rows from 274 to 0 by preserving real `.tc` filenames during VFS
  lookup. A later lifecycle companion reused the same prefix and now reports
  `scoped-project-tree-parity-evidence-complete`; production S3/Finder posture
  remains separate because TLS endpoint posture, socket accounting,
  generated-large-file policy, and production desktop UX are still open
- Next dogfood lane: `task lazy:git-repo-canary` creates a shadow-first packet
  for one clean git worktree, defaulting to `~/git/oauth-mux`. It is the safe
  path toward repo mobility before any live repo, broad `~/git`, `~/Documents`,
  dotfile, or home-directory takeover. The current green small-repo packets are
  `docs/release/evidence/git-repo-canary-oauth-mux-sourcebin-fresh-20260515T014640Z/`
  for source-built binaries and
  `docs/release/evidence/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/`
  for explicit current Nix flake package binaries on both `neo` and `honey`.
  Both prove clean shadow push, 0 skipped symlinks, honey mounted
  traversal/hydration, 9 mounted symlink target checks, and the Linux lifecycle
  companion. Homebrew `0.12.12` remains stale and skips symlinks; live repo
  moves still need large-repo fresh-tree restore and package-backed
  restore/rollback proof.
  `task lazy:git-repo-restore-proof` now records that restore gate; the first
  Nix packet run timed out during `tcfs reconcile` remote-index dry-run and is
  archived as a blocker under the Nix packet. Follow-up source-built proofs fix
  the remote-index timeout and restore all 4,601 regular files plus 9 symlinks
  exactly. The source-built packet,
  `restore-proof-source-fix-empty-dirs-20260515T183805Z/`, also records
  `state_entry_count=4610`, `restored_symlink_state_count=9`, and exact restore
  of all 12 archived empty directories with `--require-empty-dirs`. The current
  rebuilt Nix flake package binary now proves that same fresh-tree restore gate
  in `restore-proof-nixpkg-current-empty-dirs-20260515T200359Z/`
  (`tcfs_sha256=5ee0939f2d1f02cada1c46e429849613b5303fb930e0039a4622d5b712df95a8`).
  Homebrew restore remains stale/unproven. The larger clean stress canary
  against `~/git/linux-xr-fast` is now green for source-built shadow push,
  honey mounted traversal/hydration, and the Linux lifecycle companion in
  `git-repo-canary-linux-xr-fast-sourcefix-index-20260516T045054Z/`. The
  source-built run proves the raw Git pack-index, temp-pack, and exact
  `.git/index` chunk-profile fixes, but its fresh-tree restore attempt remains
  a blocker: `restore-proof/` restored 2,036 of 2,038 regular files and all 6
  empty directories, then missed two multi-GB `.git/objects/pack/*.pack` files
  after transient chunk read failures. Commit `b1a6285` now streams restore
  downloads to disk instead of buffering entire files and cleans failed temp
  files, but a full large-restore rerun is still pending enough local restore
  headroom. Live repo moves, Homebrew readiness, package-backed
  restore/rollback, production Finder, broad `~/git`, and home-directory
  takeover remain unclaimed.
- Current neo Finder truth: fresh May 16 inventory packets confirm
  `/Applications/TCFSProvider.app` is absent, the visible PlugInKit
  registration is still parented by `~/Applications/TCFSProvider.app`, ambient
  `tcfsd` is still `0.12.2`, and strict production preflight against the user
  app fails because host and extension lack Keychain access-group entitlements
  and embedded provisioning profiles. Local profile inventory does find a
  compatible Developer ID profile pair. A source-built
  `TCFSProvider.app` now passes strict signing-only production preflight with
  those profiles embedded, but it is not installed under `/Applications` and no
  PlugInKit/Finder lifecycle was run. Production Finder remains a `#309` gate,
  not a current claim.
- Release install proof: [docs/ops/distribution-smoke-matrix.md](docs/ops/distribution-smoke-matrix.md)
- Apple/Finder reality: [docs/ops/apple-surface-status.md](docs/ops/apple-surface-status.md) and [docs/ops/macos-fileprovider-reality.md](docs/ops/macos-fileprovider-reality.md)
- Live backend acceptance: [docs/ops/neo-honey-acceptance.md](docs/ops/neo-honey-acceptance.md)

## Features

- **On-demand hydration**: Linux FUSE mounted files list as normal names and hydrate transparently on open
- **E2E encryption core path**: XChaCha20-Poly1305 per-chunk, Argon2id KDF, BIP-39 recovery keys; per-surface proof varies
- **Fleet sync**: Multi-machine sync via NATS JetStream with vector clock conflict detection
- **Content-addressed storage**: FastCDC chunking, BLAKE3 hashing, zstd compression
- **Git-safe**: Syncs `.git/` directories as atomic bundles with lock detection
- **Cross-platform**: Linux is the best-supported runtime; macOS has packaged but still experimental desktop surfaces; Windows remains planned

## Quick Start

```bash
# Nix devShell (recommended)
nix develop
# Or auto-load the committed devShell + env on cd
direnv allow

# Or manual: install the pinned Rust 1.93.0 toolchain, protobuf compiler, fuse3 (Linux)

# Start local dev infrastructure (SeaweedFS + NATS + Prometheus + Grafana)
task dev

# Build + test
task check
```

## Installation

```bash
# macOS (Homebrew, current manual tap flow)
brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
brew install Jesssullivan/tummycrypt/tcfs

# Ubuntu 24.04+ / Debian 13+
sudo dpkg -i tcfsd-*.deb tcfs-*.deb

# RPM (Fedora 42 x86_64 proven; RHEL/Rocky pending, daemon-only today)
sudo rpm -i tcfsd-*.rpm

# Container (K8s worker mode; amd64 image is the current proven lane)
podman pull --arch amd64 ghcr.io/jesssullivan/tcfsd:v0.12.12

# Nix tagged profile install
TAG=v0.12.12
nix profile install \
  "github:Jesssullivan/tummycrypt?ref=${TAG}#tcfsd" \
  "github:Jesssullivan/tummycrypt?ref=${TAG}#tcfs-cli"

# Linux/macOS tarball convenience installer
# Fast CLI install, but not part of the canonical release-proof surface.
curl -fsSL https://github.com/Jesssullivan/tummycrypt/releases/latest/download/install.sh | sh
```

For the supported post-release proof contract across Homebrew, `.pkg`, `.deb`,
`.rpm`, container, and Nix, see
[docs/ops/distribution-smoke-matrix.md](docs/ops/distribution-smoke-matrix.md).

## CLI

```bash
tcfs status                    # Daemon status, device identity, NATS connection
tcfs push <path>               # Upload with encryption + vector clock tick
tcfs pull <manifest> <local>   # Download with conflict detection + decryption
tcfs mount <remote> <target>   # Linux FUSE mount with clean-name on-demand hydration
tcfs unsync <path>             # Convert clean tracked files/directories back to .tc stubs
tcfs device enroll             # Register device with age keypair
tcfs device list               # Show enrolled fleet devices
```

## Binaries

| Binary | Purpose |
|--------|---------|
| `tcfs` | CLI: push, pull, mount, unsync, device management |
| `tcfsd` | Daemon: gRPC, Linux FUSE mounts, NATS fleet sync, Prometheus metrics |
| `tcfs-tui` | Terminal UI: dashboard with sync status, conflicts, mounts |
| `tcfs-mcp` | MCP server: AI agent integration (8 tools, stdio transport) |

macOS desktop naming: `TCFSProvider.app` is only the host app, and
`TCFSFileProvider.appex` is the Finder/Files integration extension.

## Architecture

19 workspace crates organized in layers:

```
crates/
├── tcfs-core/           # Shared types, config, protobuf (gRPC service)
├── tcfs-crypto/         # XChaCha20-Poly1305, Argon2id, HKDF, BIP-39
├── tcfs-secrets/        # SOPS/age decryption, KeePassXC, device identity
├── tcfs-storage/        # OpenDAL S3/SeaweedFS operator + health checks
├── tcfs-chunks/         # FastCDC chunking, BLAKE3 hashing, zstd compression
├── tcfs-sync/           # Sync engine, vector clocks, NATS JetStream, reconciliation
├── tcfs-auth/           # TOTP, WebAuthn/FIDO2, device enrollment
├── tcfs-vfs/            # Virtual filesystem: hydration, disk cache, negative cache
├── tcfs-fuse/           # Linux FUSE3 driver
├── tcfs-nfs/            # NFS loopback mount (no kernel modules)
├── tcfs-cloudfilter/    # Windows Cloud Files API (planned)
├── tcfs-file-provider/  # macOS/iOS FileProvider FFI (cbindgen + UniFFI)
├── tcfs-sops/           # SOPS+age fleet secret propagation
├── tcfs-dbus/           # Linux D-Bus interface (stub default; gRPC backend feature-gated)
├── tcfsd/               # Daemon binary (gRPC + metrics)
├── tcfs-cli/            # CLI binary
├── tcfs-tui/            # Terminal UI (ratatui)
└── tcfs-mcp/            # MCP server (rmcp, stdio transport)
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full system design.

For packaged release proof across Homebrew, `.pkg`, `.deb`, `.rpm`, container,
and Nix surfaces, see [docs/ops/distribution-smoke-matrix.md](docs/ops/distribution-smoke-matrix.md).
For the bar after install succeeds, see
[docs/ops/packaged-install-first-use.md](docs/ops/packaged-install-first-use.md).

## Platform Support

| Feature | Linux | macOS | Windows | iOS |
|---------|-------|-------|---------|-----|
| CLI (push/pull/reconcile) | Proven | Install-smoke proven; storage commands available but not continuous macOS acceptance | Planned | - |
| Daemon (gRPC + metrics) | Proven | Available, lightly validated | Planned | - |
| Filesystem mount | x86_64 FUSE lifecycle is host-proven; packaged mount/systemd first-use is still separate; NFS fallback evidence pending | Experimental | Cloud Files API skeleton | - |
| FileProvider | - | Non-production PZM testing-mode lab-proven experimental; production `.pkg` install/signing partial proof exists but Finder remains open | - | Proof-of-concept; write hooks unproven |
| Finder/Explorer badges | - | Experimental | - | - |
| D-Bus integration | Interface exists; release UX not proven | - | - | - |
| Fleet sync (NATS) | Proven core/live lanes | Core path available, not continuously acceptance-tested | Planned | - |
| E2E encryption | Proven core path | Proven core path | Planned | Core crypto path available |

See [docs/platform-support.md](docs/platform-support.md) for details.
For the dated Apple posture, see
[docs/ops/apple-surface-status.md](docs/ops/apple-surface-status.md).

## Development

```bash
task build          # Build all crates
task test           # Run workspace tests
task lint           # Clippy + rustfmt
task deny           # License + advisory check
task check          # All of the above
```

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) for setup details and PR workflow.

## Credential Setup

```bash
task sops:init       # Generate age key + configure .sops.yaml
task sops:migrate    # Migrate credentials to SOPS-encrypted files
```

## License

MIT OR Apache-2.0
