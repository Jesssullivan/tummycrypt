#!/usr/bin/env bash
#
# repo-roam-fingerprint.sh — git-aware dev-env "zero-diff" fingerprint for the
# TCFS large-workdir repo-roam ladder (Gate G5 / TIN-1620 / TIN-1908).
#
# This is the ONE missing precision layer the research identified: it captures a
# complete git-SEMANTIC dev-env fingerprint of a repo (status, HEAD, branch,
# staged/unstaged diffs, stash, untracked, per-file modes, symlink targets,
# `git fsck --full`, and a sha256 manifest of tracked+untracked working files)
# and a `compare` mode that exits NONZERO on ANY difference.
#
# It PLUGS INTO the existing canary/evidence scaffold; it does NOT duplicate it:
#   * inventory/shadow/push/honey            -> scripts/git-repo-canary.sh
#                                               (-> scripts/home-canary-linux-xr-shadow.sh)
#   * fresh-tree restore / rollback (G5)     -> scripts/git-repo-restore-proof.sh
#   * cross-host flip-flop lifecycle (T/M)   -> the TIN-1620 flip-flop + neo-honey
#                                               demo harnesses
#   * read-only inventory                    -> scripts/large-workdir-inventory.py
# The fingerprint emits one new evidence subtree, `dev-env-fingerprint/`, and one
# gate line (`dev-env-zero-diff=pass|fail`) to thread into the packet's
# result.env / parity-gates.env — it is an ADDITIONAL assertion layered on QA
# rows T2/T3 (exact bytes), T8/T9 + M3/M6 (peer-edit rehydrate), and
# T10/T11 + M5/M5-R (conflict / keep-both), NOT a replacement for any row.
#
# Modes:
#   capture <repo> <out_dir>   Write a fingerprint of <repo> into <out_dir>.
#   compare <dir_a> <dir_b>    Diff two captured fingerprints; exit 1 on any diff.
#   seed-canary <dir>          Build a THROWAWAY repo with a realistic in-progress
#                              dev state (feature branch + staged + unstaged +
#                              untracked + stash + exec script + symlink).
#   self-test [<tmp>]          Seed -> capture -> capture -> compare round-trip in
#                              a disposable temp dir. Never touches a real repo.
#
# Safety: capture is READ-ONLY (it runs `git fsck`/`git status`/`git diff` and a
# plain-file sha256 walk; it never writes into <repo>). seed-canary refuses to
# operate on $HOME, anything under ~/git, or a filesystem root.
#
# Deny-set / noise control: the working-file manifest honors the SAME fail-closed
# deny-set posture as the reconcile engine (Blacklist::from_sync_config:
# .env*/secret/live-WAL-db never enter a push plan or manifest) plus the usual
# build-output excludes (target/, node_modules/, .direnv/) so the fingerprint
# stays deterministic and does not leak secrets into evidence.
#
set -euo pipefail

PROG="repo-roam-fingerprint"

usage() {
  cat <<'EOF'
Usage:
  scripts/repo-roam-fingerprint.sh capture <repo> <out_dir>
  scripts/repo-roam-fingerprint.sh compare <dir_a> <dir_b>
  scripts/repo-roam-fingerprint.sh seed-canary <dir>
  scripts/repo-roam-fingerprint.sh self-test [<tmp_dir>]

Modes:
  capture       Read-only git-aware dev-env fingerprint of <repo> into <out_dir>.
  compare       Diff two fingerprints; exit 1 (and print the diff) on any change.
  seed-canary   Build a throwaway repo with a realistic dirty/in-progress state.
  self-test     Disposable seed -> capture -> capture -> compare round-trip.

Environment:
  REPO_ROAM_FP_EXTRA_EXCLUDES   space-separated extra top-level dir names to omit
                                from the working-file manifest (defaults already
                                exclude target node_modules .direnv dist build).
EOF
}

fail() {
  printf '%s: error: %s\n' "$PROG" "$*" >&2
  exit 1
}

# Portable sha256 of a single file -> bare hex digest on stdout.
sha256_file() {
  local f="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -- "$f" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -- "$f" | awk '{print $1}'
  else
    fail "need sha256sum or shasum"
  fi
}

# Portable sha256 of stdin -> bare hex digest on stdout.
sha256_stdin() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 | awk '{print $1}'
  else
    fail "need sha256sum or shasum"
  fi
}

# Portable file mode (octal) -> e.g. 100644 / 100755 / 120000-style 0oNNNN.
file_mode() {
  local f="$1"
  if stat -f '%Lp' "$f" >/dev/null 2>&1; then
    stat -f '%Lp' "$f"           # BSD / macOS
  else
    stat -c '%a' "$f"            # GNU / Linux
  fi
}

# Canonicalize a path even when its leaf does not yet exist, by resolving the
# nearest existing ancestor and re-appending the missing tail. Used by the
# seed-canary safety guard so it can refuse $HOME / ~/git targets pre-mkdir.
resolve_intended_path() {
  local p="$1"
  if [[ -e "$p" ]]; then
    ( cd "$p" 2>/dev/null && pwd -P ) || printf '%s\n' "$p"
    return
  fi
  local parent base
  parent="$(dirname -- "$p")"
  base="$(basename -- "$p")"
  if [[ -d "$parent" ]]; then
    printf '%s/%s\n' "$(cd "$parent" && pwd -P)" "$base"
  else
    printf '%s\n' "$p"
  fi
}

# Top-level dir names that never belong in a deterministic working-file manifest.
default_excludes() {
  printf '%s\n' target node_modules .direnv dist build .next .turbo
}

# Fail-closed deny-set patterns mirrored from Blacklist::from_sync_config so the
# fingerprint manifest never records a secret/live-WAL path that the reconcile
# engine itself refuses to push.
is_denied_relpath() {
  local rel="$1"
  local base="${rel##*/}"
  case "$base" in
    .env|.env.*|*.pem|*.key|id_rsa|id_ed25519|*.sqlite|*.sqlite-wal|*.sqlite-shm|*.db-wal|*.db-shm)
      return 0 ;;
  esac
  case "$rel" in
    .ssh/*|.gnupg/*|*/secrets/*) return 0 ;;
  esac
  return 1
}

# -----------------------------------------------------------------------------
# capture
# -----------------------------------------------------------------------------
cmd_capture() {
  local repo="$1"
  local out="$2"
  [[ -n "$repo" && -n "$out" ]] || { usage >&2; exit 2; }
  [[ -d "$repo" ]] || fail "repo does not exist: $repo"
  git -C "$repo" rev-parse --is-inside-work-tree >/dev/null 2>&1 \
    || fail "not a git worktree: $repo"

  local repo_canon
  repo_canon="$(cd "$repo" && pwd -P)"
  mkdir -p "$out"
  out="$(cd "$out" && pwd -P)"

  local g=(git -C "$repo_canon")

  # --- raw git-state captures (deterministic, host-path-independent) ----------
  # status: porcelain=v2 --branch distinguishes index vs worktree and records
  # the checked-out branch + ahead/behind. -z for stable ordering.
  "${g[@]}" -c core.quotepath=false status --porcelain=v2 --branch -z 2>/dev/null \
    | tr '\0' '\n' >"$out/status.txt" || true

  # HEAD + branch (detached-HEAD safe).
  {
    printf 'head=%s\n' "$("${g[@]}" rev-parse --verify HEAD 2>/dev/null || echo NONE)"
    printf 'symbolic_ref=%s\n' "$("${g[@]}" symbolic-ref -q HEAD 2>/dev/null || echo DETACHED)"
    printf 'branch=%s\n' "$("${g[@]}" rev-parse --abbrev-ref HEAD 2>/dev/null || echo NONE)"
    printf 'write_tree=%s\n' "$("${g[@]}" write-tree 2>/dev/null || echo NONE)"
  } >"$out/head.env"

  # All refs + branch set (so HEAD/branch/refs round-trip is asserted).
  "${g[@]}" show-ref 2>/dev/null | sort >"$out/refs.txt" || true
  "${g[@]}" branch -a --format='%(refname)' 2>/dev/null | sort >"$out/branches.txt" || true

  # Staged (index) vs unstaged (worktree) content, captured as stable hashes.
  printf '%s' "$("${g[@]}" diff --cached 2>/dev/null || true)" \
    | sha256_stdin >"$out/diff-cached.sha256" || true
  printf '%s' "$("${g[@]}" diff 2>/dev/null || true)" \
    | sha256_stdin >"$out/diff-worktree.sha256" || true
  # Per-path staged blob shas (index identity, mode-aware).
  "${g[@]}" ls-files -s 2>/dev/null | sort >"$out/index-blobs.txt" || true

  # Untracked + stash + reflog tip.
  "${g[@]}" -c core.quotepath=false ls-files --others --exclude-standard 2>/dev/null \
    | sort >"$out/untracked.txt" || true
  "${g[@]}" stash list --format='%gd %s' 2>/dev/null >"$out/stash-list.txt" || true
  printf 'stash_ref=%s\n' "$("${g[@]}" rev-parse -q --verify refs/stash 2>/dev/null || echo NONE)" \
    >"$out/stash.env"
  "${g[@]}" reflog --format='%H %gs' 2>/dev/null | head -n 50 >"$out/reflog.txt" || true

  # git fsck --full: the corruption gate. A mid-reconcile / torn .git must NOT
  # produce a clean fsck. We record the raw output AND a normalized verdict.
  local fsck_out="$out/fsck.txt"
  "${g[@]}" fsck --full 2>&1 | sort >"$fsck_out" || true
  local fsck_bad
  fsck_bad="$(grep -ciE 'invalid sha1 pointer|missing|broken|dangling.*commit|error:|fatal:' "$fsck_out" || true)"
  if [[ "$fsck_bad" -eq 0 ]]; then
    printf 'fsck=clean\n' >"$out/fsck.env"
  else
    printf 'fsck=dirty\n' >"$out/fsck.env"
  fi

  # --- working-file manifest (tracked + untracked) ----------------------------
  # relpath<TAB>mode<TAB>sha256(or symlink target). Honors the deny-set + build
  # excludes so the manifest is deterministic and secret-free. Sorted for a
  # stable, host-independent compare.
  # default excludes plus any space-separated REPO_ROAM_FP_EXTRA_EXCLUDES, one
  # top-level dir name per line (tr turns the space-separated env list into lines).
  local excludes
  excludes="$(default_excludes)"$'\n'"$(printf '%s' "${REPO_ROAM_FP_EXTRA_EXCLUDES:-}" | tr ' ' '\n')"
  local manifest="$out/working-manifest.tsv"
  : >"$manifest"

  # Enumerate tracked + untracked (not ignored) files via git, NUL-delimited.
  local rel abs top
  while IFS= read -r -d '' rel; do
    [[ -n "$rel" ]] || continue
    top="${rel%%/*}"
    if printf '%s\n' "$excludes" | grep -qxF -- "$top"; then
      continue
    fi
    if is_denied_relpath "$rel"; then
      printf '%s\tDENIED\tdeny-set\n' "$rel" >>"$manifest"
      continue
    fi
    abs="$repo_canon/$rel"
    if [[ -L "$abs" ]]; then
      local tgt
      tgt="$(readlink -- "$abs" 2>/dev/null || echo UNREADABLE)"
      printf '%s\t120000\tsymlink:%s\n' "$rel" "$tgt" >>"$manifest"
    elif [[ -f "$abs" ]]; then
      printf '%s\t%s\t%s\n' "$rel" "$(file_mode "$abs")" "$(sha256_file "$abs")" >>"$manifest"
    fi
  done < <(
    {
      git -C "$repo_canon" ls-files -z 2>/dev/null
      git -C "$repo_canon" ls-files --others --exclude-standard -z 2>/dev/null
    }
  )

  LC_ALL=C sort -o "$manifest" "$manifest"

  # --- the single fingerprint digest ------------------------------------------
  # One content-addressed digest over every git-semantic capture above. compare
  # mode diffs the component files for a human-readable delta; this digest is the
  # one-line gate signal.
  local fp
  fp="$(
    {
      cat "$out/status.txt" "$out/head.env" "$out/refs.txt" "$out/branches.txt" \
          "$out/diff-cached.sha256" "$out/diff-worktree.sha256" "$out/index-blobs.txt" \
          "$out/untracked.txt" "$out/stash-list.txt" "$out/stash.env" \
          "$out/fsck.env" "$manifest" 2>/dev/null
    } | sha256_stdin
  )"

  {
    printf 'tool=%s\n' "$PROG"
    printf 'captured_at_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'repo=%s\n' "$repo_canon"
    printf 'head=%s\n' "$("${g[@]}" rev-parse --verify HEAD 2>/dev/null || echo NONE)"
    printf 'branch=%s\n' "$("${g[@]}" rev-parse --abbrev-ref HEAD 2>/dev/null || echo NONE)"
    printf 'status_entries=%s\n' "$("${g[@]}" status --porcelain=v1 2>/dev/null | wc -l | tr -d ' ')"
    printf 'untracked_entries=%s\n' "$(wc -l <"$out/untracked.txt" | tr -d ' ')"
    printf 'stash_entries=%s\n' "$(wc -l <"$out/stash-list.txt" | tr -d ' ')"
    printf 'manifest_entries=%s\n' "$(wc -l <"$manifest" | tr -d ' ')"
    cat "$out/fsck.env"
    printf 'fingerprint=%s\n' "$fp"
  } >"$out/fingerprint.env"

  printf '%s: captured fingerprint of %s -> %s\n' "$PROG" "$repo_canon" "$out"
  printf 'fingerprint=%s\n' "$fp"
}

# -----------------------------------------------------------------------------
# compare
# -----------------------------------------------------------------------------
cmd_compare() {
  local a="$1"
  local b="$2"
  [[ -n "$a" && -n "$b" ]] || { usage >&2; exit 2; }
  [[ -f "$a/fingerprint.env" ]] || fail "no fingerprint at: $a"
  [[ -f "$b/fingerprint.env" ]] || fail "no fingerprint at: $b"

  local fa fb
  fa="$(grep '^fingerprint=' "$a/fingerprint.env" | head -n1 | cut -d= -f2-)"
  fb="$(grep '^fingerprint=' "$b/fingerprint.env" | head -n1 | cut -d= -f2-)"

  # fsck must be clean on BOTH sides — a clean dev env requires a consistent
  # .git on the roamed-to host, not just per-file byte parity.
  local fsck_a fsck_b
  fsck_a="$(grep '^fsck=' "$a/fingerprint.env" | head -n1 | cut -d= -f2-)"
  fsck_b="$(grep '^fsck=' "$b/fingerprint.env" | head -n1 | cut -d= -f2-)"

  local components=(
    status.txt head.env refs.txt branches.txt diff-cached.sha256
    diff-worktree.sha256 index-blobs.txt untracked.txt stash-list.txt
    stash.env fsck.env working-manifest.tsv
  )
  local diff_out="${REPO_ROAM_FP_COMPARE_DIFF:-}"
  local tmp_diff
  tmp_diff="$(mktemp "${TMPDIR:-/tmp}/repo-roam-fp-compare.XXXXXX")"
  local f
  for f in "${components[@]}"; do
    if [[ -f "$a/$f" || -f "$b/$f" ]]; then
      diff -u "$a/$f" "$b/$f" >>"$tmp_diff" 2>&1 || true
    fi
  done

  local status=pass
  if [[ "$fa" != "$fb" ]]; then status=fail; fi
  if [[ "$fsck_a" != "clean" || "$fsck_b" != "clean" ]]; then status=fail; fi

  if [[ -n "$diff_out" ]]; then
    cp "$tmp_diff" "$diff_out"
  fi

  if [[ "$status" == "pass" ]]; then
    printf '%s: dev-env-zero-diff=pass (fingerprints match, fsck clean both sides)\n' "$PROG"
    printf 'dev-env-zero-diff=pass\n'
    rm -f "$tmp_diff"
    return 0
  fi

  printf '%s: dev-env-zero-diff=FAIL\n' "$PROG" >&2
  printf '  fingerprint A=%s\n  fingerprint B=%s\n' "$fa" "$fb" >&2
  printf '  fsck A=%s  fsck B=%s\n' "$fsck_a" "$fsck_b" >&2
  if [[ -s "$tmp_diff" ]]; then
    printf '  --- component diff ---\n' >&2
    sed 's/^/  /' "$tmp_diff" >&2
  fi
  printf 'dev-env-zero-diff=fail\n'
  rm -f "$tmp_diff"
  return 1
}

# -----------------------------------------------------------------------------
# seed-canary — throwaway repo with a realistic in-progress dev state.
# -----------------------------------------------------------------------------
cmd_seed_canary() {
  local dir="$1"
  [[ -n "$dir" ]] || { usage >&2; exit 2; }

  # Safety: never operate on a real working tree. Resolve the INTENDED target
  # even when the leaf does not yet exist (canonicalize its parent), and resolve
  # $HOME the same way, so the guard catches $HOME and ~/git/* before any mkdir
  # regardless of /tmp -> /private/tmp style symlinks.
  case "$dir" in
    /) fail "refusing to seed at filesystem root" ;;
  esac
  local dir_canon home_canon git_root
  dir_canon="$(resolve_intended_path "$dir")"
  home_canon="$(resolve_intended_path "$HOME")"
  git_root="$home_canon/git"
  # Compare both the resolved form and the raw literal (covers a non-existent
  # $HOME that cannot be canonicalized).
  if [[ "$dir_canon" == "$home_canon" || "$dir" == "$HOME" ]]; then
    fail "refusing to seed canary at \$HOME"
  fi
  case "$dir_canon" in
    "$git_root"|"$git_root"/*) fail "refusing to seed canary under ~/git (real repos)" ;;
  esac
  case "$dir" in
    "$HOME"/git|"$HOME"/git/*) fail "refusing to seed canary under ~/git (real repos)" ;;
  esac

  mkdir -p "$dir"
  dir="$(cd "$dir" && pwd -P)"

  git -C "$dir" init -q -b main
  git -C "$dir" config user.name "TCFS Roam Canary"
  git -C "$dir" config user.email "tcfs-roam-canary@example.invalid"
  git -C "$dir" config commit.gpgsign false

  # Committed history.
  printf '# roam canary\n\nthrowaway fixture\n' >"$dir/README.md"
  mkdir -p "$dir/src"
  printf 'fn main() {}\n' >"$dir/src/main.rs"
  git -C "$dir" add README.md src/main.rs
  git -C "$dir" commit -q -m "initial canary commit"
  printf 'committed-line\n' >>"$dir/src/main.rs"
  git -C "$dir" commit -qam "second canary commit"

  # Feature branch checked out (branch/HEAD must round-trip).
  git -C "$dir" checkout -q -b feature/in-progress

  # A stash (refs/stash + reflog must round-trip).
  printf 'work-to-stash\n' >>"$dir/README.md"
  git -C "$dir" stash push -q -m "canary wip stash"

  # Staged change (index / git diff --cached).
  printf 'staged-line\n' >>"$dir/src/main.rs"
  git -C "$dir" add src/main.rs

  # Unstaged change on a tracked file (worktree / git diff).
  printf '\nunstaged tail\n' >>"$dir/README.md"

  # Untracked file.
  printf 'untracked scratch\n' >"$dir/NOTES.txt"

  # Executable script (mode/exec bit must round-trip — T13 modes half).
  printf '#!/usr/bin/env bash\necho canary\n' >"$dir/run.sh"
  chmod +x "$dir/run.sh"
  git -C "$dir" add run.sh

  # Symlink (T12 symlink parity — the reconcile collect path drops these today
  # unless preserve_symlinks is opted in; the fingerprint makes that visible).
  ln -s README.md "$dir/README.link"

  printf '%s: seeded throwaway canary repo at %s\n' "$PROG" "$dir"
  printf '  branch=%s head=%s\n' \
    "$(git -C "$dir" rev-parse --abbrev-ref HEAD)" \
    "$(git -C "$dir" rev-parse --short HEAD)"
}

# -----------------------------------------------------------------------------
# self-test — disposable seed -> capture -> capture -> compare round-trip.
# -----------------------------------------------------------------------------
cmd_self_test() {
  local base="${1:-}"
  if [[ -z "$base" ]]; then
    base="$(mktemp -d "${TMPDIR:-/tmp}/repo-roam-fp-selftest.XXXXXX")"
    local own=1
  else
    mkdir -p "$base"
    base="$(cd "$base" && pwd -P)"
    local own=0
  fi
  local rc=0
  (
    set -e
    local repo="$base/canary-repo"
    local fp_a="$base/fp-a"
    local fp_b="$base/fp-b"
    cmd_seed_canary "$repo"
    cmd_capture "$repo" "$fp_a"
    # Second capture of the SAME unchanged tree must be byte-identical
    # (proves the fingerprint is deterministic — the precondition for a
    # source-vs-rehydrated zero-diff comparison).
    cmd_capture "$repo" "$fp_b"
    REPO_ROAM_FP_COMPARE_DIFF="$base/compare.diff" cmd_compare "$fp_a" "$fp_b"

    # Negative control: mutate the tree, re-capture, compare MUST fail.
    printf 'drift\n' >>"$repo/NOTES.txt"
    local fp_c="$base/fp-c"
    cmd_capture "$repo" "$fp_c"
    if cmd_compare "$fp_a" "$fp_c" >/dev/null 2>&1; then
      printf '%s: self-test FAILED: drift not detected\n' "$PROG" >&2
      exit 1
    fi
    printf '%s: self-test PASSED (deterministic match + drift detected)\n' "$PROG"
  ) || rc=$?
  if [[ "$own" -eq 1 ]]; then
    rm -rf "$base"
  fi
  return "$rc"
}

main() {
  local mode="${1:-}"
  [[ -n "$mode" ]] || { usage >&2; exit 2; }
  shift || true
  case "$mode" in
    capture)     cmd_capture "${1:-}" "${2:-}" ;;
    compare)     cmd_compare "${1:-}" "${2:-}" ;;
    seed-canary) cmd_seed_canary "${1:-}" ;;
    self-test)   cmd_self_test "${1:-}" ;;
    -h|--help)   usage ;;
    *) printf '%s: unknown mode: %s\n' "$PROG" "$mode" >&2; usage >&2; exit 2 ;;
  esac
}

main "$@"
