#!/usr/bin/env bash
# Dhall spec check for the blob topology and TCFS layer interfaces
# (TIN-7498, BL2-Q9). Follows softconnect's and agent-switchboard's
# spec/check-dhall.sh. Needs the pinned dhall, dhall-json and jq on PATH;
# `just spec-dhall` provides them from the nixpkgs revision in the Justfile.
#
#   check-dhall.sh [--write] [REPO_ROOT]
#
# 0. Requires the pinned tool versions, and refuses any import from the
#    environment (R1) or the network (R2) before resolving a single import.
# 1. Requires every spec/dhall file to be in `dhall format`, then type-checks
#    every module. Layers, Interfaces and Model assert that Check.dhall finds
#    no broken rule, so a broken rule fails here.
# 2. Renders the generated outputs and requires them to equal the committed
#    goldens in spec/dhall/generated/ byte for byte. --write refreshes the
#    goldens instead. Each output is rendered twice, the second time with a
#    cold cache and another locale, time zone and HOME, and both renders
#    must be the same bytes (R3). A JSON golden is the `jq -S .` form.
# 3. Requires every spec/dhall/mutations/*.dhall to fail type-checking, with
#    the text on its "-- expect:" line in the error output.
# It reads files and writes only under a private temporary directory, or the
# goldens with --write.
set -euo pipefail

# Pinned with the nixpkgs revision in the Justfile. Change them together.
want_dhall="1.42.3"
want_dhall_json="1.7.12"
want_jq="jq-1.8.2"

write=0
if [ "${1:-}" = "--write" ]; then
  write=1
  shift
fi
root="$(cd "${1:-$(dirname "$0")/..}" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
export XDG_CACHE_HOME="$work/dhall-cache"
mkdir -p "$XDG_CACHE_HOME"
dhall_dir="$root/spec/dhall"
gen="$dhall_dir/generated"
fail=0

pinned() {
  if [ "$2" != "$3" ]; then
    echo "spec-dhall: $1 is $2, not the pinned $3 (run it through just spec-dhall)" >&2
    exit 1
  fi
}
pinned dhall "$(dhall --version)" "$want_dhall"
pinned dhall-to-json "$(dhall-to-json --version)" "$want_dhall_json"
pinned jq "$(jq --version)" "$want_jq"

# R1, R2: scan code, not comments, before anything resolves an import.
imports="$work/imports"
find "$dhall_dir" -name '*.dhall' -print0 | sort -z | while IFS= read -r -d '' f; do
  sed -E 's/--.*$//' "$f" | grep -nE '(^|[^[:alnum:]_])(env:|https?://)' | sed "s|^|$f:|" || true
done >"$imports"
if [ -s "$imports" ]; then
  while IFS= read -r hit; do
    case "$hit" in
      *env:*) echo "spec-dhall: R1 the spec imports nothing from the environment: ${hit#"$root"/}" >&2 ;;
      *) echo "spec-dhall: R2 the spec imports nothing from the network: ${hit#"$root"/}" >&2 ;;
    esac
  done <"$imports"
  exit 1
fi

for f in "$dhall_dir"/*.dhall "$dhall_dir"/render/*.dhall "$dhall_dir"/mutations/*.dhall; do
  if ! dhall format --check "$f" >/dev/null 2>&1; then
    echo "spec-dhall: ${f#"$root"/} is not in dhall format" >&2
    fail=1
  fi
done

for f in "$dhall_dir"/*.dhall "$dhall_dir"/render/*.dhall; do
  if ! dhall type --file "$f" >/dev/null; then
    echo "spec-dhall: type error in ${f#"$root"/}" >&2
    fail=1
  fi
done

# render NAME DIR: writes DIR/NAME from the Dhall sources.
render() {
  case "$1" in
    layers.json) dhall-to-json --file "$dhall_dir/Layers.dhall" ;;
    interfaces.json) dhall-to-json --file "$dhall_dir/Interfaces.dhall" ;;
    file-transitions.md) dhall text --file "$dhall_dir/render/file-transitions.dhall" ;;
  esac >"$2/$1"
  if [ "${1##*.}" = json ]; then
    jq -S . "$2/$1" >"$2/$1.canon"
    mv "$2/$1.canon" "$2/$1"
  fi
}

# R3: a second render with a cold cache and another environment.
mkdir -p "$work/again" "$work/again-cache" "$work/again-home"
render_again() {
  (
    export XDG_CACHE_HOME="$work/again-cache" HOME="$work/again-home"
    export TZ=Pacific/Chatham LC_ALL=C LANG=C
    render "$1" "$work/again"
  )
}

for out in layers.json interfaces.json file-transitions.md; do
  render "$out" "$work"
  render_again "$out"
  if ! cmp -s "$work/$out" "$work/again/$out"; then
    echo "spec-dhall: R3 rendering $out twice gave different bytes" >&2
    diff -u "$work/$out" "$work/again/$out" >&2 || true
    fail=1
    continue
  fi
  if [ "$write" -eq 1 ]; then
    mkdir -p "$gen"
    cp "$work/$out" "$gen/$out"
    echo "spec-dhall: wrote spec/dhall/generated/$out"
    continue
  fi
  if [ ! -f "$gen/$out" ]; then
    echo "spec-dhall: missing golden spec/dhall/generated/$out (run with --write)" >&2
    fail=1
  elif ! cmp -s "$work/$out" "$gen/$out"; then
    echo "spec-dhall: R3 spec/dhall/generated/$out is not the canonical render of its Dhall source" >&2
    diff -u "$gen/$out" "$work/$out" >&2 || true
    fail=1
  fi
done

mutants=0
for f in "$dhall_dir"/mutations/*.dhall; do
  mutants=$((mutants + 1))
  name="${f#"$root"/}"
  expect="$(sed -n 's/^-- expect: //p' "$f" | head -n 1)"
  if [ -z "$expect" ]; then
    echo "spec-dhall: $name has no '-- expect:' line" >&2
    fail=1
    continue
  fi
  if dhall type --file "$f" >/dev/null 2>"$work/mutant.err"; then
    echo "spec-dhall: mutant $name type-checks; the spec accepts it" >&2
    fail=1
  elif ! grep -qF -- "$expect" "$work/mutant.err"; then
    echo "spec-dhall: mutant $name failed for another reason (expected: $expect)" >&2
    sed -n '1,40p' "$work/mutant.err" >&2
    fail=1
  else
    echo "spec-dhall: mutant $name rejected: $expect"
  fi
done
if [ "$mutants" -eq 0 ]; then
  echo "spec-dhall: no mutants found" >&2
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "spec-dhall: ok"
