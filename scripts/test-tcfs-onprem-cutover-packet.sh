#!/usr/bin/env bash
#
# Regression tests for tcfs-onprem-cutover-packet.sh.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/tcfs-onprem-cutover-packet.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "${TMPDIR}"' EXIT

assert_contains() {
    local file="$1"
    local expected="$2"

    if ! grep -Fq "${expected}" "${file}"; then
        printf 'expected to find %s in %s\n' "${expected}" "${file}" >&2
        printf '%s\n' '--- output ---' >&2
        cat "${file}" >&2
        exit 1
    fi
}

if bash "${SCRIPT}" >"${TMPDIR}/missing.out" 2>"${TMPDIR}/missing.err"; then
    printf 'expected missing metadata to fail\n' >&2
    exit 1
fi
assert_contains "${TMPDIR}/missing.err" 'TCFS_DOWNTIME_WINDOW must be explicitly named'

if TCFS_DOWNTIME_WINDOW=tbd \
    TCFS_PREFLIGHT_OWNER='Jess Sullivan' \
    TCFS_ROLLBACK_OWNER='Jess Sullivan' \
    TCFS_POSTCUT_SMOKE_OWNER='Jess Sullivan' \
    bash "${SCRIPT}" >"${TMPDIR}/placeholder.out" 2>"${TMPDIR}/placeholder.err"; then
    printf 'expected placeholder downtime window to fail\n' >&2
    exit 1
fi
assert_contains "${TMPDIR}/placeholder.err" 'TCFS_DOWNTIME_WINDOW must be explicitly named'

if TCFS_DOWNTIME_WINDOW=' TBD, not approved ' \
    TCFS_PREFLIGHT_OWNER='Jess Sullivan' \
    TCFS_ROLLBACK_OWNER='Jess Sullivan' \
    TCFS_POSTCUT_SMOKE_OWNER='Jess Sullivan' \
    bash "${SCRIPT}" >"${TMPDIR}/placeholder-expanded.out" 2>"${TMPDIR}/placeholder-expanded.err"; then
    printf 'expected expanded placeholder downtime window to fail\n' >&2
    exit 1
fi
assert_contains "${TMPDIR}/placeholder-expanded.err" 'TCFS_DOWNTIME_WINDOW must be explicitly named'

OUT="${TMPDIR}/packet.out"
TCFS_CONTEXT='honey context' \
    TCFS_NAMESPACE='tcfs-dev' \
    TCFS_TRACKERS='GitHub #327 / Linear TIN-720 / maintenance note' \
    TCFS_DOWNTIME_WINDOW='2026-05-20 01:00-02:00 America/New_York' \
    TCFS_PREFLIGHT_OWNER='Jess Sullivan' \
    TCFS_ROLLBACK_OWNER='Ops Lead' \
    TCFS_POSTCUT_SMOKE_OWNER='Smoke Lead' \
    bash "${SCRIPT}" >"${OUT}"

assert_contains "${OUT}" '# TCFS on-prem cutover execution packet'
assert_contains "${OUT}" 'context: honey context'
assert_contains "${OUT}" 'namespace: tcfs-dev'
assert_contains "${OUT}" 'trackers: GitHub #327 / Linear TIN-720 / maintenance note'
assert_contains "${OUT}" 'downtime_window: 2026-05-20 01:00-02:00 America/New_York'
assert_contains "${OUT}" 'preflight_owner: Jess Sullivan'
assert_contains "${OUT}" 'rollback_owner: Ops Lead'
assert_contains "${OUT}" 'post_cut_smoke_owner: Smoke Lead'
assert_contains "${OUT}" 'Do not fix the Blahaj tailnet gate with a ProxyClass-only patch'
assert_contains "${OUT}" "TCFS_CONTEXT='honey context' TCFS_NAMESPACE='tcfs-dev' just onprem-preflight"
assert_contains "${OUT}" "TCFS_CONTEXT='honey context' TCFS_NAMESPACE='tcfs-dev' just onprem-migration-plan render-target-pvc-commands"
assert_contains "${OUT}" "TCFS_CONTEXT='honey context' TCFS_NAMESPACE='tcfs-dev' just onprem-migration-plan render-cutover-commands"
assert_contains "${OUT}" "TCFS_CONTEXT='honey context' TCFS_NAMESPACE='tcfs-dev' just onprem-migration-plan render-rollback-commands"

printf 'tcfs-onprem-cutover-packet tests passed\n'
