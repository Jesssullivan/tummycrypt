# Operator Decision Record — 2026-07-01

Status: ratified operator decisions. Doc-only. No code lands with this
document; no fleet config flips with this document. These decisions were made
in the 2026-07-01 operator interview session (grounded in the summer 2026
multi-workflow audit) and are recorded here so that repo truth, agent
dispatch, and Linear sequencing all point at the same commitments. Where an
older doc, plan, ticket, or memory disagrees with this record, this record
wins until superseded by a later dated decision record.

Date: 2026-07-01.

## Decision 1 — PerDevice wrap mode is COMMITTED for 2026-08-31

`crypto.wrap_mode = per_device` is no longer a direction; it is a dated
commitment. The fleet reaches the CONTRACT phase of
[the per-device crypto migration plan](per-device-crypto-migration-2026-06-06.md)
by **2026-08-31**.

- **Dual is a waypoint, not the exit.** `wrap_mode = dual` (EXPAND) exists
  only to get every writer emitting both wraps safely. Sitting in `dual`
  past the commit date is a miss, not a partial win: `dual` still carries the
  master wrap, so it provides no revocation and no forward secrecy.
- **Pulled onto the critical path** (previously follow-up work, now
  blockers for the 2026-08-31 date):
  - **Ghost-device revocation** — removing a device from the recipient set
    must provably deny it newly written content; stale/ghost registry
    entries must be revocable without fleet-wide manual surgery.
  - **TIN-1423 update channel** — the fleet update/rollout channel, so the
    roll-call gate can actually be satisfied (every active device on a
    per-device-capable binary) instead of hand-upgraded.
  - **BIP-39 / `master.key` recovery validation** — recovery of the escrowed
    identity path must be exercised and proven before the master wrap is
    dropped, not after.
  - **Negative roll-call tests** — tests that prove the roll-call gate
    REFUSES to contract when a registered active device is missing, stale,
    or on an old binary. The gate's refusal path is release-blocking
    surface, not best-effort logging.
- **Zero W1 slip tolerance.** Work scheduled for the first week of this
  commitment does not slide; slips escalate to the operator immediately.

## Decision 2 — PR #513 adjudication protocol

PR [#513](https://github.com/Jesssullivan/tummycrypt/pull/513) (.git-aware
fast-forward conflict resolution, agent-drafted) does not merge on ordinary
review. It merges only through this fixed sequence:

1. **Fence first:** [#515](https://github.com/Jesssullivan/tummycrypt/pull/515)
   (never roam worktree gitfiles or `.git/worktrees/**`) lands as the merge
   gate. *(Merged 2026-07-01.)*
2. **Harness second:** [#506](https://github.com/Jesssullivan/tummycrypt/pull/506)
   (facet 6 `.git`-as-files conflict/corruption safety harness) lands so the
   adjudication has an executable safety net. *(Merged 2026-07-01.)*
3. **Agent adversarial review + expected-red flipflop canary:** agents run an
   adversarial review of #513 plus the flipflop canary, including the
   expected-red leg — the canary must be shown to FAIL when the protection is
   deliberately absent, so a green result is evidence rather than
   false comfort.
4. **One-page verdict packet:** the agents produce a single-page verdict
   (claim, evidence pointers, red/green legs, residual risk) — not a thread
   to re-litigate.
5. **Operator rubber-stamp:** the operator merges on the packet. The
   operator's role is sign-off on prepared evidence, not primary review.

## Decision 3 — Never enroll blahaj

`blahaj` is never enrolled as a tcfs device. It may continue to serve
adjacent infra roles (e.g. tailnet proxy placement noted in
[the on-prem storage migration doc](tcfs-onprem-storage-migration.md)), but it
does not join the device registry, does not receive per-device wraps, and
must not appear in roll-call expectations. Any plan or script that would
enroll it is wrong by definition.

## Decision 4 — Daily-driver dogfooding mandate: mount the sync root

Dogfooding means **the sync root is mounted and used on the daily driver**,
not merely that `tcfsd` is running. A daemon that reports green while no
mount is exercised is a **false-comfort signal**: it proves process health,
not product health, and it is exactly how regressions in hydration, conflict
handling, and stub behavior stay invisible.

- Green-but-idle daemon status must not be cited as dogfood evidence.
- Dogfood claims require the mounted sync root in real use (traversal,
  hydration, writes) on the operator's daily-driver host.
- Monitoring/status surfaces should treat "daemon up, zero mount activity"
  as a warning state for dogfood hosts, not a pass.

## Precedence

This record captures operator decisions of 2026-07-01 for tummycrypt/tcfs.
It does not restate the full cross-portfolio decision ledger; only the tcfs
items are recorded here. Later dated decision records supersede this one
item-by-item.
