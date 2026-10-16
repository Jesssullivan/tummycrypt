#!/usr/bin/env bash
#
# tcfsd-health-check.sh — supervisor health check for tcfsd (Linux + macOS).
#
# Reference implementation for the lab Home Manager `tcfsd-health` timer /
# launchd agent. It replaces two failure modes seen in the 2026-10-15 estate
# check while the backend was paused:
#
#   * A backend outage made the mount probe fail, so the check restarted
#     tcfsd on every tick (~115 restarts a day). Restarting the daemon cannot
#     fix the backend. This script asks the daemon first (`tcfs health`) and,
#     when only the backend is down, reports DOWN and does not restart.
#   * On FileProvider-first Darwin hosts the check reported HEALTHY whenever
#     the daemon socket existed. A socket is not health; this script reports
#     what the daemon says about storage.
#
# Restarts that are warranted (daemon unreachable, or a stale mount while the
# backend is up) back off exponentially: base, 2x, 4x, ... up to a cap, and
# reset once the daemon answers again.
#
# The script restarts tcfsd only on an explicit `DAEMON_DOWN` verdict (or a
# stale mount while the daemon reports UP). Every other outcome (a CLI or
# config error, a CLI panic, a CLI too old to know `tcfs health`, a slow
# answer, or the probe itself timing out) is reported UNKNOWN or BUSY and
# restarts nothing: never restart on a verdict nobody gave.
#
# Probes run under a pure-bash deadline. Stock macOS has neither `timeout`
# nor `gtimeout`, and a probe stuck on a dead NFS mount can ignore signals,
# so the check abandons a probe at its deadline instead of waiting on it.
#
# Exit codes (one log line per run):
#   0  HEALTHY   daemon up, storage reachable, mount (if any) responsive
#   2  DOWN      daemon unreachable or mount stale; restarted or backoff-suppressed
#   3  DOWN      backend unreachable; daemon up; not restarted
#   4  UNKNOWN   no restartable verdict (daemon busy, CLI missing, too old,
#                failing or timed out); nothing restarted
#
# Environment (all optional):
#   TCFS_BIN                       tcfs CLI (default: tcfs on PATH)
#   TCFS_HEALTH_MOUNT_PATH         mount to probe with ls (default: none)
#   TCFS_HEALTH_TIMEOUT_SECS       probe deadline (default: 30; above the
#                                  CLI's own 10 s status RPC timeout, so the
#                                  CLI's verdict normally wins)
#   TCFS_HEALTH_STATE_DIR          backoff state dir (default: ~/.local/state/tcfs)
#   TCFS_HEALTH_BACKOFF_BASE_SECS  first restart backoff (default: 300)
#   TCFS_HEALTH_BACKOFF_CAP_SECS   longest restart backoff (default: 21600)
#                                  (positive whole seconds; anything else,
#                                  such as `5m`, uses the default)
#   TCFS_HEALTH_RESTART_CMD        restart command, run with sh -c (default:
#                                  launchctl kickstart -k on Darwin,
#                                  systemctl --user restart on Linux)
#   TCFS_HEALTH_LAUNCHD_LABEL      default: dev.tinyland.tcfsd
#   TCFS_HEALTH_SYSTEMD_UNIT       default: tcfsd.service
#   TCFS_HEALTH_NOW                epoch seconds override (tests)
#
set -uo pipefail

TCFS_BIN="${TCFS_BIN:-tcfs}"
MOUNT_PATH="${TCFS_HEALTH_MOUNT_PATH:-}"
TIMEOUT_SECS="${TCFS_HEALTH_TIMEOUT_SECS:-30}"
[[ "${TIMEOUT_SECS}" =~ ^[1-9][0-9]{0,5}$ ]] || TIMEOUT_SECS=30
STATE_DIR="${TCFS_HEALTH_STATE_DIR:-${HOME:-/tmp}/.local/state/tcfs}"
# Backoff settings must be positive whole seconds. Anything else (`5m`, 0,
# a negative number) falls back to the default: a malformed value must never
# break the arithmetic below and turn into a restart on every tick.
BASE_SECS="${TCFS_HEALTH_BACKOFF_BASE_SECS:-300}"
[[ "${BASE_SECS}" =~ ^[1-9][0-9]{0,8}$ ]] || BASE_SECS=300
CAP_SECS="${TCFS_HEALTH_BACKOFF_CAP_SECS:-21600}"
[[ "${CAP_SECS}" =~ ^[1-9][0-9]{0,8}$ ]] || CAP_SECS=21600
(( CAP_SECS >= BASE_SECS )) || CAP_SECS="${BASE_SECS}"
STATE_FILE="${STATE_DIR}/tcfsd-health.state"
NOW="${TCFS_HEALTH_NOW:-$(date +%s)}"

if [[ -z "${TCFS_HEALTH_RESTART_CMD:-}" ]]; then
    if [[ "$(uname -s)" == "Darwin" ]]; then
        TCFS_HEALTH_RESTART_CMD="launchctl kickstart -k gui/$(id -u)/${TCFS_HEALTH_LAUNCHD_LABEL:-dev.tinyland.tcfsd}"
    else
        TCFS_HEALTH_RESTART_CMD="systemctl --user restart ${TCFS_HEALTH_SYSTEMD_UNIT:-tcfsd.service}"
    fi
fi

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# Run "$@" for at most TIMEOUT_SECS and print its combined output. Returns
# its exit status, or 124 when it is still running at the deadline. Pure
# bash (no `timeout`/`gtimeout`), and it never waits on a probe past the
# deadline: the probe's output goes to a file, not a pipe this shell reads,
# and a probe that ignores SIGKILL (stuck in the kernel on a dead mount) is
# simply abandoned.
with_timeout() {
    local out pid status ticks=0
    out="$(mktemp "${TMPDIR:-/tmp}/tcfsd-health.XXXXXX")" || return 125
    "$@" >"${out}" 2>&1 </dev/null &
    pid=$!
    while kill -0 "${pid}" 2>/dev/null; do
        if (( ticks >= TIMEOUT_SECS * 50 )); then
            ABANDONED_PID="${pid}"
            kill -KILL "${pid}" 2>/dev/null
            disown "${pid}" 2>/dev/null || true
            cat "${out}" 2>/dev/null
            rm -f "${out}"
            return 124
        fi
        sleep 0.02
        ticks=$((ticks + 1))
    done
    wait "${pid}"
    status=$?
    cat "${out}" 2>/dev/null
    rm -f "${out}"
    return "${status}"
}

failures=0
next_restart=0
# PID of the last mount probe abandoned at its deadline (0: none). Kept until
# a probe answers again.
mount_hung=0
load_state() {
    [[ -f "${STATE_FILE}" ]] || return 0
    local key value
    while IFS='=' read -r key value; do
        case "${key}" in
            failures) [[ "${value}" =~ ^[0-9]+$ ]] && failures="${value}" ;;
            next_restart) [[ "${value}" =~ ^[0-9]+$ ]] && next_restart="${value}" ;;
            mount_hung) [[ "${value}" =~ ^[0-9]+$ ]] && mount_hung="${value}" ;;
        esac
    done <"${STATE_FILE}"
}

save_state() {
    mkdir -p "${STATE_DIR}" 2>/dev/null || return 0
    printf 'failures=%s\nnext_restart=%s\nmount_hung=%s\n' \
        "${failures}" "${next_restart}" "${mount_hung}" \
        >"${STATE_FILE}.tmp" && mv -f "${STATE_FILE}.tmp" "${STATE_FILE}"
}

reset_state() {
    [[ -f "${STATE_FILE}" ]] && rm -f "${STATE_FILE}"
    return 0
}

# restart_with_backoff REASON [HUNG_PROBE_PID]
restart_with_backoff() {
    local reason="$1"
    load_state
    mount_hung="${2:-0}"
    if (( NOW < next_restart )); then
        save_state
        echo "$(ts) DOWN: ${reason}; restart suppressed by backoff for $((next_restart - NOW))s (failures=${failures})"
        return 0
    fi
    if ! sh -c "${TCFS_HEALTH_RESTART_CMD}" >/dev/null 2>&1; then
        echo "$(ts) WARN: restart command failed: ${TCFS_HEALTH_RESTART_CMD}" >&2
    fi
    failures=$((failures + 1))
    local delay="${BASE_SECS}" step=1
    while (( step < failures && delay < CAP_SECS )); do
        delay=$((delay * 2))
        step=$((step + 1))
    done
    (( delay > CAP_SECS )) && delay="${CAP_SECS}"
    next_restart=$((NOW + delay))
    save_state
    echo "$(ts) DOWN: ${reason}; restarted tcfsd (attempt ${failures}; next restart no sooner than ${delay}s)"
}

health_out="$(with_timeout "${TCFS_BIN}" health)"
health_status=$?
health_out="${health_out//$'\n'/ }"

unknown() {
    echo "$(ts) UNKNOWN: $*; not restarting" >&2
    exit 4
}

case "${health_status}" in
    0)
        [[ "${health_out}" == UP* ]] || unknown "tcfs health exited 0 without an UP verdict: ${health_out}"
        if [[ -n "${MOUNT_PATH}" ]]; then
            # A probe stuck in the kernel on a dead mount can outlive SIGKILL.
            # When the last probe hung and its restart's backoff is still
            # running, do not start another one: one leaked `ls` per tick
            # would pile up for as long as the mount stays dead. The mount is
            # probed again once the backoff expires.
            load_state
            if (( mount_hung > 0 && NOW < next_restart )); then
                echo "$(ts) DOWN: mount ${MOUNT_PATH} probe skipped: the last probe hung (pid ${mount_hung}) and restart backoff is active for $((next_restart - NOW))s (failures=${failures})"
                exit 2
            fi
            ABANDONED_PID=0
            if ! with_timeout ls "${MOUNT_PATH}" >/dev/null 2>&1; then
                # The daemon and backend are fine, so a mount that does not
                # answer is the daemon's to fix. Restart, with backoff.
                restart_with_backoff "mount ${MOUNT_PATH} unresponsive while backend is up" "${ABANDONED_PID}"
                exit 2
            fi
        fi
        reset_state
        echo "$(ts) HEALTHY: ${health_out}"
        exit 0
        ;;
    2)
        # `tcfs health` says DAEMON_DOWN with exit 2. A CLI that predates
        # `tcfs health` also exits 2 (unknown subcommand); never restart on
        # a verdict nobody gave.
        [[ "${health_out}" == DAEMON_DOWN* ]] \
            || unknown "tcfs CLI gave no health verdict (too old?): ${health_out}"
        restart_with_backoff "tcfsd unreachable (${health_out})"
        exit 2
        ;;
    3)
        [[ "${health_out}" == BACKEND_DOWN* ]] \
            || unknown "tcfs health exited 3 without a BACKEND_DOWN verdict: ${health_out}"
        # The daemon answered: any restart streak is over. Restarting cannot
        # bring the backend back, so report and leave tcfsd alone.
        reset_state
        echo "$(ts) DOWN: backend unreachable; tcfsd is up and was not restarted (${health_out})"
        exit 3
        ;;
    4)
        # DAEMON_BUSY: tcfsd accepted the connection but answered slowly (a
        # long transfer, a stalled backend). Killing it would kill the work.
        [[ "${health_out}" == DAEMON_BUSY* ]] \
            || unknown "tcfs health exited 4 without a verdict: ${health_out}"
        echo "$(ts) BUSY: tcfsd is running but slow to answer; not restarting (${health_out})" >&2
        exit 4
        ;;
    124)
        unknown "tcfs health did not finish within ${TIMEOUT_SECS}s"
        ;;
    126 | 127)
        unknown "tcfs CLI unavailable (${TCFS_BIN})"
        ;;
    *)
        # 1 (config or load error), 101 (CLI panic), anything else: a client
        # problem a daemon restart cannot fix.
        unknown "tcfs health exited ${health_status} without a verdict: ${health_out:-no output}"
        ;;
esac
