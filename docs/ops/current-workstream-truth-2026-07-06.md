# Current TCFS Workstream Truth, 2026-07-06

This note is the current operator-facing checkpoint for TCFS daily-driver work.
Older evidence packets remain useful provenance, but do not treat them as the
active blocker list.

## Build Substrate

Do not use `neo` for heavy local builds. If a change needs expensive Rust,
Nix, or Darwin validation, use remote CI or the repo/fleet build substrate.

`petting-zoo-mini` may be useful as a bounded Darwin lab endpoint, but it is
not the durable answer to `neo` build pressure. The durable direction is the
GloriousFlywheel/RBE/Darwin substrate lane. Nix offload to PZM is tactical and
must fail loud when unavailable.

## Keep-Both / Repo Roam

The fast-forward `.git` case is closed by #513 and live evidence. The divergent
keep-both ladder is split:

- PR-1 through PR-3 are merged: per-file `.git` resolve is fenced, the executor
  respects foreign `.git/tcfs.lock`, `tcfs conflicts` exists, and the
  operator-only repo keep-both resolver parks the peer side under
  `refs/tcfs/theirs/**`.
- PR-4 is the active loser-side no-loss guard work: before a ref pull can
  overwrite a divergent local branch, TCFS must park the old local head and keep
  an undo bundle in the machine-local state dir. This is tracked by TIN-2552
  and PR #534.

Until PR-4 is merged, deployed, and live-canary proven, do not claim the
divergent two-machine G5-git-13/T10/T11 convergence row green.

## PZM / TCC / SSD

The PZM external SSD is not the current proof blocker when directory-health and
transport checks are green. Recent observed healthy shape:

- TinylandSSD and `/nix` are APFS volumes on the external SSD container.
- Directory traversal over SSH completes for `/Volumes/TinylandSSD` and the
  Tinyland-owned subtree.
- The ASM236X enclosure enumerates at 10Gbps.

The remaining PZM risk is context-specific macOS policy, not basic SSD
enumeration: TCC/PPPC/FDA, launchd execution, Nix/profile/dyld reads, code
identity, and the exposed become password rotation. Finder access is useful
evidence, but it does not prove launchd, SSH, or Nix-store dynamic execution
contexts.

## Per-Device Crypto

Do not resurrect #517 as a merge candidate. It is closed, stale provenance for
the fresh `tcfs key rotate <prefix>` rebuild. The active direction is TIN-2551
under TIN-1417: per-prefix FileKey rotation, real revocation semantics, and
the staged `master -> dual -> per_device` live proof path.

## Documentation Hygiene

Archived packets may mention source-built binaries on `neo`, old Finder
read-timeout blockers, or PZM hardware-gated conclusions. Treat those as dated
evidence unless a current runbook explicitly promotes them. Current docs should
point here or to the relevant Linear issue before making new daily-driver
claims.
