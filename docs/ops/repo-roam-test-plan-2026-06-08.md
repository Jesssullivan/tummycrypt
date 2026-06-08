# Repo-Roam Test Plan: dev-env zero-diff fingerprint over the TIN-1620 live-repo ladder

**Status:** runbook / canary plan (agent-drafted, needs operator review)
**Date:** 2026-06-08
**Gate:** G5 — one expendable live repo (`TIN-1620`); live-execution child `TIN-1908`
**Track:** repo-roam == Phase 2 "single live repo" of
[`large-workdir-onboarding-design-2026-05-25.md`](large-workdir-onboarding-design-2026-05-25.md)
**Hosts:** `macbook-neo` (Darwin), `honey` (Linux)

---

## 0. What this plan is, and what it is NOT

This is **not new scope**. Repo-roam — "enroll a `~/git` repo so a dev ssh-ing to
honey/bumble picks up committed + uncommitted + staged + untracked + branch/HEAD +
stash + agent sessions with zero dev-env difference, via the scheduled
`tcfs reconcile --path --prefix --execute` unit" — is the **extant** large-workdir
ladder:

- Phase 2 "single live repo" in
  [`large-workdir-onboarding-design-2026-05-25.md`](large-workdir-onboarding-design-2026-05-25.md)
  (Phased Rollout table; `### TIN-1620 One Expendable Live Repo Packet`, lines 445-481).
- **Gate G5** in
  [`large-workdir-daily-driver-sequencing-2026-05-30.md`](large-workdir-daily-driver-sequencing-2026-05-30.md)
  (Gate Model, line 127: *"TIN-1620 acceptance: two-machine
  browse/hydrate/unsync/rehydrate + conflict (T10/T11) + rollback/fresh-tree
  restore."*).
- The **enrollment vehicle** is the already-shipped scheduled-reconcile unit from
  [`claude-projects-roam-enrollment-2026-06-08.md`](claude-projects-roam-enrollment-2026-06-08.md)
  (Gate G4 / `TIN-1738`): the `extraReconcileRoots` launchd/systemd unit running
  `tcfs reconcile --path <p> --prefix <pre> --execute` with the fail-closed
  `Blacklist::from_sync_config` deny-set. Repo-roam is the **same** mechanism
  pointed at a `~/git` repo instead of `~/.claude/projects`.

The **only new surface** this plan adds is a **precision layer**: a git-aware
**dev-env zero-diff fingerprint** asserted on BOTH hosts. It does **not** replace
any QA-matrix row; it is a tighter assertion *over* existing rows. See §3 for the
exact row mapping.

The QA matrix is the **existing** one — do not invent rows:

- **T-rows** T1-T15 from the onboarding design (`## QA Matrix`, lines 224-240).
- **M-rows** M1-M8 from
  [`lazy-traversal-qa-permutation-matrix-2026-05-09.md`](lazy-traversal-qa-permutation-matrix-2026-05-09.md),
  cited by the design's Onboarding Pilot Rows table.
- **G5 / TIN-1620 minimum**: T1, T3, T4, T5, T6, T10, T11 + M3, M5, M5-R, M6, M8.

---

## 1. Reuse map (do NOT rebuild these)

| Need | Reuse | Task |
| --- | --- | --- |
| inventory -> isolated shadow -> push -> honey hydrate -> Linux lifecycle (copies the FULL repo incl `.git` as plain files) | `scripts/git-repo-canary.sh` -> `scripts/home-canary-linux-xr-shadow.sh` | `task lazy:git-repo-canary` |
| read-only pre-enroll inventory (git_present/git_dirty/special-file gate) | `scripts/large-workdir-inventory.py` | `task lazy:large-workdir-inventory` |
| fresh-tree restore / rollback (the G5 "return to clean exact tree" proof) | `scripts/git-repo-restore-proof.sh` | `task lazy:git-repo-restore-proof` |
| mounted browse-before-hydrate (T1) + cat-hydrates-exact (T2) + symlink (T12) | `scripts/lazy-hydration-mounted-smoke.sh` | `task lazy:mounted-smoke` |
| G2/G3 backbone precondition (read-only) | `scripts/honey-backbone-preflight.sh` | `task lazy:honey-backbone-preflight` |
| neo<->honey unsync/rehydrate, conflict, delete/rename lifecycle (T4-T11 / M-rows) | `scripts/neo-honey-unsynced-rehydrate-demo.sh` + variants | `task lazy:neo-honey-unsynced-rehydrate-plan` etc. |
| **NEW** — git-aware dev-env zero-diff fingerprint | `scripts/repo-roam-fingerprint.sh` | `task lazy:dev-env-fingerprint` |

Two harnesses referenced below land via their own PRs and are **not yet on
`main`**, so this plan cites them as integration points rather than dependencies:

- the **TIN-1620 flip-flop** harness (`scripts/tin1620-flipflop-canary-harness.sh`,
  PR #484) — host-readiness-gated, plan-only by default; wraps the neo<->honey demos.
- the **Facet-6 .git fsck/conflict** harness
  (`scripts/git-dotgit-fsck-conflict-harness.sh`, PR #506) — the raw-mode `.git`
  corruption gate (§7).

The evidence packet shape is the established
`docs/release/evidence/<run-id>/` convention (`source-inventory/`,
`shadow-inventory/`, `push/`, `honey/`, `lifecycle/`, `restore-proof/`). This plan
adds exactly **one** new subtree, `dev-env-fingerprint/`, and **one** gate line.

---

## 2. The dev-env fingerprint tool

`scripts/repo-roam-fingerprint.sh` captures a complete git-SEMANTIC fingerprint of
a repo and compares two captures, failing on ANY difference:

- `git status --porcelain=v2 --branch` (distinguishes index vs worktree, records
  the checked-out branch + ahead/behind)
- HEAD + `symbolic-ref` (branch / detached-HEAD safe) + `write-tree` of the index
- all refs (`show-ref`) and the branch set
- staged vs unstaged content (`git diff --cached` / `git diff` hashes) and
  per-path staged blob shas (`ls-files -s`)
- untracked set, `stash list` + `refs/stash`, reflog tip
- **`git fsck --full`** — the corruption gate (clean vs dirty verdict)
- a sorted `relpath<TAB>mode<TAB>sha256|symlink-target` manifest of tracked +
  untracked working files, honoring the **same fail-closed deny-set** as the
  reconcile engine (`.env*`/secret/live-WAL never hashed; recorded `DENIED`) plus
  `target/ node_modules/ .direnv/` excludes for determinism.

Modes: `capture <repo> <out>`, `compare <a> <b>` (exit 1 on any diff),
`seed-canary <dir>` (throwaway repo with feature branch + staged + unstaged +
untracked + stash + exec script + symlink), `self-test` (disposable round-trip).

Self-test / regression: `task lazy:test-dev-env-fingerprint`.

The single green signal is `dev-env-zero-diff=pass`, threaded into the packet's
`result.env` / `parity-gates.env` next to the existing
`scoped-project-tree-parity-evidence-complete` gate.

---

## 3. Fingerprint -> existing QA-matrix mapping

The fingerprint does not introduce a new matrix. It is the
*"…and git confirms the dev env is byte-and-semantically identical, with no
mid-reconcile corruption"* qualifier on these **existing** rows:

| Existing row (source) | What it already asserts | Fingerprint precision added |
| --- | --- | --- |
| **T2 / T3** (design QA Matrix) | exact bytes hydrate / rehydrate on demand | working-file sha256 manifest equality, source vs rehydrated |
| **T4 / T5 / T6** | clean file/dir unsync; dirty unsync refusal | status/index fingerprint identical after a clean flip-flop; refusal leaves no partial state |
| **T8 / T9 + M3 / M6** | peer edit/delete/rename rehydrates latest bytes | post-rehydrate fingerprint == the peer's fingerprint |
| **T10 / T11 + M5 / M5-R** | same-file conflict visible; keep-both recovery | for a repo this means BOTH `.git` states preserved and each fsck-clean |
| **T12** (symlink parity) | preserve or explicitly fail with a recorded blocker | manifest records every `120000` symlink + target; **reconcile drops these today** (§5) |
| **T13** (xattrs / modes) | round-trip or documented unsupported | exec-bit/mode round-trips (good); xattrs unsupported (documented) |
| TIN-1620 packet "rollback proof … clean, exact tree" | fresh-tree restore returns a clean tree | restored-tree fingerprint == pre-push source fingerprint, fsck clean |

The new gate is best treated as sub-row **T13-Z (dev-env zero-diff)** layered on
T12+T13 — explicitly a *tighter* bar than G5's content-exact minimum, gated behind
the §5 mitigations. Do **not** claim T13-Z green from existing G5 evidence.

---

## 4. Enrollment: `.git-as-files` is config-scoped, NOT a global flip

Per Facet 4 (verified against source on this branch):

- The `Reconcile` clap variant exposes only `--path/-p`, `--prefix`, `--execute`,
  `--state` (`crates/tcfs-cli/src/main.rs:288-301`). **There is no
  `--sync-git-dirs` flag.**
- `.git` enrollment is gated by `[sync] sync_git_dirs` (default **false**,
  `crates/tcfs-core/src/config.rs:548`) and the `.git-as-files` choice additionally
  requires `[sync] git_sync_mode = "raw"` (default is **`"bundle"`**,
  `config.rs:549`). Raw mode recurses into `.git` and uploads internals as plain
  objects (`engine.rs:3348-3356`); bundle mode packs a `git bundle` instead.
- `cmd_reconcile` builds the blacklist via `Blacklist::from_sync_config(&config.sync)`
  from the single `-c/--config` it loads (`main.rs:6452`), and the gate reads
  through `allows_git_dirs()` / `git_sync_mode()` at `reconcile.rs:1115-1116` and
  `engine.rs:3331`.

### Blast radius of a GLOBAL flip (do NOT do this)

Setting `sync_git_dirs = true` in the daemon's shared `/etc/tcfs/config.toml`
flips it for the long-running daemon AND every default-config CLI call
(`tcfsd/src/daemon.rs:423` logs `git_dirs = config.sync.sync_git_dirs`). That would
make the primary `~/tcfs` `sync_root` and the watcher start collecting `.git` for
**every** repo under any synced root, and (with the default `bundle` mode) trigger
`collect_git_bundles` across the whole tree — a fleet-wide behavior change. **This
plan forbids the global flip.**

### The minimal, zero-code, per-root enrollment (recommended)

Point the scheduled unit at a **dedicated per-repo config** with `-c` (or
`TCFS_CONFIG=`):

```toml
# ~/.config/tcfs/roam-<repo>.toml  (per-repo, NOT the daemon's shared config)
sync_root     = "/Users/<you>/git/<repo>"
remote_prefix = "git-roam/<repo>"          # disposable, repo-scoped prefix

[sync]
sync_git_dirs   = true       # enroll .git
git_sync_mode   = "raw"      # operator's choice: .git-as-plain-files (NOT bundle)
sync_hidden_dirs = true
```

```
tcfs reconcile -c ~/.config/tcfs/roam-<repo>.toml \
  --path ~/git/<repo> --prefix git-roam/<repo> --execute
```

This scopes the `.git` gate to one root **without touching the daemon's shared
config** — exactly what the in-tree canary fixture
(`crates/tcfs-cli/src/main.rs:7461-7497`, which writes `tcfs-canary.toml` with
`sync_git_dirs = true`, `git_sync_mode = "raw"`) and the `git-repo-canary` evidence
harness already do. The fail-closed security deny-set (`SECURITY_DIRS`, secret /
live-WAL suffixes) still applies *underneath* `.git`, so this does **not** bypass
Gate G0 / `TIN-1737`.

**Verdict:** `~/git` enrollment works **as-is with no Rust change** — via a
per-repo config on the reconcile unit. An additive `#[arg(long)] sync_git_dirs`
flag on `Reconcile` is a *nice-to-have* (cleaner one-file unit) but is **not
required**; if added it must default false (no behavior change) and override
post-load via `Blacklist::new(...)` instead of `from_sync_config`.

---

## 5. The zero-diff caveat: mtime trap + symlink drop (Facet 5)

A naive `tcfs reconcile` enrollment will **NOT** produce a zero-diff dev env on
honey as-is. Two blocking defects, both verified in source on this branch:

1. **mtime / index trap.** `SyncManifest` has no source-mtime field (only
   `written_at`, the publish wall-clock; `manifest.rs:43`), and restore writes via
   atomic rename with no `set_times`/`utimensat` afterward — the restored file gets
   a fresh OS mtime. With `.git` synced raw, the restored `.git/index` carries the
   **source** machine's stat cache while the worktree files beside it get **new**
   mtimes, so honey's first `git status` force-rehashes the whole tree and smudges
   the index -> spurious local modification of a synced `.git/index`, sync churn,
   possible false conflict. There is **no** `git update-index --refresh` anywhere
   in the codebase to repair this.
   - **Mitigation (run before the first fingerprint compare):** after rehydrate,
     run `git -C <repo> update-index --refresh -q` (or a `git status` warm-up)
     **once**, deterministically, so git rewrites its own stat cache before the dev
     touches it; then re-sync the smudged `.git/index` as the new synced state.
     Longer-term fix: preserve mtime on restore (add an mtime field to
     `SyncManifest`, apply via `filetime`/`utimensat` after the rename and before
     `make_sync_state_full`).

2. **symlink drop in the reconcile collect path.** `collect_local_set` hardcodes
   `preserve_symlinks: false` (`reconcile.rs:1120`), so every working-tree symlink
   is silently dropped — a git-tracked symlink (mode `120000`) then shows as a
   **deleted** tracked file on honey, failing T12.
   - **Mitigation:** set `preserve_symlinks: true` (keep `follow_symlinks: false`)
     in `collect_local_set`; the restore side + ingress deny-set already exist.
     One-line opt-in.

File **modes / exec bit DO round-trip** (`engine.rs:1969-1976` capture +
`set_permissions` on restore), so modes are not a zero-diff risk. **xattrs** are
not captured (document as unsupported per T13). **Empty dirs** and **special
files** (FIFO/socket/device) are dropped by reconcile but do not affect
`git status`; the Phase-0 inventory (§6 step R0) flags any repo that contains them.

The fingerprint tool is what makes all of the above **visible** rather than silent:
it records symlink targets, fsck verdict, and the full manifest, so a failing
`dev-env-zero-diff` immediately points at the mtime smudge or the missing symlink.

---

## 6. The canary procedure (steps mapped to rows + LIVE markers)

Legend: **[PR]** = exercised by this PR's harness/self-test (no fleet);
**[LIVE]** = operator/agent-driven on the real neo<->honey fleet, NOT in this PR.

### R0 — Inventory + seed (neo)  · rows: pre-gate, T13 inventory

- **[LIVE]** `task lazy:honey-backbone-preflight` — confirm G2/G3 backbone
  (`nats_ok`, `storage_ok`, two real `age1…` devices). Hard precondition.
- **[PR]** Seed a throwaway canary repo and capture its source fingerprint:

  ```
  scripts/repo-roam-fingerprint.sh seed-canary /tmp/roam-canary
  scripts/repo-roam-fingerprint.sh capture /tmp/roam-canary \
    docs/release/evidence/<run-id>/dev-env-fingerprint/source
  ```

  (For a real expendable repo: `task lazy:large-workdir-inventory` first; require
  bucket `shadow_pilot_ready`, no blocking special files.)

### R1 — Shadow + enroll + push (neo)  · rows: T1, T2; coverage of uncommitted/staged/untracked as bytes

- **[PR/LIVE]** `task lazy:git-repo-canary` with `--source /tmp/roam-canary`
  `--allow-dirty-source` and a disposable `--remote .../git-roam/<repo>` prefix —
  builds the isolated shadow (full repo incl `.git` as plain files), pushes to the
  disposable prefix. The per-repo `.git-as-files` config (§4) is what scopes the
  `.git` enrollment.
- **[LIVE]** In production this is the scheduled `tcfs reconcile -c <per-repo>.toml
  --path --prefix --execute` unit (§4), not a one-shot, but the canary task proves
  the same push.

### R2 — Hydrate + fingerprint + compare (honey)  · rows: T1, T2, T3, T12; **gate T13-Z**

- **[LIVE]** On honey: `ls/find` before hydration (T1), `cat` selected file (T2),
  full hydrate, then the **mtime mitigation** from §5 (`git update-index --refresh
  -q`) **before** fingerprinting.
- **[LIVE]** Capture honey's fingerprint and compare to neo's source:

  ```
  scripts/repo-roam-fingerprint.sh capture <hydrated-repo> \
    docs/release/evidence/<run-id>/dev-env-fingerprint/rehydrated
  scripts/repo-roam-fingerprint.sh compare \
    docs/release/evidence/<run-id>/dev-env-fingerprint/source \
    docs/release/evidence/<run-id>/dev-env-fingerprint/rehydrated
  ```

  **Green bar:** `dev-env-zero-diff=pass` AND `fsck=clean` both sides AND no
  spurious-dirty `git status`. A symlink delta here is the §5 T12 drop; an index
  delta is the §5 mtime smudge.

### R3 — Flip-flop: unsync neo -> edit+commit honey -> rehydrate neo  · rows: T4, T5, T6, T8, M3, M6

- **[LIVE]** `tcfs unsync <repo>` on neo (clean) -> edit + commit on honey ->
  rehydrate on neo. Then `compare` neo's rehydrated fingerprint to honey's: must be
  zero-diff (honey's HEAD/branch/index now on neo, fsck clean).
- **[LIVE]** Reuse `scripts/neo-honey-unsynced-rehydrate-demo.sh` (and, once
  landed, the `tin1620-flipflop-canary-harness.sh` readiness gates) for the
  transport; the fingerprint is the new assertion on top.

### R4 — Rollback / fresh-tree restore  · rows: TIN-1620 packet rollback proof

- **[LIVE]** `task lazy:git-repo-restore-proof` into a clean restore-root, then
  `capture` the restored tree and `compare` to the R0 source fingerprint: restored
  == source, fsck clean. This is the G5 "return to a clean, exact tree" proof, now
  with a git-semantic equality assertion instead of per-file SHA256 only.

### R5 — Conflict / keep-both  · rows: T10, T11, M5, M5-R

- **[LIVE]** Same-file conflict across hosts: conflict visible, local bytes
  preserved; manual keep-both -> both `.git` states preserved, each fsck-clean and
  each fingerprint-able. See §7 for the concurrent-`.git` corruption gate.

---

## 7. Concurrent `.git` corruption checks (Facet 6)

Under `git_sync_mode = "raw"` + `conflict_mode = "auto"`, the `AutoResolver`
resolves each conflicting path independently with **no** awareness that a file is
part of a `.git` directory, so two machines editing the same repo's `.git`
concurrently can keep device A's `refs/heads/main` alongside device B's
object store -> a half-applied ref (`git fsck`:
`error: refs/heads/main: invalid sha1 pointer`). Aggravator: raw upload checks
`git_is_safe(.git)` once then streams `.git/*` over many seconds (TOCTOU);
`acquire_git_lock` exists but is never called on the raw upload path.

The live-repo packet must add five raw-mode `.git` rows (these are the
`scripts/git-dotgit-fsck-conflict-harness.sh` rows from PR #506, **not** on `main`):

- **G5-git-1 PEER FSCK** — after push-on-A -> rehydrate-on-B, `git fsck --full` is
  clean, HEAD resolves to a present commit. The fingerprint's `fsck.env` gate
  enforces this in R2/R3 above.
- **G5-git-2 NO TORN SNAPSHOT** — a repo with `.git/index.lock` / in-progress
  rebase is skipped that cycle, never uploaded half-applied.
- **G5-git-3 CLEAN FLIP-FLOP EXACT** — clean `tcfs unsync` -> rehydrate restores an
  fsck-clean, byte-exact repo (the R3 fingerprint compare proves this).
- **G5-git-4 DIRTY-CHILD REFUSAL** — `tcfs unsync` with a dirty `.git` child
  refuses without `--force`; no partial dehydration (row T6).
- **G5-git-5 CONCURRENT-WRITE CORRUPTION** — the per-file `.git` conflict interleave
  under `conflict_mode=auto` MUST be detected by `git fsck` (invalid sha1 pointer).
  **Until conflict resolution is made `.git`-aware, G5-git-5 is EXPECTED TO FAIL**
  and stands as the corruption gate that must close before G5 can claim
  concurrent-edit safety.

**Safe handoff rule:** the `tcfs unsync <repo>` flip-flop is the correct primitive —
do not let two machines hold the same `.git` hydrated+writable at once. Quiesce git
on the source host before unsync.

---

## 8. Scale ceiling (claim boundary)

- **Repo-by-repo only.** `TIN-1908`/`TIN-1620` deliberately enroll one repo at a
  time. Broad `~/git` ownership stays OUT of claim until `TIN-1556` (stable root
  identity) + `TIN-1416` (subscriptions) land. **Stop rule:** do not bulk-enroll
  all of `~/git` before two small repos pass R0-R5 in both directions.
- **Small `.git` first.** `linux-xr` (~6.2 GB mostly `.git`) stays a stress-only
  target, never a daily-driver enrollment. The serial crypto upload of a multi-GB
  `.git` can tear mid-cycle (§5/§7), so big-`.git` is explicitly past the Phase-2
  boundary.
- **Still NOT claimed** (per TIN-1620 boundary): broad `~/git`, home takeover,
  production Finder, keep-synced/pin, `/tmp`, general self-service.

### `.git`-as-files vs git-bundle — operator reconciliation item

The shipped acceptance doc's `.git -> Git-safety bundle/restore` root class
prescribes the **bundle** path for history and warns against naive live
`.git/objects` mirroring, whereas the operator chose **`.git`-as-plain-files**
(`git_sync_mode = "raw"`). These are in tension. A raw-mode enrollment must still
satisfy R4's HEAD/branch/refs/**fsck** assertions — which the fingerprint's
`fsck.env` + `refs.txt` + `head.env` captures enforce — but the raw path is exactly
where §7's corruption gate lives. **Flag for operator decision; do not silently
diverge from the bundle-based R4 acceptance.**

### `stash` is a precision add, not a new ticket

`stash` is not separately enumerated in any ticket; it rides the `.git/refs/stash`
+ reflog restore path. The fingerprint asserts it explicitly (`stash-list.txt`,
`stash.env`) — a row-level precision add to R4, not a goalpost move.

---

## 9. Guardrails for this plan

- READ-ONLY research + this docs/scripts PR only. No `reconcile --execute` against
  prod, no daemon/lab changes, no enrolling real repos in this PR.
- The harness is safe by construction: `seed-canary` refuses `$HOME` / `~/git/*` /
  filesystem root; `capture` is read-only; the self-test uses a disposable `/tmp`
  repo. Real repos require an explicit `--source`/`REPO=` argument.
- All **[LIVE]** steps are operator/agent-driven on the fleet and are **out of
  scope for this PR**.
