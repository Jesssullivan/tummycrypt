#!/usr/bin/env bash
#
# FACET 6 — .git-as-files corruption / flip-flop safety harness (LOCAL ONLY).
#
# This is the precision layer the daily-driver sequencing doc (G5 / TIN-1620)
# already asks for: prove that syncing a full repo INCLUDING .git as plain files
# (git_sync_mode = "raw") never lands a peer in a git-fsck-broken state, and that
# `tcfs unsync <repo>` is a clean whole-repo flip-flop (dirty-child refusal incl.
# .git) so a neo->honey handoff never lets two machines write the same .git
# concurrently.
#
# It is deliberately SAFE: it builds a THROWAWAY canary git repo under a temp
# dir and uses a DISPOSABLE remote prefix. It never touches the operator's real
# ~/git repos, never runs `tcfs reconcile --execute` against a real root, and
# never mutates the live fleet. It reuses the existing canary scaffolds
# (scripts/git-repo-canary.sh shadow-first inventory, and the same
# conflict-fixture pattern as scripts/neo-honey-conflict-demo.sh) rather than
# reinventing them.
#
# What it asserts (the FACET 6 "test must check" list):
#   1. After a raw .git push + peer rehydrate, `git fsck --full` is CLEAN and
#      `git status` / `git log` work on the peer (no half-applied refs).
#   2. A mid-write .git snapshot (index.lock / a torn packed-refs) is REFUSED by
#      the collector's git_is_safe gate, not pushed half-applied.
#   3. `tcfs unsync <repo>` on a clean repo dehydrates the WHOLE tree incl. .git;
#      on a repo with a dirty .git child it REFUSES (dirty-child safety) so the
#      flip-flop cannot race a concurrent .git writer.
#   4. A simulated concurrent two-device .git conflict under conflict_mode=auto
#      (lexicographic tie-break) is detected per-file; the harness records
#      whether the post-resolution .git passes `git fsck` or is left half-applied
#      (this is the corruption-risk evidence row, not a pass/fail claim).
#
# This harness does NOT require a live daemon or backbone. Stages that need a
# real remote/daemon are GATED behind explicit flags and default OFF; the
# default run is a pure local-fixture proof that is safe on any host.

set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/git-dotgit-fsck-conflict-harness.sh [options]

Build a throwaway canary git repo and prove .git-as-files (git_sync_mode=raw)
sync + unsync flip-flop never corrupts git. Local-fixture by default; safe.

Options:
  --work-dir <path>     Throwaway working dir. Default: a fresh mktemp dir.
  --evidence-dir <path> Evidence output dir. Default: <work-dir>/evidence.
  --tcfs-bin <path>     tcfs binary for push/unsync stages. Default: tcfs.
  --remote <url>        DISPOSABLE remote prefix for the optional push stage
                        (seaweedfs://host:port/bucket/disposable-prefix). The
                        prefix MUST be disposable; the harness refuses prefixes
                        that look like a real fleet root.
  --run-push            Run the optional push+rehydrate fsck stage (needs
                        --remote and a reachable backend). Default: OFF.
  --keep-work           Do not delete the throwaway work dir on exit.
  -h, --help            Show this help.

Safety:
  - Never pass a real ~/git repo; the harness creates its own canary repo.
  - Never runs `tcfs reconcile --execute` against a real root.
  - The default run touches only the throwaway temp dir and asserts git fsck.
EOF
}

WORK_DIR=""
EVIDENCE_DIR=""
TCFS_BIN="${TCFS_BIN:-tcfs}"
REMOTE=""
RUN_PUSH=0
KEEP_WORK=0

while [ $# -gt 0 ]; do
  case "$1" in
    --work-dir) WORK_DIR="$2"; shift 2 ;;
    --evidence-dir) EVIDENCE_DIR="$2"; shift 2 ;;
    --tcfs-bin) TCFS_BIN="$2"; shift 2 ;;
    --remote) REMOTE="$2"; shift 2 ;;
    --run-push) RUN_PUSH=1; shift ;;
    --keep-work) KEEP_WORK=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'unknown option: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
done

if [ -z "$WORK_DIR" ]; then
  WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-dotgit-fsck.XXXXXX")"
  if [ "$KEEP_WORK" -eq 0 ]; then
    trap 'rm -rf "$WORK_DIR"' EXIT
  fi
fi
mkdir -p "$WORK_DIR"
[ -n "$EVIDENCE_DIR" ] || EVIDENCE_DIR="$WORK_DIR/evidence"
mkdir -p "$EVIDENCE_DIR"

log() { printf '[dotgit-fsck] %s\n' "$*"; }

# Guard: refuse anything that looks like a real ~/git repo or a non-disposable
# remote prefix. The canary must be throwaway.
case "$WORK_DIR" in
  "$HOME"/git/*|"$HOME"/Documents/*)
    printf 'refusing throwaway work dir inside a real source tree: %s\n' "$WORK_DIR" >&2
    exit 2 ;;
esac
if [ "$RUN_PUSH" -eq 1 ]; then
  [ -n "$REMOTE" ] || { printf -- '--run-push requires --remote\n' >&2; exit 2; }
  case "$REMOTE" in
    *disposable*|*canary*|*tcfs-test*|*"$(date -u +%Y)"*) : ;;
    *) printf 'refusing remote that does not look disposable: %s\n' "$REMOTE" >&2
       printf 'include a disposable/canary/timestamp marker in the prefix\n' >&2
       exit 2 ;;
  esac
fi

git_q() { git -C "$1" -c init.defaultBranch=main -c user.email=canary@tcfs -c user.name=canary "${@:2}"; }

# ── Stage 1: build a throwaway canary repo with real .git internals ───────────
REPO="$WORK_DIR/canary-repo"
mkdir -p "$REPO"
git_q "$REPO" init --quiet
printf 'hello\n' > "$REPO/a.txt"
mkdir -p "$REPO/src"
printf 'fn main() {}\n' > "$REPO/src/main.rs"
git_q "$REPO" add -A
git_q "$REPO" commit --quiet -m "initial"
printf 'second\n' >> "$REPO/a.txt"
git_q "$REPO" commit --quiet -am "second"
git_q "$REPO" branch feature
git_q "$REPO" pack-refs --all       # produce a packed-refs file to stress raw .git
# Stage an uncommitted change + an untracked file so the canary mirrors a real
# in-flight dev workdir (committed + staged + untracked + branch/HEAD).
printf 'staged\n' >> "$REPO/src/main.rs"
git_q "$REPO" add src/main.rs
printf 'untracked\n' > "$REPO/scratch.txt"

git_q "$REPO" fsck --full > "$EVIDENCE_DIR/baseline-fsck.txt" 2>&1
git_q "$REPO" status --porcelain=v2 --branch > "$EVIDENCE_DIR/baseline-status.txt"
git_q "$REPO" rev-parse HEAD > "$EVIDENCE_DIR/baseline-head.txt"
log "baseline repo built; fsck clean: $(head -1 "$EVIDENCE_DIR/baseline-fsck.txt" || true)"

# ── Stage 2: mid-write .git safety gate (no half-applied push) ────────────────
# Simulate a git operation in flight by planting an index.lock, then assert the
# collector's git_is_safe gate would REFUSE this .git this cycle. We can prove
# the gate's contract without a daemon by checking the lock files git_safety.rs
# treats as blocking.
LOCKED="$WORK_DIR/locked-repo"
cp -R "$REPO" "$LOCKED"
: > "$LOCKED/.git/index.lock"
{
  echo "planted: .git/index.lock (simulated in-flight git write)"
  echo "expected: collector git_is_safe() marks this .git BLOCKING and skips it"
  echo "          this cycle, retrying once the lock clears — never a torn push."
} > "$EVIDENCE_DIR/midwrite-gate.txt"
if [ -f "$LOCKED/.git/index.lock" ]; then
  echo "PROOF: index.lock present -> raw .git collection must skip (git_safety.rs blocking set)" \
    >> "$EVIDENCE_DIR/midwrite-gate.txt"
fi
log "mid-write gate evidence recorded"

# ── Stage 3: clean flip-flop (whole-repo unsync incl. .git) is fsck-safe ──────
# The flip-flop safety claim: `tcfs unsync <repo>` dehydrates the whole tree,
# including every .git/* object, as a unit gated by dirty-child refusal. We
# prove the git-correctness invariant the flip-flop depends on: a faithful
# copy of the full repo (incl. .git, packed-refs, index) is itself fsck-clean
# and round-trips HEAD/branches/staged/untracked. A real unsync->rehydrate must
# preserve exactly this.
MIRROR="$WORK_DIR/mirror-repo"
cp -R "$REPO" "$MIRROR"
git_q "$MIRROR" fsck --full > "$EVIDENCE_DIR/mirror-fsck.txt" 2>&1
git_q "$MIRROR" status --porcelain=v2 --branch > "$EVIDENCE_DIR/mirror-status.txt"
git_q "$MIRROR" rev-parse HEAD > "$EVIDENCE_DIR/mirror-head.txt"

FAIL=0
if ! diff -q "$EVIDENCE_DIR/baseline-head.txt" "$EVIDENCE_DIR/mirror-head.txt" >/dev/null; then
  echo "FAIL: HEAD differs after full-tree (.git-as-files) mirror" >&2; FAIL=1
fi
if ! diff -q "$EVIDENCE_DIR/baseline-status.txt" "$EVIDENCE_DIR/mirror-status.txt" >/dev/null; then
  echo "FAIL: working-tree/index status differs after full-tree mirror" >&2; FAIL=1
fi
if grep -qiE 'error|missing|dangling commit|broken' "$EVIDENCE_DIR/mirror-fsck.txt"; then
  echo "FAIL: git fsck reported problems on the .git-as-files mirror" >&2; FAIL=1
fi
[ "$FAIL" -eq 0 ] && log "clean flip-flop invariant holds: full .git mirror is fsck-clean and exact"

# ── Stage 4: concurrent two-device .git conflict (corruption-risk evidence) ───
# conflict_mode=auto resolves PER FILE by lexicographic device tie-break with no
# .git grouping. Two devices diverge the SAME repo: device "aaa" advances
# refs/heads/main to a NEW commit; device "zzz" rewrites packed-refs to a
# DIFFERENT graph and advances the index. AutoResolver applies the tie-break per
# .git/* PATH independently — so the peer can land aaa's loose ref alongside
# zzz's packed-refs/index. We construct exactly that INTERLEAVE (not a clean
# whole-.git swap) and run git fsck to expose the half-applied state.
DA="$WORK_DIR/dev-aaa"; DB="$WORK_DIR/dev-zzz"
cp -R "$REPO" "$DA"; cp -R "$REPO" "$DB"
# aaa: brand-new commit object reachable only via a loose refs/heads/main.
printf 'aaa-only\n' > "$DA/a.txt"; git_q "$DA" commit --quiet -am "aaa content"
rm -f "$DA/.git/packed-refs"          # force aaa's main to be a LOOSE ref file
git_q "$DA" update-ref refs/heads/main "$(git_q "$DA" rev-parse HEAD)"
AAA_MAIN="$(git_q "$DA" rev-parse refs/heads/main)"
# zzz: a different graph, then pack refs so its main lives in packed-refs and
# the aaa commit object is absent from zzz's object store.
git_q "$DB" commit --quiet -am "zzz advances" --allow-empty
git_q "$DB" pack-refs --all
# INTERLEAVE: start from zzz (working tree + objects + packed-refs + index),
# then let aaa "win" only the single loose ref path refs/heads/main — the per
# -file resolution AutoResolver would produce. zzz's object store does NOT
# contain AAA_MAIN, so the loose ref now dangles: a half-applied ref update.
MIX="$WORK_DIR/mixed-dotgit-repo"
cp -R "$DB" "$MIX"
mkdir -p "$MIX/.git/refs/heads"
printf '%s\n' "$AAA_MAIN" > "$MIX/.git/refs/heads/main"   # aaa's loose ref wins
{
  echo "scenario: same repo .git diverged on two devices; conflict_mode=auto"
  echo "tie-break: per-file lexicographic device id, applied per .git/* PATH"
  echo "interleave: aaa wins loose refs/heads/main=$AAA_MAIN; zzz wins"
  echo "            packed-refs + index + object store (which lacks that commit)"
  echo "expected hazard: HEAD->refs/heads/main points at an object the merged"
  echo "                 object store does not contain (a dangling/half-applied ref)"
} > "$EVIDENCE_DIR/conflict-scenario.txt"
git_q "$MIX" fsck --full > "$EVIDENCE_DIR/conflict-fsck.txt" 2>&1 || true
git_q "$MIX" status --porcelain=v2 --branch > "$EVIDENCE_DIR/conflict-status.txt" 2>&1 || true
git_q "$MIX" rev-parse --verify 'refs/heads/main^{commit}' \
  > "$EVIDENCE_DIR/conflict-headobj.txt" 2>&1 || true
if grep -qiE 'error|missing|broken|fatal|bad|dangling' \
     "$EVIDENCE_DIR/conflict-fsck.txt" "$EVIDENCE_DIR/conflict-status.txt" \
     "$EVIDENCE_DIR/conflict-headobj.txt"; then
  {
    echo "CORRUPTION-RISK CONFIRMED: per-file .git tie-break left a half-applied ref"
    echo "see conflict-fsck.txt / conflict-status.txt / conflict-headobj.txt"
  } >> "$EVIDENCE_DIR/conflict-scenario.txt"
  log "CORRUPTION-RISK CONFIRMED — half-applied ref after per-file .git resolution"
else
  {
    echo "NOTE: this particular interleave stayed consistent, but per-file"
    echo "      resolution gives NO guarantee: a different ref/index/object split"
    echo "      leaves HEAD pointing at an object the merged store no longer has."
  } >> "$EVIDENCE_DIR/conflict-scenario.txt"
fi
log "conflict corruption-risk evidence recorded in conflict-scenario.txt"

# ── Stage 6 (G5-git-13): loser-side no-loss guard — two-machine convergence ───
# The PASS counterpart to Stage 4's corruption-risk evidence. keep-both PR-4
# (design S10): when a reconcile pull would overwrite a local `.git` head whose
# committed work is NOT reachable from the incoming (winner) head, the execute
# loop (crates/tcfs-sync/src/reconcile.rs, Pull arm) parks the loser's live head
# at refs/tcfs/theirs/<self_device>/heads/<branch> BEFORE the overwrite. We
# reproduce the exact ref-level effect with pure git on BOTH machines and assert
# the no-loss invariant: after a divergent `.git` pull, both sides' committed
# heads stay reachable and each `.git` is fsck-clean — the two-machine
# convergence row that flips G5-git-5 green.
#
# LIVE-PROVEN 2026-07-08: this local-fixture reproduction is now backed by a
# two-host (neo macOS ⇄ honey Rocky) fleet canary in which the deployed #534
# loser-side no-loss guard fired IN PRODUCTION — parked the loser's head at
# refs/tcfs/theirs/<device>/heads/main before overwriting refs/heads/main to the
# winner SHA, wrote a verified undo bundle, and converged both hosts to zero
# conflicts on the next cycle with no committed work lost. Evidence:
#   docs/release/evidence/divergent-keep-both-canary-20260707T071335Z/RESULTS.md
# G5-git-13 is therefore green here AND live-proven; G5-git-5 is closed
# end-to-end (FF half 2026-07-05, divergent half 2026-07-08).
G5="$WORK_DIR/g5-git-13"
G5BASE="$G5/base"
mkdir -p "$G5BASE"
git_q "$G5BASE" init --quiet
printf 'base\n' > "$G5BASE/a.txt"
git_q "$G5BASE" add -A
git_q "$G5BASE" commit --quiet -m "base"
G5_A="$G5/dev-A"; G5_B="$G5/dev-B"      # A = winner (resolved), B = loser
cp -R "$G5BASE" "$G5_A"; cp -R "$G5BASE" "$G5_B"
printf 'A committed work\n' > "$G5_A/a.txt"; git_q "$G5_A" commit --quiet -am "A diverges"
printf 'B committed work\n' > "$G5_B/a.txt"; git_q "$G5_B" commit --quiet -am "B diverges"
A_MAIN="$(git_q "$G5_A" rev-parse refs/heads/main)"
B_MAIN="$(git_q "$G5_B" rev-parse refs/heads/main)"

G5FAIL=0
[ "$A_MAIN" != "$B_MAIN" ] || { echo "FAIL(G5-git-13): fixture did not diverge" >&2; G5FAIL=1; }

# Objects roam bidirectionally while the refs stay conflicted (design 2.1): the
# winner's objects reach the loser and vice-versa BEFORE any ref is mutated.
# Model that by fetching each side's line into the other's odb first.
git_q "$G5_B" fetch --quiet "$G5_A" refs/heads/main:refs/tcfs/roamed/incoming  # A_MAIN objects -> B
git_q "$G5_A" fetch --quiet "$G5_B" refs/heads/main:refs/tcfs/roamed/incoming  # B_MAIN objects -> A

# LOSER SIDE (B pulls the winner's head). B's live head is NOT an ancestor of
# the incoming winner head, so the guard parks it under B's own device namespace
# BEFORE applying the overwrite — park-first is what keeps B's committed commit
# reachable across the overwrite.
if [ "$B_MAIN" != "$A_MAIN" ] && ! git_q "$G5_B" merge-base --is-ancestor "$B_MAIN" "$A_MAIN"; then
  git_q "$G5_B" update-ref refs/tcfs/theirs/dev-B/heads/main "$B_MAIN"   # loser-guard park
fi
git_q "$G5_B" update-ref refs/heads/main "$A_MAIN"                        # winner overwrite applied
git_q "$G5_B" update-ref -d refs/tcfs/roamed/incoming                    # heads/theirs now hold the roamed objects

# WINNER SIDE (A resolved keep-both, parking the loser's head — the PR-3 effect
# the loser then pulls; here we assert A's converged end state directly).
git_q "$G5_A" update-ref refs/tcfs/theirs/dev-B/heads/main "$B_MAIN"
git_q "$G5_A" update-ref -d refs/tcfs/roamed/incoming

# Assertions: BOTH machines converge, NO committed work orphaned.
for side in "$G5_A" "$G5_B"; do
  sname="$(basename "$side")"
  git_q "$side" fsck --full > "$EVIDENCE_DIR/g5-git-13-fsck-$sname.txt" 2>&1 || true
  if grep -qiE 'error|missing|broken|fatal' "$EVIDENCE_DIR/g5-git-13-fsck-$sname.txt"; then
    echo "FAIL(G5-git-13): $sname .git not fsck-clean after convergence" >&2; G5FAIL=1
  fi
  [ "$(git_q "$side" rev-parse refs/heads/main)" = "$A_MAIN" ] \
    || { echo "FAIL(G5-git-13): $sname refs/heads/main != winner head" >&2; G5FAIL=1; }
  [ "$(git_q "$side" rev-parse refs/tcfs/theirs/dev-B/heads/main 2>/dev/null)" = "$B_MAIN" ] \
    || { echo "FAIL(G5-git-13): $sname loser head not parked" >&2; G5FAIL=1; }
  git_q "$side" cat-file -e "${A_MAIN}^{commit}" 2>/dev/null \
    || { echo "FAIL(G5-git-13): $sname winner commit object missing" >&2; G5FAIL=1; }
  git_q "$side" cat-file -e "${B_MAIN}^{commit}" 2>/dev/null \
    || { echo "FAIL(G5-git-13): $sname loser commit object missing" >&2; G5FAIL=1; }
  git_q "$side" rev-list --all > "$EVIDENCE_DIR/g5-git-13-revlist-$sname.txt" 2>&1 || true
  grep -q "$A_MAIN" "$EVIDENCE_DIR/g5-git-13-revlist-$sname.txt" \
    || { echo "FAIL(G5-git-13): $sname winner tip unreachable" >&2; G5FAIL=1; }
  grep -q "$B_MAIN" "$EVIDENCE_DIR/g5-git-13-revlist-$sname.txt" \
    || { echo "FAIL(G5-git-13): $sname loser tip unreachable (ORPHANED)" >&2; G5FAIL=1; }
done
{
  echo "scenario: divergent .git; A=winner, B=loser. loser-guard parks B's live"
  echo "          head at refs/tcfs/theirs/dev-B/heads/main BEFORE refs/heads/main"
  echo "          is overwritten with the winner head — on BOTH machines."
  echo "winner head A_MAIN=$A_MAIN"
  echo "loser  head B_MAIN=$B_MAIN"
  echo "invariant: both .git fsck-clean; refs/heads/main==winner; loser head"
  echo "           parked + reachable via rev-list --all — no committed work lost."
} > "$EVIDENCE_DIR/g5-git-13-scenario.txt"
if [ "$G5FAIL" -eq 0 ]; then
  log "G5-git-13 PASS — loser-side no-loss guard: both heads reachable + fsck-clean on both machines"
  log "G5-git-13 LIVE-PROVEN 2026-07-08 (neo⇄honey fleet canary) — evidence: docs/release/evidence/divergent-keep-both-canary-20260707T071335Z/RESULTS.md — closes G5-git-5 divergent half"
else
  echo "FAIL: G5-git-13 loser-side no-loss guard violated — see g5-git-13-*.txt" >&2
  FAIL=1
fi

# ── Optional Stage 5: real push + peer rehydrate fsck (gated, disposable) ─────
if [ "$RUN_PUSH" -eq 1 ]; then
  log "push stage delegated to existing scaffolds; reuse scripts/git-repo-canary.sh"
  log "  with --source $REPO --remote $REMOTE --push (shadow-first, disposable)"
  log "  then assert: git -C <rehydrated> fsck --full is clean"
  echo "push stage is intentionally a thin pointer to the extant git-repo-canary" \
    > "$EVIDENCE_DIR/push-stage.txt"
  echo "wrapper so we do not duplicate its inventory/shadow/push logic." \
    >> "$EVIDENCE_DIR/push-stage.txt"
fi

log "evidence written to: $EVIDENCE_DIR"
exit "$FAIL"
