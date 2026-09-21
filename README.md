# tummycrypt / TCFS

TCFS is the encrypted, remote-first userspace filesystem for the tinyland
fleet. Its product goal is simple: enroll a working tree or selected directory
once, then SSH to another enrolled machine and continue with the same bytes,
including in-progress Git and agent state. Hydration and local unsync should be
ordinary verbs, not a migration project.

> Active development. The current release is `v0.12.17`; do not treat TCFS as
> a whole-home or linked-worktree solution yet.

## Start here

- [Vision](docs/VISION.md) — the north star and TCFS's place in Cordillera and
  Rockies.
- [Product sequence](docs/PRODUCT.md) — the approved A → B → C strategy and
  proof ladder.
- [Current truth](docs/ops/current.md) — live blockers, proof boundaries, and
  the next operator ceremony.
- [Documentation index](docs/index.md) — engineering, operations, evidence, and
  client references.

## What is proven

- Linux FUSE browse-before-download, hydration on open, write/readback,
  rehydrate, and safe unsync.
- One real Git repository roamed from `neo` to `honey` with branch, index,
  dirty files, untracked files, stashes, and history intact.
- Divergent raw-`.git` edits converged without committed-work loss through the
  automatic keep-both guard. The separate operator resolver remains gated.
- A bounded Claude project subtree roams between `neo` and `honey`.
- A signed macOS FileProvider lifecycle has been proven in the PZM lab lane.

The proof packets are under [`docs/release/evidence/`](docs/release/evidence/).
Claims without a packet or named live canary remain unproven.

## What is not proven

- Root-targeted production conflict resolution for scheduled roam roots.
- Two repositories completing the full bidirectional roam, unsync, rehydrate,
  divergence, restore, and second-cycle convergence loop.
- Automatic linked-worktree reconstruction through the roaming daemon,
  arbitrary agent sessions, or broad home and dot-directory remotification.
- Per-device-only crypto, headless SSH-first enrollment, or a TLS-protected
  production S3 path.
- Rocky 10 RPM/FUSE acceptance, Windows Explorer parity, iOS production use, or
  NFS client parity.

## Native Bulkload migration tools

This source tree also contains the explicit Rust migration adapter, separate
from automatic TCFS roaming:

```bash
cargo build --release -p tcfs-bulkload-agent -p tcfs-bulkload-bench
target/release/tcfs-bulkload-bench --help
```

The [agent command interface](crates/tcfs-bulkload-agent/src/main.rs) supports
resumable transfers, Git capture/import and guarded worktree restoration.
Use explicit source snapshots and private state/receipt directories; retain
captures until acceptance. An imported bundle is not a restored workspace.
Existing worktrees and provider databases are not replaceable copy targets.

The benchmark compares real local native/rclone copies and resumes on a sealed
corpus. It does not establish network performance or whole-estate completion.
See [TIN-3692](https://linear.app/tinyland/issue/TIN-3692) for migration acceptance
and the outstanding policy decisions.

## Develop

```bash
nix develop
~/.cargo/bin/cargo build --workspace
~/.cargo/bin/cargo test --workspace
~/.cargo/bin/cargo fmt --all -- --check
~/.cargo/bin/cargo clippy --workspace --all-targets
```

The workspace members are declared in [Cargo.toml](Cargo.toml). The protobuf source of truth is
[`crates/tcfs-core/src/proto/tcfs.proto`](crates/tcfs-core/src/proto/tcfs.proto).
See [`AGENTS.md`](AGENTS.md) before changing code or running fleet workflows.

## Install

### Canonical home

The canonical source and release home is
[`Jesssullivan/tummycrypt`](https://github.com/Jesssullivan/tummycrypt).
The Nix release is the least ambiguous current installation surface:

```bash
TAG=v0.12.17
nix profile install \
  "github:Jesssullivan/tummycrypt?ref=${TAG}#tcfsd" \
  "github:Jesssullivan/tummycrypt?ref=${TAG}#tcfs-cli"
```

Other artifacts exist, but their proof tiers differ:

- Homebrew is stale at `0.12.12` and its formula skips symlinks.
- Fedora 42 daemon-only RPM installation is proven; Rocky 10/FUSE is pending.
- Debian/Ubuntu packages, tarballs, containers, and the macOS package have
  lane-specific evidence and gaps.

Use the [distribution smoke matrix](docs/ops/distribution-smoke-matrix.md)
before promoting an artifact or platform claim.

## Architecture in one minute

```text
CLI / TUI / MCP / native clients
              │ gRPC
              ▼
            tcfsd
      ┌───────┼────────┐
      ▼       ▼        ▼
  sync/VFS  crypto   auth/secrets
      │
      ├── encrypted CAS and manifests ──► S3 / SeaweedFS
      └── state events ─────────────────► NATS JetStream
```

APFS, FUSE, FileProvider, NFS, and CFAPI are client or platform substrates.
TCFS is the encrypted roaming and hydration layer above them. SSH transports
the live terminal; TCFS transports persistent file state.

Dual licensed under MIT and Apache-2.0.
