#!/usr/bin/env bash
#
# Regression tests for tcfsd-health-check.sh: a backend outage is reported
# DOWN without restarting the daemon, and daemon restarts back off
# exponentially instead of firing on every timer tick.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/tcfsd-health-check.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

# Stub `tcfs`: `tcfs health` exits with $STUB_HEALTH_EXIT and prints what the
# real CLI prints for that exit (or hangs when STUB_HEALTH_EXIT=hang).
write_stub() {
    cat >"${WORK}/tcfs" <<'STUB'
#!/usr/bin/env bash
[[ "${1:-}" == "health" ]] || exit 64
case "${STUB_HEALTH_EXIT:-0}" in
    0) echo "UP storage=ok" ;;
    1) echo "Error: loading config: invalid TOML" >&2 ;;
    2) echo "DAEMON_DOWN socket missing" ;;
    3) echo "BACKEND_DOWN storage=unreachable" ;;
    4) echo "DAEMON_BUSY connected, but the status RPC did not answer within 10s" ;;
    101) echo "thread 'main' panicked" >&2 ;;
    hang) exec sleep 60 ;;
esac
exit "${STUB_HEALTH_EXIT:-0}"
STUB
    chmod +x "${WORK}/tcfs"
}
write_stub

RESTART_LOG="${WORK}/restarts.log"
: >"${RESTART_LOG}"

run_check() {
    env \
        PATH="${CHECK_PATH:-${PATH}}" \
        TCFS_BIN="${WORK}/tcfs" \
        TCFS_HEALTH_TIMEOUT_SECS="${CHECK_TIMEOUT_SECS:-30}" \
        TCFS_HEALTH_MOUNT_PATH="${CHECK_MOUNT_PATH:-}" \
        TCFS_HEALTH_STATE_DIR="${WORK}/state" \
        TCFS_HEALTH_RESTART_CMD="echo restart >> '${RESTART_LOG}'" \
        TCFS_HEALTH_BACKOFF_BASE_SECS=60 \
        TCFS_HEALTH_BACKOFF_CAP_SECS=3600 \
        TCFS_HEALTH_NOW="${NOW}" \
        STUB_HEALTH_EXIT="${STUB_HEALTH_EXIT}" \
        "${SCRIPT}"
}

restarts() { wc -l <"${RESTART_LOG}" | tr -d ' '; }

fail() { echo "FAIL: $*" >&2; exit 1; }

expect_exit() {
    local want="$1"; shift
    local out status
    set +e
    out="$(run_check 2>&1)"
    status=$?
    set -e
    [[ "${status}" == "${want}" ]] || fail "expected exit ${want}, got ${status}: ${out}"
    LAST_OUT="${out}"
}

NOW=1000000

# 1. Backend down, daemon fine: report DOWN, never restart.
STUB_HEALTH_EXIT=3
expect_exit 3
[[ "${LAST_OUT}" == *"DOWN"* && "${LAST_OUT}" == *"backend"* ]] || fail "backend outage not reported DOWN: ${LAST_OUT}"
[[ "$(restarts)" == 0 ]] || fail "backend outage must not restart tcfsd"
for _ in 1 2 3 4 5; do NOW=$((NOW + 300)); expect_exit 3; done
[[ "$(restarts)" == 0 ]] || fail "repeated backend outage must not restart tcfsd"

# 2. Healthy: report HEALTHY.
STUB_HEALTH_EXIT=0
expect_exit 0
[[ "${LAST_OUT}" == *"HEALTHY"* ]] || fail "healthy daemon not reported HEALTHY: ${LAST_OUT}"

# 3. Daemon down: restart once, then back off exponentially.
STUB_HEALTH_EXIT=2
expect_exit 2
[[ "$(restarts)" == 1 ]] || fail "first daemon-down check must restart once"
NOW=$((NOW + 30)); expect_exit 2
[[ "$(restarts)" == 1 ]] || fail "restart within the 60s backoff must be suppressed"
[[ "${LAST_OUT}" == *"suppressed"* ]] || fail "suppressed restart not reported: ${LAST_OUT}"
NOW=$((NOW + 31)); expect_exit 2
[[ "$(restarts)" == 2 ]] || fail "restart after the 60s backoff must run"
NOW=$((NOW + 61)); expect_exit 2
[[ "$(restarts)" == 2 ]] || fail "second backoff must double to 120s"
NOW=$((NOW + 60)); expect_exit 2
[[ "$(restarts)" == 3 ]] || fail "restart after the doubled backoff must run"

# A day of 5-minute ticks with the daemon down stays far below one restart
# per tick (the cap is one hour, so at most ~24 + the ramp).
for _ in $(seq 1 288); do NOW=$((NOW + 300)); expect_exit 2; done
day="$(restarts)"
(( day <= 40 )) || fail "restart storm: ${day} restarts in a simulated day"

# 4. Recovery resets the backoff.
STUB_HEALTH_EXIT=0
expect_exit 0
STUB_HEALTH_EXIT=2
before="$(restarts)"
NOW=$((NOW + 1)); expect_exit 2
[[ "$(restarts)" == $((before + 1)) ]] || fail "a recovered daemon must get an immediate restart on the next outage"

# 5. A CLI without `tcfs health` (exit 2, clap usage error) is UNKNOWN, not
#    DAEMON_DOWN: it must never trigger a restart.
cat >"${WORK}/tcfs" <<'STUB'
#!/usr/bin/env bash
echo "error: unrecognized subcommand 'health'" >&2
exit 2
STUB
before="$(restarts)"
NOW=$((NOW + 100000)); expect_exit 4
[[ "$(restarts)" == "${before}" ]] || fail "an old CLI must not trigger a restart"

# 6. No outcome except an explicit DAEMON_DOWN restarts tcfsd: a config or
#    load error (1), a CLI panic (101) and a busy daemon (4) are all UNKNOWN.
write_stub
for code in 1 101 4; do
    STUB_HEALTH_EXIT="${code}"
    before="$(restarts)"
    NOW=$((NOW + 100000)); expect_exit 4
    [[ "$(restarts)" == "${before}" ]] || fail "exit ${code} must not restart tcfsd: ${LAST_OUT}"
done
STUB_HEALTH_EXIT=4
NOW=$((NOW + 100000)); expect_exit 4
[[ "${LAST_OUT}" == *"BUSY"* ]] || fail "a busy daemon must be reported BUSY: ${LAST_OUT}"

# 7. Probes are bounded without coreutils `timeout`/`gtimeout` (stock macOS):
#    PATH holds only what the script needs, minus any timeout binary.
BIN="${WORK}/bin"
mkdir -p "${BIN}"
for tool in bash sh env mktemp cat rm sleep date uname id mkdir mv; do
    ln -sf "$(command -v "${tool}")" "${BIN}/${tool}"
done
[[ ! -e "${BIN}/timeout" && ! -e "${BIN}/gtimeout" ]] || fail "fixture PATH must lack timeout"
# A stale mount: `ls` never returns.
printf '#!%s\nexec sleep 60\n' "$(command -v bash)" >"${BIN}/ls"
chmod +x "${BIN}/ls"
CHECK_PATH="${BIN}"
CHECK_TIMEOUT_SECS=1

#    a. A hung `tcfs health` is UNKNOWN within the deadline, never a restart.
STUB_HEALTH_EXIT=hang
before="$(restarts)"
started="$(date +%s)"
NOW=$((NOW + 100000)); expect_exit 4
(( $(date +%s) - started < 10 )) || fail "a hung tcfs health must be abandoned at the deadline"
[[ "$(restarts)" == "${before}" ]] || fail "a hung CLI must not restart tcfsd"

#    b. A mount whose `ls` hangs is detected within the deadline and, with the
#       daemon UP, restarted (with backoff).
STUB_HEALTH_EXIT=0
CHECK_MOUNT_PATH="${WORK}/mnt"
before="$(restarts)"
started="$(date +%s)"
NOW=$((NOW + 100000)); expect_exit 2
(( $(date +%s) - started < 10 )) || fail "a hung mount probe must be abandoned at the deadline"
[[ "$(restarts)" == $((before + 1)) ]] || fail "a stale mount with the daemon up must restart once"
unset CHECK_PATH CHECK_TIMEOUT_SECS CHECK_MOUNT_PATH

echo "tcfsd-health-check: all checks passed"
