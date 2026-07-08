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

PR-4 is deployed (daemons post-#534 `4c61da4`) and **LIVE-PROVEN 2026-07-08**.
A two-host neo ⇄ honey divergent keep-both canary converged both hosts with no
committed work lost: the loser-side no-loss guard fired in production, parked
honey's head at `refs/tcfs/theirs/<device>/heads/main`, wrote a verified undo
bundle, roamed the parked ref to neo, and recorded zero conflicts on the next
cycle both sides. **G5-git-13 divergent two-machine convergence is PROVEN live
(loser-guard path); G5-git-5 is CLOSED end-to-end** — FF half 2026-07-05
(`docs/release/evidence/bidirectional-ff-canary-20260705T225429Z/RESULTS.md`) +
divergent half 2026-07-08
(`docs/release/evidence/divergent-keep-both-canary-20260707T071335Z/RESULTS.md`).

The run drove out four product defects before it could pass — this ledger is
part of the truth:

- **TIN-2584 — FIXED (#540):** the first divergent reconcile silently absorbed
  the divergence (out-of-band commit never ticks the vclock; the dominated clock
  is structurally conflict-unreachable; the LIST race returns `UpToDate`). Fix
  proven live — honey then recorded the 5-conflict repo group.
- **TIN-2652 — FIXED (#541):** plan-path conflicts were recorded with
  `status=synced`, invisible to the resolver. Fix proven at the state layer —
  the five conflict-entry statuses flipped `synced → conflict`.
- **TIN-2653 — OPEN:** headless session token is write-only (TOTP provenance
  unusable over ssh). Resolve was exercised via the repo-precedent
  `require_session=false` bypass window, re-locked immediately after.
- **TIN-2657 — OPEN:** the daemon remaps `sync.state_db` → `state.json`
  (`crates/tcfs-daemon/src/daemon.rs:315`), so the CLI and daemon act on
  different files — this is why the operator resolve VERB returned 0 refs even
  post-#541.

The convergence proof is via the **automatic loser-guard**; the **operator
resolve VERB** (`tcfs resolve … --execute`) is honestly NOT yet claimed and is
blocked by the two open tickets above. Harness Stage 6 (G5-git-13) carries a
LIVE-PROVEN marker citing the evidence dir.

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
