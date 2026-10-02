# 2026-10-01: sting disk, provisioning and bulkload completeness audit

**Seat:** sting, Claude Code session `neo:f1c4ead8` (copied by hand from neo).
**Plan:** `~/.claude/plans/cryptic-napping-quiche.md`.
**Rulings:** R-N13. Operator interview 2026-10-01 ratified:
- all four phases;
- codex history compressed in place to `.jsonl.zst`;
- TMPDIR moves to `/srv/scratch` and dev caches to `/srv/cache`;
- bulkload first, with disk operations alongside.

## Why

`/srv/fast-local` on sting (1.6T xfs on nvme1) ran out of space on 2026-10-01.
The operator deleted roughly 400–600G by hand to recover, including two large
archives. tinyland-cleanup sat at "critical" for about 19 hours and recovered
only about 24G itself.

## Findings

### Biggest consumers on `/srv/fast-local` (bulkload is not the main one)

| Consumer | Size |
|---|---|
| codex rollouts, `state/codex` (live `~/.codex`) | 317G |
| `git/` | 190G |
| `pre-boundary-20260825` | 55G |
| `cache/` | 47G |
| gf-reapi-cell-store PVC (requests 50Gi) | 80G |
| `tmp/` (TMPDIR; about 11k nix-shell and nix-develop dirs) | 27G |
| bulkload cohort corpora and state | about 27G |

### Provisioning gaps, intended vs. actual

The intent is lab `vars/linux_storage_contract.yml` at deployed revision
`9bacba5b`.

- **fast-local.** Declared as "recoverable scratch/cache PVC backing". In
  practice it holds about 874G of human dev state. It also shares nvme1 with the
  etcd-db and kubelet LVs, even though the contract says "separate device".
- **tinyland-cleanup.** The temp-root budget of 4096 is shared across scan paths
  (`plugins/devartifacts.go:286`). `/tmp` uses it all up, so
  `/srv/fast-local/jess/tmp` is never scanned. The RKE2 plugin is a disabled
  placeholder.
- **local-path volumes.**
  - No reaper for released PVs or orphaned `pvc-*` dirs.
  - gf-reapi janitor and reconciler suspended for 82 days.
  - No per-StorageClass quota.
- **Idle tiers.** `/srv/cache` (393G free) and `/srv/scratch` (76G free) have
  no declared tenants.

### Integrity checks (Phase 0)

- **neo-transfer archive.** The operator's deletes removed
  `fast-local/.../neo-transfer-backup-20260930T164001Z/source-fs`. That left
  4931 transcript symlinks dangling in
  `/srv/data/jess/neo-transfer-20260930T164001Z/neo-fs`. All 4931 session
  UUIDs are still present in sting `state/codex`, so no transcript was lost.
  The archive links need repointing, or `report.json` needs a superseding
  note.
- **Bulkload cohort1.** The apply exited 1 with `CONTRACT_SELF_INCONSISTENT`
  after its imports.
  - 1232 of 1240 neo ref tips are on sting.
  - The 8 missing ones (`crs310-8g-2s-in`: `main` plus 7 branches) are all on
    GitHub `xoxd-ai/crs310-8g-2s-in`.
  - The sting copy has no remote.
  - No loss.
- **Worktree manifest.** The manifest covers 1197 checkouts:

  | State | Count |
  |---|---|
  | Uncommitted changes | 335 |
  | With stashes | 120 |
  | Commits on no remote | 499 |
  | Clean and pushed `*.worktrees` | 208 (about 47G) |

  The 208 are the only prune candidates.
- **Stray repo at `~/git`.** `/srv/fast-local/jess/git/.git` is an empty
  repository, created 2026-09-23. Its HEAD was set to
  `work/tin-5184-sol-inference-client` on 09-30. Any git command run from a
  subdirectory of `~/git` that has no repository of its own resolves to it.
  It is flagged only; nothing was changed.

Evidence is in `/srv/data/jess/archive/sting-disk-audit-20261001/`:
`wt-manifest.tsv`, `c1-missing.tsv`, `broken.txt`, and the scripts.

## Actions taken (R-N13 receipts)

- **Codex rollouts compressed in place.** Scope: `.jsonl` untouched for more
  than 14 days, 9546 files and 272G. This follows the operator ruling of
  2026-10-01, and lab `scripts/migrations/neo-codex-state.py:50` treats
  `.jsonl.zst` as codex-native. For each file, the script:
  1. runs `zstd -t`;
  2. checks that the sha256 of the decompressed stream equals the original;
  3. preserves the mtime;
  4. renames the original into
     `/srv/fast-local/jess/.quarantine/codex-jsonl-20261001/` and records it in
     `manifest.tsv`.

  Nothing is deleted. The operator removes the quarantine when satisfied.
  Script: `codex-zst.sh` (in the evidence folder).
- **No pruning, moves or provisioning changes yet.**

### Results (2026-10-02 UTC)

- **Codex compression.** 7359 originals were compressed and quarantined. An
  independent verify pass, `codex-zst-verify.sh`, recomputed sha256 of each
  original against the decompressed published `.zst`. Result: **7359 OK, 0
  mismatch, 0 missing**. That is 165.5 GB of originals against 14.8 GB of zst;
  the quarantine takes 155G on disk.
  - **Deviation:** the parallel run's manifest append was broken (a misuse of
    `flock -c`), so `manifest.tsv` holds only the first 903 rows. The
    authoritative record is `.quarantine/codex-jsonl-20261001/verify.tsv`. The
    script is fixed for future runs.
  - **Skipped:** 2187 `.jsonl` files already had a `.jsonl.zst` sibling from
    earlier history. They were left untouched and are being checked for
    byte-identity.
- **Worktrees.** 157 clean, pushed worktrees were removed with `git worktree
  remove`, about 21G. 51 were skipped as touched within the last 48h. The log
  is `wt-prune.log`.
- **Stray `~/git/.git`.** It was moved to
  `.quarantine/git-root-dotgit-20261001/`.
- **Cold trees.** `pre-boundary-20260825`, `rollback` and `TCFS Pilot` are
  being copied to `/srv/data/jess/archive/fast-local-cold-20261001/`. Each copy
  is checked with `diff -rq` before its source moves to quarantine.
- **Quarantine deletion** (operator ruling: delete right after verification).
  The operator runs the delete. Nothing in this session deletes data.
- **PRs opened:**
  - lab: xoxd-ai/lab#2043 (TMPDIR to `/srv/scratch`, storage contract).
  - tinyland-cleanup: Jesssullivan/tinyland-cleanup#131 (temp budget per scan
    path).
  - blahaj: xoxd-ai/blahaj#1766 (local-path orphan reaper, dry-run by default).
  - GloriousFlywheel: xoxd-ai/GloriousFlywheel#2081 (gf-reapi janitor
    unsuspended, CAS capped at 40Gi).
  - bulkload:
    - #80 / issue #79: closure report and space preflight. The closure report
      flags cohort2a: `lab-wt/tin-4287-adapter` has no outcome.
    - #76: merged as `8a3d922`.
    - #75: round-2 findings N1–N5 fixed in `13344e1`; round-3 review in
      progress.
    - #77: wire v5 agent migration in progress.
- **Signing.** Under operator ruling OI-1001-Q6, bulkload commits on sting are
  signed with `C613B…`. Sting's bulkload `.git/config` now carries that key.

### Results (2026-10-02, continued)

- **Cold trees.** `rollback` (11G), `TCFS Pilot` (8.1G) and
  `pre-boundary-20260825` (54G) were copied to
  `/srv/data/jess/archive/fast-local-cold-20261001/` and checked with `diff -rq`.
  The sources are in `.quarantine/moved-to-srv-data-20261001`.
  - **Deviation:** `cp -a` gave two sparse qutebrowser `places.sqlite` fixtures
    a bogus apparent size of about 17 TB and 54 TB. `--sparse=never` fixed one
    copy but not the other. Both were re-copied as byte streams, and both now
    `cmp` identical to the source.
- **Codex rollouts that already had a `.zst` sibling** (2187 files):
  - 987 were byte-identical. Their `.jsonl` copies were moved to
    `.quarantine/codex-jsonl-dup-20261001` (54G).
  - 602 were strict prefix extensions of the `.zst`. They were recompressed,
    with the sha256 of the decompressed stream checked against the `.jsonl`.
    The stale `.zst` and the `.jsonl` went to
    `.quarantine/codex-jsonl-extend-20261001` (65G). Result: 602 OK, 0 failed.
  - 598 had diverged, probably because the first line's cwd was remapped. Both
    copies were kept.
- **Old gf-reapi store.** Spot check: the live cell mounts
  `gf-reapi-cell-store-recovery-20260718`, and no pod mounts the old
  `gf-reapi-cell-store` PVC (80G). Per the operator ruling it is to be
  discarded, and the operator runs the delete.
- **Codex recovery trees from neo** (lab ruling R-C122). They are at
  `/srv/fast-local/jess/state/neo-recovery-relocated` (112G), with `EXPIRES`
  2026-11-02, `MANIFEST.sha256` and a README. Operator ruling 2026-10-02: leave
  them on fast-local until they expire. The operator deletes them on or after
  2026-11-02.
- **Bulkload:**
  - #75 merged as `bc5e13c` after six adversarial rounds plus a review of the
    merge-of-main commit.
  - #77 (wire v5) is in round 3.
  - #80 (closure report, space preflight), #81 (test tiers) and #96 (cohort3
    note) are open.
  - Follow-ups: #82–#85, #89–#95.
- **Cohort closure:**
  - cohort1 and cohort2b pass.
  - cohort2a passes: 147 applied, 22 refused, 0 unaccounted. item7955 was
    applied after neo's lab `info/exclude` was adopted byte-identically.
  - cohort3 passes in the closure ledger: 71 referenced-only, 2 refused, 0
    unaccounted. Natively it still shows 73 unaccounted, pending #95. The three
    infra repos were repaired, plain-git index fixes were applied on sting, and
    chapel's 5 missing files were restored.
  - cohort4 (215 items, never pulled) is deferred to the M2 engine by operator
    ruling.
- **neo TinylandState** (98% full). This seat verified the following for the
  operator to delete:
  - `tummycrypt-litter-20260921`: the R23 evidence is recorded in bulkload
    `docs/evidence`.
  - `cargo-target-bulkload-*`: no open files.
  - `bulkload-m0/w3`: their `logs/` were copied to
    `/srv/data/jess/archive/bulkload-evidence/` and sha-verified.

  The lab seat runs the codex compression and the recovery-tree move
  (TIN-3342).

## Open, not yet ruled

- Prune the 208 clean, pushed worktrees.
- Quarantine the stray `~/git/.git`.
- Repoint the neo-transfer archive links.
- Bulkload M2 is paused under R-N133:
  - #75 has a round-1 fix (`ad52be7`) and needs a round-2 adversarial review;
  - #76 is a docs note, green;
  - #77 is a draft with failing fault-harness and source gates.
- Provisioning PRs:
  - lab `sting.nix`: TMPDIR and caches;
  - lab storage contract;
  - tinyland-cleanup: per-path budget;
  - GF/blahaj: orphan reaper and janitor.
