#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/tin3277-ciphertext-parity-gate.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tin3277-parity-gate-test.XXXXXX")"
trap 'rm -rf "$TMPDIR"' EXIT

assert_contains() {
  local file="$1" expected="$2"
  if ! grep -Fq -- "$expected" "$file"; then
    printf 'expected to find %s in %s\n' "$expected" "$file" >&2
    printf '%s\n' '--- output ---' >&2
    cat "$file" >&2
    exit 1
  fi
}

assert_not_contains() {
  local file="$1" unexpected="$2"
  if grep -Fq -- "$unexpected" "$file"; then
    printf 'did not expect to find %s in %s\n' "$unexpected" "$file" >&2
    cat "$file" >&2
    exit 1
  fi
}

# Fake ssh: given `ssh HOST 'f=...; ...'`, resolve a canned per-host,
# per-path digest from $TMPDIR/fixture.tsv (host<TAB>relpath<TAB>digest),
# where digest may be a real 64-hex sha256, "MISSING", or "DECRYPT_FAIL".
# This never touches age or any real key material - it is pure fixture data.
cat >"$TMPDIR/ssh" <<'FAKESSH'
#!/usr/bin/env bash
set -euo pipefail
host="$1"
cmd="$2"
# Pull the relative path back out of the remote command's $HOME/tcfs/<path>
relpath="$(printf '%s' "$cmd" | sed -n 's#.*\$HOME/tcfs/\([^ ;"]*\).*#\1#p' | head -1)"
digest="$(awk -F'\t' -v h="$host" -v p="$relpath" '$1==h && $2==p {print $3; found=1} END{if(!found) exit 1}' "$FIXTURE")" || {
  echo "FIXTURE_MISS:${host}:${relpath}" >&2
  exit 1
}
case "$digest" in
  MISSING) echo MISSING ;;
  DECRYPT_FAIL) exit 1 ;;
  *) echo "$digest" ;;
esac
FAKESSH
chmod +x "$TMPDIR/ssh"

HASH_1="$(printf 'x' | shasum -a 256 2>/dev/null | cut -d' ' -f1 || printf '%064d' 1)"
HASH_2="$(printf 'y' | shasum -a 256 2>/dev/null | cut -d' ' -f1 || printf '%064d' 2)"

# --- Case 1: all paths match -> PARITY, exit 0 ---
cat >"$TMPDIR/fixture-parity.tsv" <<EOF
neo	secrets/.manifest.toml	${HASH_1}
honey	secrets/.manifest.toml	${HASH_1}
EOF
out="$TMPDIR/out-parity.txt"
if ! FIXTURE="$TMPDIR/fixture-parity.tsv" "$SCRIPT" --ssh "$TMPDIR/ssh" \
    --identity-a /id/a.txt --identity-b /id/b.txt \
    --host-a neo --host-b honey secrets/.manifest.toml >"$out" 2>&1; then
  echo "expected PARITY case to exit 0" >&2
  cat "$out" >&2
  exit 1
fi
assert_contains "$out" "PARITY"

# --- Case 2: mismatched digests -> MISMATCH, non-zero exit ---
cat >"$TMPDIR/fixture-mismatch.tsv" <<EOF
neo	secrets/.manifest.toml	${HASH_1}
honey	secrets/.manifest.toml	${HASH_2}
EOF
out="$TMPDIR/out-mismatch.txt"
if FIXTURE="$TMPDIR/fixture-mismatch.tsv" "$SCRIPT" --ssh "$TMPDIR/ssh" \
    --identity-a /id/a.txt --identity-b /id/b.txt \
    --host-a neo --host-b honey secrets/.manifest.toml >"$out" 2>&1; then
  echo "expected MISMATCH case to exit non-zero" >&2
  cat "$out" >&2
  exit 1
fi
assert_contains "$out" "MISMATCH"

# --- Case 3: missing on one host -> MISSING-ONE, non-zero exit ---
cat >"$TMPDIR/fixture-missing.tsv" <<EOF
neo	secrets/.manifest.toml	${HASH_1}
honey	secrets/.manifest.toml	MISSING
EOF
out="$TMPDIR/out-missing.txt"
if FIXTURE="$TMPDIR/fixture-missing.tsv" "$SCRIPT" --ssh "$TMPDIR/ssh" \
    --identity-a /id/a.txt --identity-b /id/b.txt \
    --host-a neo --host-b honey secrets/.manifest.toml >"$out" 2>&1; then
  echo "expected MISSING-ONE case to exit non-zero" >&2
  cat "$out" >&2
  exit 1
fi
assert_contains "$out" "MISSING-ONE"

# --- Case 4: decrypt failure on one host -> ERROR, non-zero exit ---
cat >"$TMPDIR/fixture-decryptfail.tsv" <<EOF
neo	secrets/api/github_token.age	${HASH_1}
honey	secrets/api/github_token.age	DECRYPT_FAIL
EOF
out="$TMPDIR/out-decryptfail.txt"
if FIXTURE="$TMPDIR/fixture-decryptfail.tsv" "$SCRIPT" --ssh "$TMPDIR/ssh" \
    --identity-a /id/a.txt --identity-b /id/b.txt \
    --host-a neo --host-b honey secrets/api/github_token.age >"$out" 2>&1; then
  echo "expected ERROR case to exit non-zero" >&2
  cat "$out" >&2
  exit 1
fi
assert_contains "$out" "ERROR"

# --- Case 5: missing --identity-a is rejected before any ssh call ---
out="$TMPDIR/out-noident.txt"
if FIXTURE="$TMPDIR/fixture-parity.tsv" "$SCRIPT" --ssh "$TMPDIR/ssh" \
    --identity-b /id/b.txt --host-a neo --host-b honey secrets/.manifest.toml \
    >"$out" 2>&1; then
  echo "expected missing --identity-a to fail" >&2
  cat "$out" >&2
  exit 1
fi
assert_contains "$out" "identity-a"

# --- Case 6: the gate never prints plaintext bytes, only digests/status ---
out="$TMPDIR/out-noleak.txt"
FIXTURE="$TMPDIR/fixture-parity.tsv" "$SCRIPT" --ssh "$TMPDIR/ssh" \
    --identity-a /id/a.txt --identity-b /id/b.txt \
    --host-a neo --host-b honey secrets/.manifest.toml >"$out" 2>&1 || true
assert_not_contains "$out" "-----BEGIN"
assert_not_contains "$out" "age-encryption.org"

echo "ok"
