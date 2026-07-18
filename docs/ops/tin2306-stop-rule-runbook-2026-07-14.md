# TIN-2306 Stop-Rule Clearance Runbook — Two-Repo R0–R5, Both Directions

**Status:** DRAFT / executable-under-supervision · **Gate:** G5 → TIN-1620 → TIN-2306 · **Live child:** TIN-1908
**Operative ladder:** `docs/ops/repo-roam-test-plan-2026-06-08.md` §6 (newer, PR/LIVE-boundary-aware; supersedes TIN-1908's own Acceptance rungs as the *procedure*, though TIN-1908's Acceptance remains the open Linear gate — see §8).
**House style basis:** `docs/release/evidence/divergent-keep-both-canary-PLAN.md` §0–§10.
**Fleet:** neo (macOS, ~0.12.16, deploy PENDING) ↔ honey (Rocky, v0.12.17 re-switch IN FLIGHT).
**Read-only worktree note:** this repo checkout is a READ-ONLY operator worktree. Read all docs via `git show origin/main:<path>`. Do not commit here. Evidence lands under `docs/release/evidence/` on a *writable* clone/branch, not this worktree.

---

## Legend for operator flags

- **⛔ [GATE]** — a hard precondition an operator must confirm/authorize before proceeding. Do not auto-advance.
- **🔑 [CRED]** — requires a live credential / auth session an agent cannot self-provision.
- **🔀 [DECISION]** — an operator must choose between documented options; the choice changes later steps.

---

## Stage 0 — Preflight (both hosts)

**Purpose:** prove versions, config, canonical state paths (post-#545), backbone, disk, auth posture, and candidate-repo cleanliness *before* touching any repo. Everything here is read-only except the two candidate-repo hygiene fixes (0.7).

### 0.1 ⛔ [GATE] neo deploy state

The merged bidirectional/keep-both FF stack (#513/#534/#542, TIN-2657/#545) MUST be live on neo. neo deploy is currently PENDING and neo is `blockedBy` **TIN-2519** (Darwin HM offload lane). **neo NEVER compiles from source** — deploy only via PZM darwin remote builder or the hosted-CI darwin closure (#524), never a local build.

```bash
tcfs --version                 # neo: expect >= the #545/TIN-2657 build, not 0.12.13
git -C <writable-clone> merge-base --is-ancestor 754390f origin/main && echo "545-ancestor OK"
```
**Pass:** neo binary contains #545 (state-path convergence) and the #534 loser-guard. If neo is still pre-deploy → **ABORT, escalate TIN-2519.** Do not proceed on a neo that cannot honor the merged FF logic (2026-07-04 TIN-2306 comment: "should not start broad clearance yet" pending live-fleet proof).

### 0.2 ⛔ [GATE] honey re-switch complete

```bash
ssh jess@honey 'tcfs --version && systemctl --user is-active tcfsd 2>/dev/null || launchctl ...'
```
**Pass:** honey reports v0.12.17 and daemon active. If re-switch still in flight → **HOLD** at Stage 0; R2/R3 (honey-side hydrate) cannot run against a half-switched daemon.

### 0.3 Canonical state paths (post-#545) — verify, do not "fix"

#545 normalizes any `--state`/`TCFS_STATE_PATH` override via `expand_tilde(p).with_extension("json")`, so `.db` and `.json` converge on the canonical `state.json`; daemon boot does a one-time guarded absorb of a legacy `state.db`. Expect the **absorb-then-diverge signature**:

```bash
# neo
grep -n state_db ~/.config/tcfs/config.toml           # key may STILL name .../state.db — expected, harmless
ls -la ~/.local/share/tcfsd/                           # state.json = active/newer; state.db = stale
# honey
ssh jess@honey 'ls -la ~/.local/share/tcfsd/'
```
**Pass:** `state.json` is the newer/active file on both hosts. `state.db` older/stale is *correct*, not a fault. **Do not delete state.db** (absorb is idempotent-guarded; leave it).

### 0.4 🔀 [DECISION] Enrollment path — canonical-state vs nix-isolated-state

This decision determines whether **manual `tcfs resolve`** works in R5.

- **Option M (RECOMMENDED): manual per-root config.** Author `~/.config/tcfs/roam-<repo>.toml` and drive `tcfs reconcile -c … --path --prefix --execute` directly (repo-roam-test-plan §4 "minimal, zero-code, per-root enrollment"). Zero nix-switch, zero systemd — sidesteps neo-never-builds friction. Conflicts land in the daemon's **canonical** state → `tcfs resolve` verb works.
- **Option N: nix-enrolled unit** (`extraReconcileRoots` in lab). Writes conflicts to an **ISOLATED** per-unit state (`~/.local/state/tcfsd/reconcile/<name>.json`). Per PLAN §0 surprise-6 (UNCHANGED by #545): `tcfs resolve` cannot see isolated-state conflicts — reachable only via `tcfs conflicts --state <isolated-path>`, and R5 can rely only on the **automatic loser-guard** (proven), not the manual resolve verb. Also requires nix edit + flake update + `nix-switch` on **both** hosts = new deploy work on a neo that can't build.

**Default for this runbook: Option M.** Record the chosen option in RESULTS.md; every R5 command below assumes M. If N is chosen, replace R5 manual-resolve steps with `tcfs conflicts --state …` + loser-guard convergence assertion only.

### 0.5 🔑 [CRED] Auth posture + agent-coupling

TIN-1908 requires an **agent-coupling** proof (matching `~/.claude/projects` session/subtree hashes across hosts) and a **policy-leak** check.
- **honey Claude auth is DEAD** (fleet memory 2026-07-09) and headless tokens are open (TIN-2653). If the agent-coupling sub-proof is in scope for this run, an operator must restore honey Claude auth first. → **🔑 [CRED] / possibly defer agent-coupling to a follow-up; note the deferral explicitly in §9.**
- Policy-leak guard (run against every push plan and every evidence packet before archiving):
```bash
grep -rEl '(^|/)\.env|auth\.json|\.sqlite($|3)|-wal$|-shm$|/(cache|generated)/' <push-plan-or-evidence-dir> \
  && echo "POLICY LEAK — ABORT" || echo "policy-leak clean"
```
**Pass:** zero matches. Any hit → **ABORT** that rung, scrub, re-plan.

### 0.6 Backbone + disk preflight

```bash
task lazy:honey-backbone-preflight    # expect nats_ok, storage_ok, two real age1… devices
task lazy:large-workdir-inventory     # expect bucket shadow_pilot_ready, no blocking special files
df -h ~ ; ssh jess@honey 'df -h ~'    # neo noted 97% full historically → ENOSPC risk on shadow build
```
**Pass:** backbone green (G2/G3), inventory bucket `shadow_pilot_ready`, and **≥ shadow-size free headroom on both hosts**. neo ENOSPC previously failed a switch — if neo home is >90% full, **⛔ [GATE] operator must free space** before R1 shadow build.

### 0.7 Candidate-repo selection + hygiene (one-time, on neo)

**Repo A = `/Users/jess/git/Dell-7810`** (9.3M, 241 files, clean, no symlinks, no sqlite/WAL, no live worktree fence).
**Repo B = `/Users/jess/git/site.scaffold`** (git payload tiny/clean, main in sync, freshest activity; but 311M node_modules with **1,280 pnpm symlinks** must be fenced out of roam scope).
**REJECTED: `ci-templates`** — DISQUALIFIED by a *live* worktree fence (two populated checkouts share its `.git`). Do not use as a primary slot.

Hygiene fixes (idempotent, safe):
```bash
# Repo A: clear the dead run3 worktree + get off the abandoned run2 branch ([gone] upstream)
git -C /Users/jess/git/Dell-7810 worktree prune
git -C /Users/jess/git/Dell-7810 checkout main      # 🔀 [DECISION] main vs run3 — record which trunk you certify
git -C /Users/jess/git/Dell-7810 status --porcelain=v2 --branch    # expect clean, branch.ab +0 -0 if on main

# Repo B: confirm main in sync; fence node_modules OUT of roam scope
git -C /Users/jess/git/site.scaffold status --porcelain=v2 --branch # expect main, +0 -0
git -C /Users/jess/git/site.scaffold check-ignore -v node_modules    # expect matched .gitignore
```
**Pass:** A clean on chosen trunk, no live worktrees (`git worktree list` = 1 entry). B clean on `main`, node_modules confirmed gitignored. For B, the roam config (Stage R1) MUST explicitly exclude `node_modules` unless the deployed reconcile is confirmed `.gitignore`-aware (verify against `tcfs reconcile --help`); a raw walk that traverses 1,280 symlinks is a §5 T12 symlink-farm hazard.

### 0.8 PR-side fsck preflight (daemon-free, safe on neo — pure shell/git, no tcfs build)

```bash
task lazy:test-git-dotgit-fsck-conflict     # scripts/git-dotgit-fsck-conflict-harness.sh, builds own fixture
```
**Pass:** all stages green (fsck/mid-write/flip-flop/corruption-risk + Stage-6 loser-guard reproduction). This proves the *mechanism* only, NOT fleet zero-diff (see §9). A red here **ABORTS** the whole run — do not roam with a broken corruption-guard.

**Stage 0 rollback:** none needed (read-only + idempotent hygiene). If any GATE fails, stop; no fleet state was mutated.

---

## Stages 1–6 — The R0–R5 ladder (run 4 times)

Run the full ladder **four times**, once per (repo, direction) tuple. `both directions` = full R0–R5 twice per repo (TIN-2306 body + TIN-1908 R1/R2):

| Run | REPO | ORIGIN | TARGET | Packet dir suffix |
|-----|------|--------|--------|-------------------|
| 1 | Dell-7810 | neo | honey | `A-fwd` |
| 2 | Dell-7810 | honey | neo | `A-rev` |
| 3 | site.scaffold | neo | honey | `B-fwd` |
| 4 | site.scaffold | honey | neo | `B-rev` |

Parameters per run:
```
RUN_ID=repo-roam-canary-$(date -u +%Y%m%dT%H%M%SZ)     # one per (repo,direction) packet
REPO=<Dell-7810|site.scaffold>
ORIGIN=<neo|honey>   TARGET=<honey|neo>
PREFIX=git-roam/<repo>-<A-fwd|A-rev|B-fwd|B-rev>        # disposable, per-run isolated prefix
EVID=docs/release/evidence/$RUN_ID                      # on a WRITABLE clone/branch, not this worktree
```
> All `neo`/`honey` host references below mean ORIGIN/TARGET for the run in question — the ladder is symmetric (PLAN §5: "either host could be chosen; the design is symmetric").

### Stage R0 — Inventory + seed (ORIGIN) · proves pre-gate + T13 inventory

```bash
mkdir -p $EVID/dev-env-fingerprint/source
# seed + capture source fingerprint on ORIGIN
scripts/repo-roam-fingerprint.sh seed-canary   --repo /path/to/$REPO   # for a REAL repo, this is a no-op seed; use capture
scripts/repo-roam-fingerprint.sh capture --repo /path/to/$REPO --out $EVID/dev-env-fingerprint/source
git -C /path/to/$REPO fsck --full --strict 2>&1 | tee $EVID/R0-origin-fsck.txt
git -C /path/to/$REPO status --porcelain=v1 -b | tee $EVID/R0-origin-status.txt
```
**Pass:** inventory bucket `shadow_pilot_ready` (from 0.6), source fingerprint captured, `fsck` clean, status matches the certified trunk. No blocking special files.

### Stage R1 — Shadow + enroll + push (ORIGIN) · proves T1, T2 (uncommitted/staged/untracked as bytes)

Option M enrollment (per 0.4):
```bash
cat > ~/.config/tcfs/roam-$REPO.toml <<TOML
# per-root, secret-free; excludes vendor tree for site.scaffold
sync_root = "/path/to/$REPO"
# git_sync_mode = "raw"  (raw .git-as-files; supported since PR #18 / 0.12.14)
# exclude = ["node_modules"]   # REQUIRED for site.scaffold unless reconcile is gitignore-aware
TOML

tcfs reconcile -c ~/.config/tcfs/roam-$REPO.toml --path /path/to/$REPO --prefix $PREFIX --execute \
  2>&1 | tee $EVID/R1-push.log
```
For the canary self-test variant instead of a real repo: `task lazy:git-repo-canary --source … --allow-dirty-source --prefix git-roam/<repo>`.

**Pass:** shadow build completes (full repo incl `.git` as plain files), push reports the expected object/file count, **policy-leak grep (0.5) clean on the push plan**, no ENOSPC. For B: confirm `node_modules` and its 1,280 symlinks are **absent** from the push manifest.

### Stage R2 — Hydrate + fingerprint + compare (TARGET) · proves T1,T2,T3,T12 · **gate T13-Z (dev-env zero-diff)**

```bash
ssh jess@$TARGET '
  mkdir -p /tmp/hydrate-'"$REPO"' &&
  ls -la /tmp/hydrate-'"$REPO"' ; find /tmp/hydrate-'"$REPO"' | head    # T1: browse-before-hydrate
'
tcfs pull  --prefix $PREFIX --dest <target-path>    # full hydrate on TARGET
ssh jess@$TARGET '
  cat <selected-file> ;                                               # T2: exact-hydrate bytes
  git -C <target-path> update-index --refresh -q ;                   # mtime mitigation BEFORE fingerprint
  git -C <target-path> fsck --full --strict ;
  git -C <target-path> status --porcelain=v1 -b
'
scripts/repo-roam-fingerprint.sh capture --repo <target-path> --out $EVID/dev-env-fingerprint/target
scripts/repo-roam-fingerprint.sh compare  --source $EVID/dev-env-fingerprint/source \
                                           --target $EVID/dev-env-fingerprint/target \
  2>&1 | tee $EVID/R2-zero-diff.txt
```
**Pass (GREEN BAR):** `dev-env-zero-diff=pass` AND `fsck=clean` both sides AND no spurious-dirty `git status`. **Fail signatures:** a symlink delta = §5 **T12** drop; an index delta = §5 **mtime smudge** (re-check the `update-index --refresh` ran before fingerprint).

### Stage R3 — Flip-flop (unsync ORIGIN → edit+commit TARGET → rehydrate ORIGIN) · proves T4,T5,T6,T8,M3,M6

```bash
tcfs unsync $REPO            # on ORIGIN, repo clean
ssh jess@$TARGET 'cd <target-path> && <edit a tracked file> && git commit -am "R3 flip-flop $RUN_ID"'
# rehydrate ORIGIN
tcfs pull --prefix $PREFIX --dest /path/to/$REPO
git -C /path/to/$REPO update-index --refresh -q
scripts/repo-roam-fingerprint.sh capture --repo /path/to/$REPO --out $EVID/R3-origin-rehydrated
# compare ORIGIN-rehydrated to TARGET's post-edit fingerprint
scripts/repo-roam-fingerprint.sh compare --source <target-post-edit> --target $EVID/R3-origin-rehydrated \
  | tee $EVID/R3-zero-diff.txt
```
Reuses `scripts/neo-honey-unsynced-rehydrate-demo.sh` and (once landed) `tin1620-flipflop-canary-harness.sh`.
**Pass:** zero-diff — TARGET's HEAD/branch/index now on ORIGIN, `fsck` clean. TARGET's new commit is present on ORIGIN.

### Stage R4 — Rollback / fresh-tree restore · proves TIN-1620 "rollback proof — clean, exact tree"

```bash
task lazy:git-repo-restore-proof --prefix $PREFIX --restore-root /tmp/restore-$REPO   # offload build if it compiles
git -C /tmp/restore-$REPO fsck --full --strict
scripts/repo-roam-fingerprint.sh capture --repo /tmp/restore-$REPO --out $EVID/R4-restored
scripts/repo-roam-fingerprint.sh compare --source $EVID/dev-env-fingerprint/source --target $EVID/R4-restored \
  | tee $EVID/R4-restore-equal.txt
```
**Pass:** `restored == source` (git-semantic equality, not per-file SHA only), `fsck` clean, and the restore verifies **HEAD, branch, all refs needed by the WIP, `git status`, object-graph sanity** (TIN-1908 R4). This IS the G5 "return to a clean, exact tree" proof.
> **neo-never-builds:** if `lazy:git-repo-restore-proof` compiles tcfs, run it on PZM/CI, not neo. If it only invokes the installed binary, neo is fine.

### Stage R5 — Conflict / keep-both · proves T10, T11, M5, M5-R

```bash
# same-file divergence across hosts
ssh jess@$TARGET 'cd <target-path> && echo "target-side" >> CONFLICT.txt && git commit -am "R5 target"'
# on ORIGIN, edit same file differently, then reconcile
(cd /path/to/$REPO && echo "origin-side" >> CONFLICT.txt && git commit -am "R5 origin")
tcfs reconcile -c ~/.config/tcfs/roam-$REPO.toml --path /path/to/$REPO --prefix $PREFIX --execute \
  2>&1 | tee $EVID/R5-conflict.log

# Option M (canonical primary state) — inspect, dry-run, then execute:
tcfs conflicts
tcfs resolve /path/to/$REPO --strategy keep-both
tcfs resolve /path/to/$REPO --strategy keep-both --execute

# Option N (registered isolated state) — the daemon selects the enrolled root
# descriptor; no client state path or remote prefix is accepted for mutation:
tcfs conflicts --root <root-id>
tcfs resolve /path/to/$REPO --root <root-id> --strategy keep-both
tcfs resolve /path/to/$REPO --root <root-id> --strategy keep-both --execute

# after keep-both: both .git states preserved, each fsck-clean and each fingerprint-able
git -C /path/to/$REPO fsck --full --strict
git -C <parked-loser-path> fsck --full --strict
```
**Pass:** conflict visible; **local bytes preserved** (zero-loss); keep-both leaves **both** `.git` states intact, each `fsck`-clean and each fingerprint-able; the G5-git-13 loser-guard auto-park is observed (via #534, not a resolve *verb*). Tied to §7 concurrent-`.git` corruption gate (Facet 6 harness, G5-git-1..5/13). Independent sibling edits (M5-R) must converge.

**Per-run teardown/rollback:**
```bash
tcfs unsync $REPO                                   # detach the roam
rm -f ~/.config/tcfs/roam-$REPO.toml                # Option M only
# reset both working copies to the certified trunk; drop the disposable prefix
git -C /path/to/$REPO checkout <trunk> && git -C /path/to/$REPO branch -D <R3/R5 scratch branches> 2>/dev/null
ssh jess@$TARGET 'cd <target-path> && git checkout <trunk> && git clean -fdx -e node_modules'   # -e for site.scaffold!
# the disposable git-roam/<repo>-<dir> prefix is throwaway; leave it or GC per bucket policy
```
Because every run uses a **disposable per-run prefix**, a failed run never pollutes a later run. The candidate repos are real → **never `git clean -fdx` without `-e node_modules` on site.scaffold** and never delete the operator's real trunk branches.

---

## Stage 7 — Evidence packet structure

One dated packet **per repo per direction** (4 total). Archive under `docs/release/evidence/` on a writable branch.

```
docs/release/evidence/<RUN_ID>/            # e.g. repo-roam-canary-20260714T…Z  (suffix A-fwd/A-rev/B-fwd/B-rev)
├── RESULTS.md
├── dev-env-fingerprint/
│   ├── source/            # R0 ORIGIN capture
│   └── target/            # R2 TARGET capture
├── R0-origin-fsck.txt / R0-origin-status.txt
├── R1-push.log           # + policy-leak grep result
├── R2-zero-diff.txt      # gate T13-Z result
├── R3-zero-diff.txt      # + R3-origin-rehydrated/
├── R4-restore-equal.txt  # + R4-restored/
└── R5-conflict.log       # + parked-loser fsck output
```

**RESULTS.md skeleton:**
```markdown
# R0–R5 Evidence — <REPO> — <ORIGIN>→<TARGET> — <RUN_ID>
- Gate: G5 / TIN-1620 ; stop-rule clearance: TIN-2306 ; live child: TIN-1908
- Enrollment path: [M canonical-state | N nix-isolated-state]   ← 0.4 decision
- Versions: neo <ver> / honey <ver> ; #545 ancestor: [yes]
- Candidate hygiene: A worktree-pruned+on <trunk> / B node_modules fenced [yes]

| Rung | Rows proven | Result | Evidence file |
|------|-------------|--------|---------------|
| R0 | pre-gate, T13 | PASS/FAIL | R0-*.txt |
| R1 | T1,T2 | … | R1-push.log |
| R2 | T1,T2,T3,T12 (T13-Z) | … | R2-zero-diff.txt |
| R3 | T4,T5,T6,T8,M3,M6 | … | R3-zero-diff.txt |
| R4 | TIN-1620 rollback | … | R4-restore-equal.txt |
| R5 | T10,T11,M5,M5-R | … | R5-conflict.log |

- Policy-leak check: clean [yes]
- Agent-coupling (~/.claude/projects hash match): [done | DEFERRED — honey Claude auth dead, TIN-2653]
- Overall: PASS / FAIL / PARTIAL
```

---

## Stage 8 — Stop-rule verdict criteria (what makes bulk enrollment LEGAL) + abort paths

**Stop rule (verbatim, repo-roam-test-plan §8):** *"do not bulk-enroll all of `~/git` before two small repos pass R0-R5 in both directions."*
**Second clause (large-workdir-daily-driver-sequencing, 2026-07-05 banner):** *"no broad `~/git` or home takeover until two small repos clear R0-R5 both directions AND the merged divergent no-loss stack is deployed and proven by the live canary."*

**CLEARANCE (bulk enrollment becomes LEGAL) requires ALL of:**
1. **4 GREEN packets** — Dell-7810 {neo→honey, honey→neo} + site.scaffold {neo→honey, honey→neo}, each with R0–R5 all PASS, each archived dated.
2. Every R2 shows `dev-env-zero-diff=pass` and every R4 shows `restored==source`, `fsck` clean throughout.
3. Every R5 shows zero-loss keep-both with the loser-guard auto-park observed; policy-leak clean on all 4.
4. **Second clause satisfied:** the merged divergent no-loss stack (#513/#534/#542/#545) is confirmed *deployed live on both hosts* (Stage 0.1/0.2) and *proven by these live canaries* — not merely [PR] self-test green.
5. TIN-2306 acceptance-comment condition met (2026-07-04): live-fleet proof of the merged bidirectional FF logic exists.

**Even when CLEARED, the ONLY thing unlocked is:** repo-by-repo scoped multi-repo claims + R6 (bumble as third host). Broad `~/git` / `/tmp` / home takeover stays OUT of claim pending **TIN-1556** (stable root identity) + **TIN-1416/TIN-1416 subscriptions** (§9).

**FAILURE / ABORT paths:**
- **Stage-0 GATE fail** (neo pre-deploy, honey half-switched, ENOSPC, backbone red, fsck-harness red) → ABORT, no fleet mutation, escalate the named blocker (TIN-2519 for neo).
- **Any R2 non-zero-diff** → capture the delta (symlink=T12 / index=mtime), file a defect, mark packet FAIL. A single FAIL among the 4 = **stop-rule NOT cleared.**
- **Policy-leak hit** at any rung → ABORT that rung, scrub, do not archive the leaking packet.
- **R5 loss/corruption** (loser bytes lost, either `.git` fsck-dirty) → ABORT, this is a §7 corruption-gate regression; escalate Facet-6 harness owners.
- **Partial (TIN-1908 named-candidate) coverage:** if runs use substitute fixtures rather than TIN-1908's named repos, TIN-1908's Linear Acceptance is NOT auto-satisfied → see §9 + the 2026-07-08 truth-sweep: EITHER re-run against a named candidate OR **🔀 [DECISION]** formally amend TIN-1908 Acceptance to accept the substitute (tinyland-tool-daemon/#542) fixture. This runbook's chosen candidates (Dell-7810, site.scaffold) *are* two of TIN-1908's three named repos, so completing all 4 packets satisfies both TIN-2306 and TIN-1908's named-candidate requirement directly.

---

## Stage 9 — What this run does NOT claim

- **Does NOT claim broad `~/git`, `/tmp`, or home takeover.** Clearance unlocks only scoped, repo-by-repo multi-repo claims + R6 (bumble). Broad ownership stays gated on **TIN-1556** (stable root identity) + subscriptions (**TIN-1416**).
- **[PR] self-test green ≠ [LIVE] fleet green.** A green `task lazy:test-dev-env-fingerprint` / fsck-harness proves ONLY tool-internal consistency — NOT flip-flop zero-diff in either direction, NOT live `.git` corruption catching. Only the 4 LIVE packets count toward the verdict.
- **G5 (TIN-1620) is narrower than the stop rule.** G5 itself needs one expendable repo, two-machine browse/hydrate/unsync/rehydrate + conflict + rollback. The "two repos, both directions" bar is the **TIN-2306 layer on top of** G5, not G5 itself.
- **Agent-coupling sub-proof may be DEFERRED** if honey Claude auth is unrestored (TIN-2653) — record the deferral in every affected RESULTS.md; a deferred coupling proof means TIN-1908's "agent coupling" acceptance bullet is NOT yet closed even if R0–R5 are green.
- **Does not certify the nix-enrolled (`extraReconcileRoots`) production path** unless Option N was chosen — under default Option M, the runbook proves the manual `roam-<repo>.toml` + canonical-state path only; the scheduled-unit isolated-state design (and its `tcfs resolve` blindness, PLAN §0 surprise-6) is exercised only if N is run.
- **Does not resolve the ladder-definition divergence.** repo-roam-test-plan §6 is used as the operative *procedure*; TIN-1908's Acceptance rungs are conceptually convergent but not word-for-word identical (R1–R3 framing differs). This run satisfies TIN-1908 by using its named candidates, but does not itself reconcile the two documents.

---

### Executability notes for the supervising agent
- Run stages strictly in order; **halt at every ⛔ [GATE]** and surface it to the operator — do not self-authorize deploy state, auth, or the 0.4 enrollment-path decision.
- neo must **never** compile tcfs; offload any building `task lazy:*` to PZM/CI. Pure-shell harnesses (`git-dotgit-fsck-conflict-harness.sh`) are safe on neo.
- Confirm exact flag names (`--prefix`, `--path`, `--state`, `compare` subcommand, `--exclude`) against `tcfs … --help` / `scripts/repo-roam-fingerprint.sh --help` on the *deployed* binary before first push; the commands above are shaped to the documented behavior but the installed build is authoritative.
- All paths in evidence and push plans are absolute; archive packets on a writable branch, never in this read-only operator worktree.

**Key source files (all on origin/main):** `docs/ops/repo-roam-test-plan-2026-06-08.md` (§4 zero-code enroll, §6 ladder, §7 corruption gate, §8 stop rule), `docs/ops/large-workdir-daily-driver-sequencing-2026-05-30.md` (Gate Model + 2026-07-05 stop-rule banner), `docs/release/evidence/divergent-keep-both-canary-PLAN.md` (§0–§10 house style), `docs/release/evidence/b1-roam-enroll-readd-spec-20260608.md`, `scripts/repo-roam-fingerprint.sh`, `scripts/git-dotgit-fsck-conflict-harness.sh`, `scripts/neo-honey-unsynced-rehydrate-demo.sh`. Linear: TIN-2306 (clearance ticket, Backlog), TIN-1908 (live child, In Progress, blockedBy TIN-2519), TIN-1620 (G5), TIN-1556/TIN-1416 (broad-ownership gates).
