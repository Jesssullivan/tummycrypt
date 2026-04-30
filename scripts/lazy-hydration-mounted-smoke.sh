#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/lazy-hydration-mounted-smoke.sh [options]

Verify an already-mounted TCFS lazy hydration surface:
clean names are visible through ls/find, raw .tc/.tcf suffixes do not leak into
the mounted view, and cat of a remote-backed file hydrates and returns content.

This helper intentionally does not start tcfsd, seed storage, or mount a remote.
Use it after a real or disposable backend has been seeded and mounted.

Options:
  --mount-root <path>             Mounted TCFS root to inspect (required)
  --expected-file <relpath>       Remote-backed file to cat under mount root (required)
  --expect-entry <relpath>        Additional entry expected to exist; repeatable
  --expected-content <text>       Exact expected cat output
  --expected-content-file <path>  File containing exact expected cat output
  --expected-contains <text>      Substring expected in cat output
  --max-depth <n>                 find depth for clean-name scan (default: 4)
  -h, --help                      Show this help
EOF
}

fail() {
  echo "$*" >&2
  exit 1
}

validate_relpath() {
  local label="$1"
  local rel="$2"

  [[ -n "$rel" ]] || fail "$label must not be empty"
  [[ "$rel" != /* ]] || fail "$label must be relative: $rel"

  case "$rel" in
    ..|../*|*/..|*/../*)
      fail "$label must not contain .. path segments: $rel"
      ;;
  esac
}

MOUNT_ROOT=""
EXPECTED_FILE_REL=""
EXPECTED_CONTENT=""
EXPECTED_CONTENT_FILE=""
EXPECTED_CONTAINS=""
MAX_DEPTH=4
EXPECT_ENTRIES=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mount-root)
      [[ $# -ge 2 ]] || fail "--mount-root requires a value"
      MOUNT_ROOT="$2"
      shift 2
      ;;
    --expected-file)
      [[ $# -ge 2 ]] || fail "--expected-file requires a value"
      EXPECTED_FILE_REL="$2"
      shift 2
      ;;
    --expect-entry)
      [[ $# -ge 2 ]] || fail "--expect-entry requires a value"
      EXPECT_ENTRIES+=("$2")
      shift 2
      ;;
    --expected-content)
      [[ $# -ge 2 ]] || fail "--expected-content requires a value"
      EXPECTED_CONTENT="$2"
      shift 2
      ;;
    --expected-content-file)
      [[ $# -ge 2 ]] || fail "--expected-content-file requires a value"
      EXPECTED_CONTENT_FILE="$2"
      shift 2
      ;;
    --expected-contains)
      [[ $# -ge 2 ]] || fail "--expected-contains requires a value"
      EXPECTED_CONTAINS="$2"
      shift 2
      ;;
    --max-depth)
      [[ $# -ge 2 ]] || fail "--max-depth requires a value"
      MAX_DEPTH="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$MOUNT_ROOT" || -z "$EXPECTED_FILE_REL" ]]; then
  usage >&2
  exit 2
fi

if [[ "$MOUNT_ROOT" != "/" ]]; then
  MOUNT_ROOT="${MOUNT_ROOT%/}"
fi

validate_relpath "--expected-file" "$EXPECTED_FILE_REL"
for entry in "${EXPECT_ENTRIES[@]}"; do
  validate_relpath "--expect-entry" "$entry"
done

if [[ -n "$EXPECTED_CONTENT" && -n "$EXPECTED_CONTENT_FILE" ]]; then
  echo "--expected-content and --expected-content-file are mutually exclusive" >&2
  exit 2
fi

if ! [[ "$MAX_DEPTH" =~ ^[0-9]+$ ]] || [[ "$MAX_DEPTH" -lt 1 ]]; then
  echo "--max-depth must be a positive integer" >&2
  exit 2
fi

[[ -d "$MOUNT_ROOT" ]] || {
  echo "mount root is not a directory: $MOUNT_ROOT" >&2
  exit 1
}

if [[ -n "$EXPECTED_CONTENT_FILE" && ! -f "$EXPECTED_CONTENT_FILE" ]]; then
  echo "expected content file not found: $EXPECTED_CONTENT_FILE" >&2
  exit 1
fi

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-lazy-hydration-smoke.XXXXXX")"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

listing="$TMP_DIR/listing.txt"
stub_leaks="$TMP_DIR/stub-leaks.txt"
cat_output="$TMP_DIR/cat-output.bin"
expected_output="$TMP_DIR/expected-output.bin"

expected_path="$MOUNT_ROOT/$EXPECTED_FILE_REL"
parent_rel="$(dirname "$EXPECTED_FILE_REL")"
if [[ "$parent_rel" == "." ]]; then
  parent_path="$MOUNT_ROOT"
else
  parent_path="$MOUNT_ROOT/$parent_rel"
fi

[[ -d "$parent_path" ]] || {
  echo "expected file parent is not a directory: $parent_path" >&2
  exit 1
}

echo "mounted root: $MOUNT_ROOT"
echo "listing parent before cat: $parent_path"
ls -la "$parent_path"

find "$MOUNT_ROOT" -maxdepth "$MAX_DEPTH" -print | sort >"$listing"
echo "find evidence (max depth $MAX_DEPTH):"
cat "$listing"

while IFS= read -r path; do
  rel="${path#"$MOUNT_ROOT"/}"
  if [[ "$rel" == "$path" || "$rel" == "." || -z "$rel" ]]; then
    continue
  fi

  case "${rel##*/}" in
    *.tc|*.tcf)
      printf '%s\n' "$rel"
      ;;
  esac
done <"$listing" >"$stub_leaks"

if [[ -s "$stub_leaks" ]]; then
  echo "mounted view exposed physical stub suffixes:" >&2
  cat "$stub_leaks" >&2
  exit 1
fi

for entry in "${EXPECT_ENTRIES[@]}"; do
  if [[ ! -e "$MOUNT_ROOT/$entry" ]]; then
    echo "expected entry missing from mounted view: $entry" >&2
    exit 1
  fi
done

[[ -f "$expected_path" ]] || {
  echo "expected file is not visible before cat: $expected_path" >&2
  exit 1
}

echo "cat hydrate target: $EXPECTED_FILE_REL"
cat "$expected_path" >"$cat_output"
echo "cat byte count: $(wc -c <"$cat_output" | tr -d ' ')"

if [[ -n "$EXPECTED_CONTENT_FILE" ]]; then
  cmp -s "$EXPECTED_CONTENT_FILE" "$cat_output" || {
    echo "cat output did not match expected content file: $EXPECTED_CONTENT_FILE" >&2
    exit 1
  }
elif [[ -n "$EXPECTED_CONTENT" ]]; then
  printf '%s' "$EXPECTED_CONTENT" >"$expected_output"
  cmp -s "$expected_output" "$cat_output" || {
    echo "cat output did not match --expected-content" >&2
    exit 1
  }
elif [[ -n "$EXPECTED_CONTAINS" ]]; then
  grep -F -- "$EXPECTED_CONTAINS" "$cat_output" >/dev/null || {
    echo "cat output did not contain expected text" >&2
    exit 1
  }
fi

echo "lazy hydration mounted smoke passed"
