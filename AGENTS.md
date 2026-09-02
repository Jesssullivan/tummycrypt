# tcfs Development Context

## Program context — where truth lives

This file covers build/test/navigation and the machines that run them. For what
is true and what ships next, read these in order (each owns a distinct layer;
none is duplicated here):

1. [docs/VISION.md](docs/VISION.md) — north star + the claim-tier legend.
2. [docs/PRODUCT.md](docs/PRODUCT.md) — the accepted A→B→C delivery sequence,
   stable-root design, repository ownership.
3. [docs/ops/current.md](docs/ops/current.md) — the authoritative live
   blocker/proof boundary (see its own precedence preamble).
4. [docs/platform-support.md](docs/platform-support.md) — per-client maturity.
5. [docs/release/evidence/README.md](docs/release/evidence/README.md) —
   evidence corpus index.

Operational ladders an agent should read rather than re-derive:

- **Gate ladder (G0–G6).**
  [docs/ops/large-workdir-daily-driver-sequencing-2026-05-30.md](docs/ops/large-workdir-daily-driver-sequencing-2026-05-30.md)
  owns the gate definitions and their dependency order (`G1 + G2 → G3 → G4`,
  with `G5`/`G6` following). It is explicitly superseded for *live status* by
  `docs/ops/current.md` — read it for sequencing, not for state.
- **Repo-roam program.**
  [docs/ops/repo-roam-test-plan-2026-06-08.md](docs/ops/repo-roam-test-plan-2026-06-08.md)
  is the G5 dev-env zero-diff ladder over `neo`/`honey`;
  [docs/ops/git-roam-daily-driver-acceptance-2026-06-08.md](docs/ops/git-roam-daily-driver-acceptance-2026-06-08.md)
  is the "machine does not matter" acceptance plan.
- **Per-device crypto is sequenced, not a config flip.**
  [docs/ops/per-device-crypto-identity-design-2026-05-18.md](docs/ops/per-device-crypto-identity-design-2026-05-18.md)
  is the design;
  [docs/ops/per-device-crypto-migration-2026-06-06.md](docs/ops/per-device-crypto-migration-2026-06-06.md)
  owns the expand/contract ordering and the tri-state `crypto.wrap_mode` gate
  that replaces the shipped-but-never-flipped `crypto.per_device_wrapping`
  bool; and
  [docs/ops/shared-master-fleet-migration-runbook-2026-07-28.md](docs/ops/shared-master-fleet-migration-runbook-2026-07-28.md)
  owns how to execute one step. Every host is still `wrap_mode = master`. Do
  not flip a wrapping mode as a side effect of a code change, and treat
  `tcfs key rotate <prefix>` as a rebuild gate rather than an available
  operator remedy.

Tracked work: Linear initiative "Tummycrypt — Daily Driver Track"
(https://linear.app/tinyland/initiative/tummycrypt-daily-driver-track-95eeeb5e7493),
umbrella "Cordillera - Tinyland Remote-Everything Program"
(https://linear.app/tinyland/initiative/cordillera-tinyland-remote-everything-program-15f56b187c19).
Sibling repos: tinyland-inc/rockies (OS adoption seed, TIN-2300),
tinyland-inc/lab (fleet deploy/pins, host inventory, and the estate's
build-placement and host-hold rulings), Jesssullivan/prompts-enqueue
(program ledger, prompts 47/60).

## Quick Start

```bash
# Enter the Nix devShell (recommended). Its shellHook puts the pinned
# toolchain ahead of any Home Manager rustc/cargo already on PATH.
nix develop
# Or let direnv load the committed .envrc once:
direnv allow

# Build / test / lint. On `neo` these run on `sting` or `honey`, never
# locally — see "Machines that run this repo" below.
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets

# Related lanes through the task runner. The flags are NOT identical to the
# cargo block or to CI: `task lint` adds `-D warnings` but omits
# `--all-targets`, so it never compiles tests/benches/examples; CI clippy is
# `--all-targets` without `-D warnings`. Green in one does not imply the other.
task build         # cargo build --workspace
task test          # cargo test --workspace
task lint          # cargo clippy --workspace -D warnings + cargo fmt --check

# Local dev stack: 8 containers (3 SeaweedFS masters + volume + filer, NATS,
# Prometheus, Grafana) via docker-compose. Linux host only — see below.
task dev
```

## Environment Notes

### Toolchain

- **Rust**: edition 2021; workspace `rust-version = "1.93"`; pinned to
  `1.93.0` by `rust-toolchain.toml`. `.envrc` prints a loud direnv error when
  the active `rustc` is below the minimum or does not match the pin — but
  `log_error` does not abort the load, so the mismatched `rustc` stays on
  `PATH`. Check `rustc --version` yourself; do not treat a successful direnv
  load as proof.
- **Invoke plain `cargo`.** It is expected to come from the pinned toolchain or
  the Nix devShell (`Justfile` header; `flake.nix` `shellHook`). Do not
  hardcode `~/.cargo/bin/cargo` — that path is only the rustup fallback
  `.envrc` adds when `nix` is absent. Known drift: `Taskfile.yaml` `bench`
  still hardcodes `~/.cargo/bin/cargo bench`, so `task bench` fails on a
  Nix-only machine.
- **Linker**: there is no `mold` in the Nix devShell or in any system
  toolchain this repo assumes — `flake.nix` does not package it. The default
  system linker is used everywhere. The `mold` comment at the top of
  `.cargo/config.toml` is stale; do not act on it, and do not add a `mold`
  entry to `.cargo/config.toml`.
- **PATH ordering**: the devShell and `.envrc` both put `target/debug` and
  `target/release` ahead of installed packages, so a stale workspace build can
  shadow the deployed `tcfs`/`tcfsd`. Separately, on `neo` an unmanaged
  `v0.12.12` install currently shadows the managed build in the effective
  interactive `PATH` (`docs/ops/current.md`, Fleet row; an attended cleanup is
  queued). Smoke harnesses print the resolved binary path — check it before
  believing a version string.

### Machines that run this repo

Not exhaustive: acceptance and storage hosts are a separate pool, listed in
[docs/ops/lab-host-acceptance-matrix.md](docs/ops/lab-host-acceptance-matrix.md).

| Host | What it is | How agents use it |
| --- | --- | --- |
| `neo` | macOS (Darwin) maintainer workstation, resource-constrained | Orchestration, review, editing, and the release-adjacent `neo → honey` live lane. **No local compile/test/`task dev` here** — the estate build-placement ruling sends heavy toolchain work to a remote host, and an unbounded local build takes every session on the machine down with it. Login shell is `bash`, so agent and SSH commands land in bash; interactive terminals re-enter fish from emulator config, so `export VAR=VALUE` fails at a `neo` prompt too. |
| `honey` | Rocky Linux 10; canonical Linux control point | High-volume push/pull, daemon and service checks, conflict and stress lanes, Linux-first operator truth. Login shell is **`fish`**, which has no `export VAR=VALUE`; use `env VAR=VALUE command` or wrap in `bash -c '...'`. |
| `sting` | Rocky Linux 10.2; headless remote-dev seat driven from `neo` | The dev seat for build/test work that must not run on `neo` — under the bounded readmission below. Login shell is POSIX `bash` with an interactive-only fish handoff, so a non-interactive SSH command lands in `bash`. |

Host facts, shell settings, and every hold state above are owned by
`tinyland-inc/lab` (`AGENTS.md`, `inventory/host_vars/{macbook-neo,honey,sting}.yml`,
`vars/fleet_switch_targets.json`); that repo is authoritative if it disagrees
with this table.

- **Rocky-specific (`honey`, `sting`)**: a rustup install lands in
  `~/.cargo/bin`, which is not on `PATH` by default. `.envrc`'s non-Nix
  fallback adds it. Inside `nix develop` the pinned toolchain leads and this
  does not apply.
- **`sting` is under an open continuity hold.** The lab-side machine hold
  (`STING_CONTINUITY_INCIDENT_2026-08-17`) is still active; `hold.active` is
  `true` and the host stays out of every rendered switch scope. An attended
  2026-08-22 operator ruling readmitted the **dev-seat role only** —
  interactive SSH, tmux, and dev work under `~/git`. That readmission lifts no
  other role. Before using `sting`, read `hold.roles` in lab's
  `vars/fleet_switch_targets.json`. `sting` also carries production roles owned
  by lab and outside this repo's scope, so unbounded resource use, reboots, and
  any host-level change there are not this repo's call.
- **`sting`'s `tcfs_runtime` role is held disabled** — `tcfsd` and
  `tcfsd-health` are inactive and disabled/masked/not-found. Do not start a
  daemon or drive enrollment there. That is narrower than "not a TCFS target":
  `docs/ops/current.md` still queues `sting` for release/root-topology
  convergence and for candidate/activation invariants, so fleet-coherence work
  against it is sequenced, not forbidden — it just needs its own attended
  readmission first.
- **Acceptance hosts are a different question.** The host pool, lane order, and
  reset contract for real-host acceptance live in
  [docs/ops/lab-host-acceptance-matrix.md](docs/ops/lab-host-acceptance-matrix.md)
  and [docs/ops/neo-honey-acceptance.md](docs/ops/neo-honey-acceptance.md).
  A dev seat is not an acceptance target. That matrix's `sting` row still reads
  "none yet / blocked on lab-side hardware stabilization"; it predates the
  dev-seat ratification and correcting it is an operator call, not a
  docs-hygiene one.
- **Live-work freezes outrank convenience.** `docs/ops/current.md` records
  which live resolver, enrollment, deploy, and crypto ceremonies are frozen.
  Source review, tests, and landing continue during a freeze; new fleet claims
  do not.

## Workspace Crates (19 members)

| Crate | Type | Description |
|-------|------|-------------|
| `tcfs-core` | lib | Shared types, config, errors, protobuf (gRPC service definition) |
| `tcfs-auth` | lib | Authentication and authorization providers |
| `tcfs-vfs` | lib | Virtual filesystem trait, disk cache, stub formats, hydration |
| `tcfs-crypto` | lib | XChaCha20-Poly1305 encryption, Argon2id KDF, BIP-39 |
| `tcfs-secrets` | lib | SOPS/age decryption, KeePassXC, device identity/registry |
| `tcfs-sops` | lib | SOPS+age fleet secret propagation |
| `tcfs-storage` | lib | OpenDAL S3/SeaweedFS operator + health checks |
| `tcfs-chunks` | lib | FastCDC chunking, BLAKE3 hashing, zstd compression |
| `tcfs-sync` | lib | Sync engine, vector clocks, state cache, NATS JetStream |
| `tcfs-fuse` | lib | Linux FUSE driver (fuse3) |
| `tcfs-nfs` | lib | NFS loopback server (NFSv3, FUSE-free mount) |
| `tcfs-cloudfilter` | lib | Windows Cloud Files API (CFAPI) provider |
| `tcfs-file-provider` | lib | C FFI bridge for macOS/iOS FileProvider (cbindgen/uniffi) |
| `tcfs-dbus` | lib | D-Bus interface for Linux file sync status |
| `tcfsd` | lib+bin | Daemon: gRPC over Unix socket, FUSE, metrics, systemd. Lib surface exposed for integration tests (see `tcfsd::daemon::test_support`). |
| `tcfs-cli` | lib+bin | CLI: push, pull, mount, device, status, unsync. Lib surface exposes ordering-sensitive command helpers for integration tests. |
| `tcfs-tui` | bin | Terminal UI: ratatui 5-tab dashboard |
| `tcfs-mcp` | bin | MCP server: 7 non-resolution tools, rmcp 0.16, stdio transport |
| `tests/e2e` | test | End-to-end integration test crate |

## Key Patterns

- **Proto source of truth**: `crates/tcfs-core/src/proto/tcfs.proto` — all crates import via `tcfs_core::proto`
- **Error handling**: `thiserror` for libraries, `anyhow` for binaries
- **Async**: tokio full features, `tracing` for structured logging
- **State cache**: JSON-backed at `{config.sync.state_db}.json`
- **CAS layout**: chunks at `{prefix}/chunks/{hash}`, manifests at `{prefix}/manifests/{file_hash}`
- **Feature gates**: `fuse` feature on tcfs-cli (default on), `nats` feature on tcfs-sync

## Testing

```bash
# All tests
cargo test --workspace

# Specific crate
cargo test -p tcfs-sync

# Feature-gated lane CI also runs
cargo test -p tcfs-sync --features nats,crypto

# Property-based tests
cargo test -p tcfs-sync -- conflict
cargo test -p tcfs-sync --test multi_machine_sim

# With output
cargo test -- --nocapture
```

Shell-harness and proof lanes are `task` recipes, not cargo tests — see
`task --list` (for example `task lazy:check`, `task lazy:dev-env-fingerprint`).
Prefer the `lazy:test-*` regression recipes when changing a harness script.

## CI

Always-on lanes (every PR):

- `ci.yml`: `check` (`cargo check --workspace --locked` first — a dependency
  edit that leaves `Cargo.lock` stale fails here before anything else — then
  fmt, FileProvider surface contract, clippy, `cargo test`, feature-isolated
  and wire-up tests, then workspace/`k8s-worker`/no-FUSE builds), plus
  `cloudfilter-windows`, `nix`, `fileprovider-staticlib`, `ios-typecheck`,
  `deny` (cargo-deny), and `secret-scan` (gitleaks).
- `nix-ci.yml`: `flake-check`, `build-linux-x86_64`, `build-macos-aarch64`.
- `docs.yml`: `check-links` (lychee), `build-pdf` (tectonic), `deploy` (GitHub
  Pages). The repo's only pre-commit hook is the same lychee check, so a broken
  relative link in a Markdown edit fails locally and in CI.

Path-scoped lanes that also fire on a PR — check `.github/workflows/` rather
than assuming the list above is complete:

- `ci-live-storage.yml`: real-storage lane on `crates/**`, `tests/**`,
  `Cargo.toml`, `Cargo.lock`, `docker-compose.yml`, `config/**`. It exists
  because `ci.yml` can stay green while a sync path is broken, so a code PR
  that passes `ci.yml` is not yet proven.
- `linux-package-container-smoke.yml`: on `scripts/install-smoke.sh` and its
  own workflow/test files.

Release: `release.yml` has 9 jobs — `plan`, `build-binaries`, `build-image`,
`nix-build`, `generate-installers`, `build-fileprovider`, `build-pkg`,
`create-release`, `update-homebrew`. The remaining workflows are
`workflow_dispatch`-only.

## Agent Coordination

Ground rules when multiple agents (Claude, Codex, or other) touch this repo
concurrently. Adapted from the GFTB multi-agent orchestration pattern
(`site.scaffold` `docs/patterns/multi-agent-orchestration.md` §3, commit
`36c14ae`) and lab's durable-notes rule (`lab` commit `b48d46f7`, `TIN-2520`).

- **Shared-PR lane claims — the `#534` lesson.** Claude-driven feature
  branches live on the personal remote (`origin` = `Jesssullivan/tummycrypt`);
  `codex/sync-origin-main-*` branches/PRs on the org mirror (`tinyland` =
  `tinyland-inc/tummycrypt`) are codex-owned reconciliation lanes — don't
  hand-edit or merge another agent's lane without diffing against the history
  it reconciles.
- **DO-NOT-TOUCH lists.** When a lane is in flight, its PR body should carry
  a short list of the crates it owns (e.g. "this PR owns `crates/tcfs-sync`
  — coordinate before stacking commits").
- **Single merge authority per PR.** One agent (or the operator) merges.
  Other agents report findings as PR comments — never push fixes onto a
  branch you don't own without an explicit handoff. `#534` was churned by
  two agents stacking unverified hardening commits on an already-reviewed
  clean head; each "fix round" cited a fresh adversarial pass but none
  re-ran a local build/test. Verify before stacking; don't self-certify onto
  someone else's lane.
- **`README.md` and `AGENTS.md` are live roam fixtures.** `docs/ops/current.md`
  tracks deliberate user-content conflicts on these two paths for the TIN-2658
  production resolver gate. Editing them in a PR is fine; do not "resolve" the
  live host-side divergence as a side effect of a docs change.
- **Durable notes over scratchpad.** Findings that matter beyond the current
  session go in dated files under `docs/ops/` (the existing ~30-file
  convention), never only in an ephemeral scratchpad or chat context a
  compaction/rotation sweep can wipe. This repo rule outranks a harness
  default that says to park working files in `/tmp` or a scratchpad dir —
  that default covers genuinely transient scratch only (lab's `TIN-2520`
  clause is the canonical statement of this precedence).
- **Instruction precedence.** Repo-root `AGENTS.md` (this file) > the
  nearest in-repo `.claude/CLAUDE.md` overlay > named `docs/ops/` facet docs
  referenced from here or a task > machine-level / home-manager defaults.
