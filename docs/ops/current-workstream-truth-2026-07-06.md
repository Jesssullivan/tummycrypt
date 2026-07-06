# Current TCFS Workstream Truth, 2026-07-06

This note is the current operator-facing checkpoint for TCFS daily-driver work.
Older evidence packets remain useful provenance, but do not treat them as the
active blocker list.

## Build Substrate

Do not use `neo` for heavy local builds. If a change needs expensive Rust,
Nix, or Darwin validation, use remote CI or the repo/fleet build substrate.

`petting-zoo-mini` may be useful as a bounded Darwin lab endpoint, but it is
not the durable answer to `neo` build pressure. The durable direction is the
GloriousFlywheel/RBE/Darwin substrate lane. Nix offload to PZM is tactical, must
fail loud when unavailable, and is currently not accepted for TCFS deploy work
until the lab verifier says it is healthy again.

## Keep-Both / Repo Roam

The fast-forward `.git` case is closed by #513 and live evidence. The divergent
keep-both ladder is split:

- PR-1 through PR-3 are merged: per-file `.git` resolve is fenced, the executor
  respects foreign `.git/tcfs.lock`, `tcfs conflicts` exists, and the
  operator-only repo keep-both resolver parks the peer side under
  `refs/tcfs/theirs/**`.
- PR-4 (loser-side no-loss guard) is merged: #534 / commit 4c61da4, TIN-2552,
  merged 2026-07-06. Before a ref pull overwrites a divergent local branch,
  TCFS parks the old local head under `refs/tcfs/theirs/**` and keeps an undo
  bundle in the machine-local state dir; the final overwrite is CAS-protected
  via `git update-ref`. A 4-lens post-merge adversarial audit of the final
  delta passed (no correctness defects; tests strengthened; three minor
  follow-ups tracked in Linear).

PR-4 is merged but NOT yet deployed (fleet runs v0.12.16, which predates it)
and NOT live-canary proven. Until a post-4c61da4 build is deployed and the
divergent canary passes (runbook:
`docs/release/evidence/divergent-keep-both-canary-PLAN.md`), do not claim the
divergent two-machine G5-git-13/T10/T11 convergence row green.

## PZM / TCC / SSD

The PZM external SSD can be visible and browsable at the block/APFS/Finder
layer, but that is not enough for TCFS offload. Current lab probes show SSH
child enumeration still times out under `/nix` and
`/Volumes/TinylandSSD/tinyland`, and denial logs show System Policy/FDA
decisions involving shell and Nix-store paths. Finder access is useful evidence,
but it does not prove launchd, SSH, Nix/dyld, runner, daemon, or remote-builder
contexts.

Before any TCFS deploy uses PZM again, lab must pass the read-only recovery
gate: directory-health, Nix execution-context probes, denial-log check, and
`just nix-remote-builder-verify --live --strict`. Keep `TIN-2521`
become-password rotation separate and required.

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
