# RFC 0001: Fleet Sync Integration for Lab Machines

**Status**: Draft
**Author**: xoxd
**Date**: 2026-02-22
**Branch**: `fleet/multi-machine-sync` (PR #18)
**Tracking**: Sprint 6

---

## Abstract

This RFC describes the integration plan for deploying tcfs multi-machine sync
across the tinyland lab fleet (xoxd-bates, yoga, petting-zoo-mini). It covers
the Nix module design, infrastructure prerequisites, rollout sequence, and
verification procedures.

## Motivation

Three alpha machines share overlapping repos in `~/git/` that drift between
machines. Files may be stale, duplicative, or have uncommitted work. tcfs
brings all machines into a uniform view where every machine sees every file
but only hydrates on demand.

Current pain points:

- Manual `rsync`/`scp` between machines for file sharing
- No awareness of which machine has the latest version
- No conflict detection for concurrent edits
- No audit trail of what changed and where

## Architecture

```
 xoxd-bates (macOS)       yoga (Rocky Linux)     petting-zoo-mini (macOS)
      |                        |                        |
      +--- push/pull ----------+---- push/pull ---------+
      |                        |                        |
      +--- NATS events -------NATS JetStream-----------+
                               |
                        SeaweedFS S3 (CAS)
                        nats.tcfs.svc.cluster.local
```

### Data Flow

1. **Write**: Machine writes file locally, ticks its VectorClock
2. **Push**: Chunks via FastCDC, uploads to SeaweedFS CAS, writes SyncManifest v2 (JSON)
3. **Publish**: `StateEvent::FileSynced` on NATS subject `STATE.{device_id}.file_synced`
4. **Subscribe**: Other machines receive event via per-device durable consumer
5. **Compare**: VectorClock comparison determines: LocalNewer / RemoteNewer / Conflict
6. **Pull**: If RemoteNewer, auto-fetch chunks and reassemble
7. **Conflict**: If concurrent, invoke ConflictResolver (auto/interactive/defer)

### Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Conflict detection | Vector clocks | Full distributed partial ordering; no central coordinator |
| Conflict resolution | Pluggable (auto/interactive/defer) | Auto for headless, interactive for workstations |
| Auto-resolve tie-break | Lexicographic device name | Deterministic, reproducible, no coordination needed |
| .git sync | Opt-in, bundle mode default | Git bundles are atomic; raw mode risks corruption |
| Manifest format | JSON v2 with v1 text fallback | Backward-compatible with pre-Sprint 6 data |
| State transport | NATS JetStream (not S3 polling) | Real-time, durable, fan-out, per-device cursors |
| Stream retention | Limits (7 days, file storage) | Survives restarts, bounded growth |

## Infrastructure Prerequisites

### Already Running (Civo K8s)

| Service | Endpoint | Namespace |
|---------|----------|-----------|
| SeaweedFS S3 | `seaweedfs.tcfs.svc.cluster.local:8333` | tcfs |
| NATS JetStream | `nats.tcfs.svc.cluster.local:4222` | tcfs |

### Required Before Rollout

1. **NATS accessible from lab machines**
   - Current: NATS is cluster-internal only
   - Options: (a) Civo NodePort/LoadBalancer, (b) WireGuard tunnel, (c) NATS leaf node on local network
   - Recommended: NATS leaf node on local network (lowest latency, works offline)

2. **SeaweedFS S3 accessible from lab machines**
   - Current: Accessible via `dees-appu-bearts:8333` on local network
   - Status: Already reachable from all three machines

3. **Device enrollment**
   - Each machine runs `tcfs device enroll --name $(hostname)` once
   - Registry stored in S3 at `tcfs-meta/devices.json`

## Nix Module Design

### Upstream (tummycrypt repo)

Located in `nix/modules/` within the tummycrypt flake:

- `tcfs-daemon.nix` — NixOS system-level service (`services.tcfsd.*`)
- `tcfs-user.nix` — Home Manager user-level service (`programs.tcfs.*`)

Both expose fleet options: `deviceName`, `conflictMode`, `syncGitDirs`,
`gitSyncMode`, `natsUrl`, `excludePatterns`.

### Downstream (crush-dots repo)

The existing `nix/home-manager/tummycrypt.nix` module needs extension:

```nix
# New fleet options under tinyland.tummycrypt
fleet = {
  enable = mkEnableOption "multi-machine fleet sync";
  conflictMode = mkOption { type = enum ["auto" "interactive" "defer"]; default = "auto"; };
  syncGitDirs = mkOption { type = bool; default = false; };
  gitSyncMode = mkOption { type = enum ["bundle" "raw"]; default = "bundle"; };
  natsUrl = mkOption { type = str; default = "nats://nats.tcfs.svc.cluster.local:4222"; };
  excludePatterns = mkOption { type = listOf str; default = ["*.swp" "*.swo" ".direnv"]; };
};
```

### Feature Flag (flags.nix)

Existing flag: `tinyland.host.tummycrypt.enable` (default: false)

New sub-flags needed:

```nix
tinyland.host.tummycrypt = {
  enable = mkOption { type = bool; default = false; };
  fleet = mkOption { type = bool; default = false; };  # NEW
};
```

### Per-Host Configuration

| Host | Platform | `tummycrypt.enable` | `fleet` | `conflictMode` | `syncGitDirs` |
|------|----------|---------------------|---------|----------------|---------------|
| xoxd-bates | macOS (aarch64) | true | true | auto | false |
| yoga | Linux (x86_64) | true | true | interactive | true (bundle) |
| petting-zoo-mini | macOS (aarch64) | true | true | auto | false |

## Rollout Plan

### Phase 1: Merge + Package (Day 0)

1. Merge PR #18 to `main` in tummycrypt
2. Tag `v0.3.0` release
3. Nix flake update in crush-dots: `nix flake update tummycrypt`
4. Verify `nix build .#tcfs-cli` succeeds on all platforms

### Phase 2: Enroll Devices (Day 1)

1. Deploy updated Home Manager config to all 3 machines
2. On each machine:
   ```bash
   tcfs device enroll --name $(hostname)
   tcfs device list  # verify all 3 visible
   ```
3. Verify S3 registry at `tcfs-meta/devices.json` shows 3 devices

### Phase 3: Smoke Test (Day 1)

1. On yoga:
   ```bash
   echo "fleet sync test" > /tmp/fleet-test.txt
   tcfs push /tmp/fleet-test.txt
   ```
2. On xoxd-bates:
   ```bash
   tcfs pull tcfs/default/fleet-test.txt /tmp/fleet-test.txt
   diff <(echo "fleet sync test") /tmp/fleet-test.txt
   ```
3. On petting-zoo-mini: same pull + verify

### Phase 4: Conflict Validation (Day 2)

1. On yoga: `echo "yoga version" > /tmp/conflict.txt && tcfs push /tmp/conflict.txt`
2. On xoxd-bates (before pulling): `echo "xoxd version" > /tmp/conflict.txt && tcfs push /tmp/conflict.txt`
3. Verify conflict detected (not silently overwritten)
4. Resolve via CLI: `tcfs resolve-conflict conflict.txt --keep-local`

### Phase 5: Git Repo Sync (Day 3, yoga only)

1. Enable `syncGitDirs = true` on yoga
2. Push a test repo: `tcfs push ~/git/test-repo`
3. Verify git bundle is created and round-trips
4. Pull on another machine and verify `git log` matches

### Phase 6: NATS Real-Time (Day 4+)

1. Establish NATS connectivity from lab machines to Civo cluster
2. Start tcfsd daemon on each machine
3. Modify file on one machine, verify auto-pull on others within seconds
4. Take one machine offline, modify files on others, bring back online, verify catch-up

## Verification Checklist

- [ ] PR #18 CI all green (Build + Lint + Test, Nix Build, Security Audit, cargo-deny)
- [ ] All 133 tests pass locally on each platform (linux-x86_64, darwin-aarch64)
- [ ] `tcfs device enroll` succeeds on all 3 machines
- [ ] Push → Pull round-trip byte-perfect
- [ ] Conflict detection works (concurrent writes detected, not overwritten)
- [ ] Auto-resolver picks lexicographic winner correctly
- [ ] Manifest v2 JSON written on push, v1 text still parseable
- [ ] VectorClock monotonicity holds across push/pull cycles
- [ ] Home Manager `nix switch` succeeds on all 3 machines after module update
- [ ] TUI shows Conflicts tab (key `5`)
- [ ] MCP `device_status` and `resolve_conflict` tools respond correctly

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| NATS unreachable from lab | Medium | Blocks Phase 6 | Sync works without NATS (manual push/pull) |
| Manifest v2 breaks old clients | Low | Data inaccessible | v1 fallback parser, tested in CI |
| VectorClock overflow (u64) | Negligible | Corruption | 2^64 ticks = centuries at 1M ticks/sec |
| Git bundle fails (dirty worktree) | Medium | .git sync skipped | `git_is_safe()` pre-checks, warnings logged |
| macOS FUSE unavailable | Low | Mount doesn't work | Push/pull/sync work without FUSE |

## Open Questions

1. **NATS access path**: NodePort, WireGuard, or leaf node? Leaf node recommended
   for offline resilience but requires running NATS on one lab machine.
2. **Credential distribution**: How to distribute SeaweedFS S3 credentials to all
   machines? Currently via `TCFS_*` env vars from sops-nix secrets. Need to add
   SeaweedFS creds to `nix/secrets/hosts/*.yaml` for each host.
3. **Automatic daemon startup**: Should tcfsd start automatically via systemd/launchd?
   Or on-demand via CLI? Recommend: systemd on yoga, launchd on macOS machines.

---

Signed-off-by: xoxd
