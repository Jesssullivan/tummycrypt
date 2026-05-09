# Tinyland Branch Prune Proposal - 2026-05-09

This document records the non-destructive audit of the `tinyland` remote branch
tranche after origin PR #350 merged. It is a proposal only: no remote branches
were deleted during this pass.

## Baseline

| Item | Value |
|------|-------|
| Canonical ref | `origin/main` at `fd121112883c` |
| Downstream ref | `tinyland/main` at `987a6b4c3900` |
| Retired cached ref | `yoga/main` at `b8333ddf2988` |
| `tinyland/main` divergence | 21 commits ahead / 140 commits behind `origin/main` |
| `yoga/main` cached divergence | 137 commits ahead / 471 commits behind `origin/main` |
| `tinyland` branch count | 65 actual remote branches, excluding symbolic `tinyland/HEAD` |

Branch mix on `tinyland` at audit time:

| Prefix | Count |
|--------|------:|
| `fix/*` | 41 |
| `feat/*` | 14 |
| `test/*` | 4 |
| `chore/*` | 3 |
| `refactor/*` | 1 |
| `homebrew-tap` | 1 |
| `main` | 1 |

## Method

The audit compared the `tinyland/*` topic branches against current
`origin/main` and `tinyland/main`. Raw `main..branch` commit counts are noisy
for old March branches because many carry disconnected or stale ancestry, so
the classification used a combination of:

- branch tip subjects and topic intent;
- patch-id checks where practical;
- current source and test coverage checks on `origin/main`;
- existing release and operations docs that name proven versus unproven lanes.

The local pass fetched `tinyland` with pruning to refresh remote-tracking refs.
No remote branch deletion command was run.

## Recommended Action

Use a two-step cleanup. First remove the obvious superseded fix/chore backlog
after operator approval. Then do a short human subject-matter review of the
feature/test tranche before deleting those branches.

Do not delete `main`, `homebrew-tap`, or the two architecture-sensitive hold
branches listed below.

## Tranche A: Fix And Chore Candidates

These 44 branches are recommended for deletion after explicit operator approval.
They are either patch-equivalent to current `origin/main`, represented by later
mainline PRs, or obsolete build/bookkeeping branches.

```text
tinyland/chore/bump-build-5
tinyland/chore/bump-v0.9.0
tinyland/chore/repo-hygiene
tinyland/fix/auto-pull-index-first
tinyland/fix/aws-credential-order
tinyland/fix/cargo-fmt
tinyland/fix/cargo-lock-blake3
tinyland/fix/cargo-lock-fuse3
tinyland/fix/cbindgen-enumerate-changes
tinyland/fix/chunk-upload-retry
tinyland/fix/cli-unwraps
tinyland/fix/conflict-clearing
tinyland/fix/daemon-state-races
tinyland/fix/disable-stale-attic-cache
tinyland/fix/empty-dir-sync
tinyland/fix/fileprovider-hydration-deadlock
tinyland/fix/fileprovider-subdirectory-enumerate
tinyland/fix/fuse-flush-and-errno
tinyland/fix/grpc-path-traversal
tinyland/fix/hydrate-json-manifest
tinyland/fix/in-process-nfs-mount
tinyland/fix/ios-ats-http
tinyland/fix/ios-swift-compile-error
tinyland/fix/linux-nfs-sudo
tinyland/fix/lockfile-serde-json
tinyland/fix/manifest-retry
tinyland/fix/migrate-serde-yml-to-norway
tinyland/fix/nats-durability
tinyland/fix/nats-manifest-path-prefix
tinyland/fix/nats-stream-update
tinyland/fix/nfs-exit-diagnostics
tinyland/fix/nfs-panic-detection
tinyland/fix/nfs-vfs-timeout
tinyland/fix/orphaned-index-entries
tinyland/fix/p0-sync-engine
tinyland/fix/pull-absolute-path-122
tinyland/fix/remove-stale-fuse-t-feature
tinyland/fix/resource-bounds
tinyland/fix/security-hardening
tinyland/fix/skip-directory-push
tinyland/fix/statecache-flush
tinyland/fix/sync-panic-safety
tinyland/fix/version-bump-cache-bust
tinyland/fix/vfs-hydration-logging
```

Representative grounding:

| Branch group | Current mainline evidence |
|--------------|---------------------------|
| FUSE/VFS fix branches | Current `tcfs-vfs` carries read/write FUSE paths, flush handling, vector-clock conflict detection, transparent `.tc` suffix behavior, directory ops, timeout handling, and hydration logging/proof docs. |
| Sync/state fix branches | Current sync engine carries delete reconciliation, empty-directory markers, streaming/FastCDC chunking, path normalization, conflict clearing, retry/backoff, and panic-safety/property tests. |
| NATS and daemon fix branches | Current daemon/VFS code carries state-cache persistence, active-file race handling, NATS stream update/durability, NATS publish wiring, and gRPC path sanitization. |
| FileProvider/iOS fix branches | Current Swift surfaces carry FileProvider testing-mode proof paths, subdirectory enumeration, placeholder hydration behavior, iOS QR enrollment, ATS posture, and generated Swift bindings. |
| Bookkeeping branches | Version, lockfile, formatting, cache, and historical build bumps are behind the current v0.12.x release line and current CI/release evidence. |

## Tranche B: Feature And Test Candidates

These 17 branches also appear superseded, but they should get a final
human subject-matter pass because they name user-visible feature or coverage
lanes. The audit found no unrecovered code that requires preserving the branch
refs.

```text
tinyland/feat/bootstrap-qr-encryption
tinyland/feat/compact-qr-enrollment
tinyland/feat/fastcdc-flush
tinyland/feat/fuse3-mount
tinyland/feat/ios-qr-enrollment
tinyland/feat/menubar-status
tinyland/feat/nats-fuse-flush
tinyland/feat/read-write-fuse
tinyland/feat/streaming-chunker
tinyland/feat/symlink-handling
tinyland/feat/transparent-tc-suffix
tinyland/feat/vfs-directory-ops
tinyland/feat/vfs-vclock-integration
tinyland/test/daemon-coverage
tinyland/test/engine-unit-tests
tinyland/test/fileprovider-functional
tinyland/test/fuzz-targets
```

Representative grounding:

| Branch | Current mainline evidence |
|--------|---------------------------|
| `feat/bootstrap-qr-encryption` | Current `tcfs-auth` enrollment code carries signed, expiring invites, compact encoding, `tcfs://enroll` links, storage credential fields, and encryption material. Keep a short human review because the old branch had several patch-unique commits. |
| `feat/compact-qr-enrollment` | Current enrollment code and generated Swift APIs include compact invite encoding. |
| `feat/fastcdc-flush` and `feat/streaming-chunker` | Current sync upload path uses streaming chunking/FastCDC and has tests around large-file paths. |
| `feat/fuse3-mount` | Current Linux lane keeps FUSE active and documented; NFS remains a fallback proof lane. |
| `feat/ios-qr-enrollment` | Current iOS host app includes QR enrollment view and bootstrap QR scripting, while iOS is still documented as experimental. |
| `feat/menubar-status` | Current mainline includes `TCFSStatus.app` conflict/status monitor work. |
| `feat/nats-fuse-flush` | Current VFS flush path and daemon gRPC/NATS wiring cover mounted-write publish behavior. |
| `feat/read-write-fuse`, `feat/transparent-tc-suffix`, `feat/vfs-directory-ops`, `feat/vfs-vclock-integration` | Current VFS tests and implementation cover mounted read/write, suffix hiding, directory operations, and vector-clock conflict metadata. |
| `feat/symlink-handling` | Current sync tests cover symlink handling and cycle detection. |
| `test/*` branches | Current mainline includes daemon/gRPC tests, sync/engine/NATS/reconcile tests, FileProvider memory-backed functional tests, proptests, and fuzz targets. |

## Hold Branches

Keep these branches until a human owner explicitly resolves their architectural
intent:

| Branch | Reason to hold |
|--------|----------------|
| `tinyland/feat/fuse-free-vfs-nfs` | Carries a broad FUSE-free/NFS loopback direction plus authentication/session work. Some code paths are represented today, but the architectural direction conflicts with the current Linux FUSE proof posture. |
| `tinyland/refactor/retire-fuse-crates` | Names FUSE retirement directly. Current docs and code still treat Linux FUSE as the primary mounted Linux lifecycle surface, so this should not be pruned without a product decision. |

Keep these non-topic branches:

```text
tinyland/main
tinyland/homebrew-tap
```

## Deletion Commands Not Run

After operator approval, Tranche A can be deleted with:

```bash
git push tinyland --delete \
  chore/bump-build-5 \
  chore/bump-v0.9.0 \
  chore/repo-hygiene \
  fix/auto-pull-index-first \
  fix/aws-credential-order \
  fix/cargo-fmt \
  fix/cargo-lock-blake3 \
  fix/cargo-lock-fuse3 \
  fix/cbindgen-enumerate-changes \
  fix/chunk-upload-retry \
  fix/cli-unwraps \
  fix/conflict-clearing \
  fix/daemon-state-races \
  fix/disable-stale-attic-cache \
  fix/empty-dir-sync \
  fix/fileprovider-hydration-deadlock \
  fix/fileprovider-subdirectory-enumerate \
  fix/fuse-flush-and-errno \
  fix/grpc-path-traversal \
  fix/hydrate-json-manifest \
  fix/in-process-nfs-mount \
  fix/ios-ats-http \
  fix/ios-swift-compile-error \
  fix/linux-nfs-sudo \
  fix/lockfile-serde-json \
  fix/manifest-retry \
  fix/migrate-serde-yml-to-norway \
  fix/nats-durability \
  fix/nats-manifest-path-prefix \
  fix/nats-stream-update \
  fix/nfs-exit-diagnostics \
  fix/nfs-panic-detection \
  fix/nfs-vfs-timeout \
  fix/orphaned-index-entries \
  fix/p0-sync-engine \
  fix/pull-absolute-path-122 \
  fix/remove-stale-fuse-t-feature \
  fix/resource-bounds \
  fix/security-hardening \
  fix/skip-directory-push \
  fix/statecache-flush \
  fix/sync-panic-safety \
  fix/version-bump-cache-bust \
  fix/vfs-hydration-logging
```

After the subject-matter pass, Tranche B can be deleted with:

```bash
git push tinyland --delete \
  feat/bootstrap-qr-encryption \
  feat/compact-qr-enrollment \
  feat/fastcdc-flush \
  feat/fuse3-mount \
  feat/ios-qr-enrollment \
  feat/menubar-status \
  feat/nats-fuse-flush \
  feat/read-write-fuse \
  feat/streaming-chunker \
  feat/symlink-handling \
  feat/transparent-tc-suffix \
  feat/vfs-directory-ops \
  feat/vfs-vclock-integration \
  test/daemon-coverage \
  test/engine-unit-tests \
  test/fileprovider-functional \
  test/fuzz-targets
```

Post-delete verification:

```bash
git fetch tinyland --prune
git branch -r --list 'tinyland/*'
git rev-list --left-right --count origin/main...tinyland/main
```

Expected result after Tranche A only: the `tinyland` remote branch count drops
from 65 to 21, while `tinyland/main` remains 21 ahead / 140 behind
`origin/main`.

Expected result after Tranche A and B: the `tinyland` remote branch count drops
from 65 to 4, leaving only:

```text
tinyland/main
tinyland/homebrew-tap
tinyland/feat/fuse-free-vfs-nfs
tinyland/refactor/retire-fuse-crates
```

## Tracker Disposition

- #312 can close once this proposal is merged and an operator either approves
  or explicitly defers the Tranche A deletion.
- #313 can close on the documentation-only retirement decision already recorded
  in [Remote Governance](remote-governance.md). No `yoga` archive, SSH host
  deletion, key revocation, or local remote removal is part of this branch
  hygiene lane.
