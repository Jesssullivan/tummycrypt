#!/usr/bin/env bash
#
# Regression tests for lazy-hydration-mounted-smoke.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/lazy-hydration-mounted-smoke.sh"
TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-lazy-mounted-test.XXXXXX")"
trap 'rm -rf "${TMPDIR}"' EXIT

assert_contains() {
    local file="$1"
    local expected="$2"

    if ! grep -Fq -- "${expected}" "${file}"; then
        printf 'expected to find %s in %s\n' "${expected}" "${file}" >&2
        printf '%s\n' '--- output ---' >&2
        cat "${file}" >&2
        exit 1
    fi
}

assert_fails_contains() {
    local expected="$1"
    shift

    local out="${TMPDIR}/failure.out"
    local err="${TMPDIR}/failure.err"

    if "$@" >"${out}" 2>"${err}"; then
        printf 'expected command to fail: %s\n' "$*" >&2
        exit 1
    fi

    cat "${out}" "${err}" >"${TMPDIR}/failure.combined"
    assert_contains "${TMPDIR}/failure.combined" "${expected}"
}

MOUNT_ROOT="${TMPDIR}/mount"
EXPECTED_CONTENT="${TMPDIR}/expected.txt"
mkdir -p "${MOUNT_ROOT}/projects/alpha/notes" "${MOUNT_ROOT}/empty"

printf 'remote hydrated contents' >"${MOUNT_ROOT}/projects/alpha/notes/plan.txt"
printf 'legitimate source fixture\n' >"${MOUNT_ROOT}/projects/alpha/notes/parser.tc"
printf 'legitimate source fixture\n' >"${MOUNT_ROOT}/projects/alpha/notes/schema.tcf"
printf 'remote hydrated contents' >"${EXPECTED_CONTENT}"
ln -s notes/plan.txt "${MOUNT_ROOT}/projects/alpha/plan-link"
printf 'projects/alpha/plan-link\tnotes/plan.txt\n' >"${TMPDIR}/symlink-targets.tsv"

OUT="${TMPDIR}/positive.out"
bash "${SCRIPT}" \
    --mount-root "${MOUNT_ROOT}" \
    --expected-file "projects/alpha/notes/plan.txt" \
    --expect-entry "projects/alpha" \
    --expect-entry "empty" \
    --expected-content-file "${EXPECTED_CONTENT}" \
    --expected-symlink-targets-file "${TMPDIR}/symlink-targets.tsv" \
    --max-depth 5 \
    >"${OUT}"
assert_contains "${OUT}" "lazy hydration mounted smoke passed"
assert_contains "${OUT}" "cat byte count: 24"
assert_contains "${OUT}" "symlink target checks passed: 1"

CONTAINS_OUT="${TMPDIR}/contains.out"
bash "${SCRIPT}" \
    --mount-root "${MOUNT_ROOT}" \
    --expected-file "projects/alpha/notes/plan.txt" \
    --expected-contains "hydrated" \
    >"${CONTAINS_OUT}"
assert_contains "${CONTAINS_OUT}" "lazy hydration mounted smoke passed"

ROOT_SUFFIX_MOUNT="${TMPDIR}/mount-root.tc"
mkdir -p "${ROOT_SUFFIX_MOUNT}/docs"
printf 'clean content' >"${ROOT_SUFFIX_MOUNT}/docs/readme.txt"
ROOT_SUFFIX_OUT="${TMPDIR}/root-suffix.out"
bash "${SCRIPT}" \
    --mount-root "${ROOT_SUFFIX_MOUNT}" \
    --expected-file "docs/readme.txt" \
    --expected-content "clean content" \
    >"${ROOT_SUFFIX_OUT}"
assert_contains "${ROOT_SUFFIX_OUT}" "lazy hydration mounted smoke passed"

cat >"${MOUNT_ROOT}/projects/alpha/notes/leaked.tc" <<'EOF'
version https://tummycrypt.io/tcfs/v1
chunks 23
compressed 0
fetched 0
oid blake3:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e239
origin seaweedfs://filer.example.com/bucket/path/to/file
size 94371840
EOF
assert_fails_contains \
    "mounted view exposed physical TCFS stubs" \
    bash "${SCRIPT}" \
        --mount-root "${MOUNT_ROOT}" \
        --expected-file "projects/alpha/notes/plan.txt" \
        --max-depth 5
rm "${MOUNT_ROOT}/projects/alpha/notes/leaked.tc"

cat >"${MOUNT_ROOT}/projects/alpha/notes/leaked.tcf" <<'EOF'
version https://tummycrypt.io/tcfs/v1
chunks 0
compressed 0
fetched 0
oid blake3:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e239
origin seaweedfs://filer.example.com/bucket/path/to/dir
size 0
EOF
assert_fails_contains \
    "projects/alpha/notes/leaked.tcf" \
    bash "${SCRIPT}" \
        --mount-root "${MOUNT_ROOT}" \
        --expected-file "projects/alpha/notes/plan.txt" \
        --max-depth 5
rm "${MOUNT_ROOT}/projects/alpha/notes/leaked.tcf"

assert_fails_contains \
    "cat output did not match --expected-content" \
    bash "${SCRIPT}" \
        --mount-root "${MOUNT_ROOT}" \
        --expected-file "projects/alpha/notes/plan.txt" \
        --expected-content "wrong content"

assert_fails_contains \
    "expected entry missing from mounted view" \
    bash "${SCRIPT}" \
        --mount-root "${MOUNT_ROOT}" \
        --expected-file "projects/alpha/notes/plan.txt" \
        --expect-entry "missing/entry"

printf 'projects/alpha/plan-link\twrong-target\n' >"${TMPDIR}/bad-symlink-targets.tsv"
assert_fails_contains \
    "mounted symlink target mismatch" \
    bash "${SCRIPT}" \
        --mount-root "${MOUNT_ROOT}" \
        --expected-file "projects/alpha/notes/plan.txt" \
        --expected-symlink-targets-file "${TMPDIR}/bad-symlink-targets.tsv"

assert_fails_contains \
    "--expected-file must not contain .. path segments" \
    bash "${SCRIPT}" \
        --mount-root "${MOUNT_ROOT}" \
        --expected-file "../outside.txt"

assert_fails_contains \
    "--mount-root requires a value" \
    bash "${SCRIPT}" \
        --mount-root

printf 'lazy hydration mounted smoke tests passed\n'
