#!/usr/bin/env bash
# Linux post-install smoke harness (TIN-1422 scaffold).
#
# Verify the installed Linux package path after .deb/.rpm install:
# package install, tcfsd reachable (systemd unit or fallback foreground),
# FUSE mount, remote index gate via `tcfs index inspect`, exact-content
# hydration through the mount, and optional evict/rehydrate and mutation
# exercises.
#
# This is the structural analog of scripts/macos-postinstall-smoke.sh.
# Some sections are explicit TODO stubs — see the comments and the PR body
# for TIN-1422 follow-up. The goal of this commit is to lock in the shape,
# argument surface, log-dir layout, and the require_expected_remote_index
# gate so future commits can fill out coverage without renegotiating the
# harness contract.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

usage() {
  cat <<'EOF'
Usage: scripts/linux-postinstall-smoke.sh [options]

Verify the installed Linux package path after .deb/.rpm install:
package layout, tcfsd reachable, FUSE mount, exact-content hydration,
optional evict/rehydrate and mutation exercises.

This harness assumes a real operator config and a routable backend.
It does not fabricate storage credentials.

Options:
  --package-path <path>       .deb or .rpm artifact to install. If omitted
                              the harness assumes tcfsd/tcfs are already on
                              PATH (useful when packages were installed by
                              the workflow before this script runs).
  --config <path>             tcfs config (default: ~/.config/tcfs/config.toml)
  --expected-version <ver>    Require tcfs/tcfsd --version output to include this
  --expected-file <relpath>   Relative path under the mount to enumerate + hydrate
  --expected-content <text>   Exact content expected from --expected-file
  --expected-content-file <path>
                              File containing exact expected content
  --seed-expected-file        Create a timestamped fixture, push to remote, and
                              use it as the expected hydration target
  --exercise-evict-rehydrate  After hydration, run `tcfs unsync` and re-read to
                              verify rehydrate
  --exercise-mutation         Write a new file through the mount and verify via
                              remote pull
  --mutation-file <relpath>   Relative mutation file path under the mount
  --mutation-content <text>   Exact content for the mutation file
  --mutation-content-file <path>
                              File containing exact mutation content
  --remote-prefix <prefix>    Remote prefix used for index inspect + push/pull
  --mount-point <path>        FUSE mount point (default: temp dir under LOG_DIR)
  --remote-spec <spec>        Remote spec for `tcfs mount` (default:
                              s3://<bucket>/<prefix>; required for direct mount)
  --tcfs <path-or-name>       CLI binary (default: tcfs)
  --tcfsd <path-or-name>      Daemon binary (default: tcfsd)
  --timeout <seconds>         Wait timeout for async steps (default: 45)
  --log-dir <path>            Persist logs instead of using a temp dir
  --skip-status               Skip `tcfs status` checks
  --skip-package-install      Skip dpkg/rpm install (binaries already present)
  --systemd-unit <name>       systemd unit to start (default: tcfsd.service)
  --no-systemd                Always run tcfsd in foreground, don't try systemd
  -h, --help                  Show this help
EOF
}

EXPECTED_VERSION=""
CONFIG_PATH="${TCFS_CONFIG:-$HOME/.config/tcfs/config.toml}"
EXPECTED_FILE_REL=""
EXPECTED_CONTENT=""
EXPECTED_CONTENT_FILE=""
SEED_EXPECTED_FILE="${TCFS_SEED_EXPECTED_FILE:-0}"
EXERCISE_EVICT_REHYDRATE="${TCFS_EXERCISE_EVICT_REHYDRATE:-0}"
EXERCISE_MUTATION="${TCFS_EXERCISE_MUTATION:-0}"
MUTATION_FILE_REL="${TCFS_MUTATION_FILE_REL:-}"
MUTATION_CONTENT="${TCFS_MUTATION_CONTENT:-}"
MUTATION_CONTENT_FILE="${TCFS_MUTATION_CONTENT_FILE:-}"
REMOTE_PREFIX="${TCFS_REMOTE_PREFIX:-}"
REMOTE_SPEC="${TCFS_REMOTE_SPEC:-}"
MOUNT_POINT="${TCFS_MOUNT_POINT:-}"
PACKAGE_PATH=""
SKIP_PACKAGE_INSTALL="${TCFS_SKIP_PACKAGE_INSTALL:-0}"
SYSTEMD_UNIT="${TCFS_SYSTEMD_UNIT:-tcfsd.service}"
USE_SYSTEMD="${TCFS_USE_SYSTEMD:-1}"
TCFS_BIN="${TCFS_BIN:-tcfs}"
TCFSD_BIN="${TCFSD_BIN:-tcfsd}"
TIMEOUT_SECS="${TIMEOUT_SECS:-45}"
LOG_DIR_OVERRIDE=""
SKIP_STATUS=0
LOG_DIR=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --package-path)
      PACKAGE_PATH="$2"
      shift 2
      ;;
    --config)
      CONFIG_PATH="$2"
      shift 2
      ;;
    --expected-version)
      EXPECTED_VERSION="$2"
      shift 2
      ;;
    --expected-file)
      EXPECTED_FILE_REL="$2"
      shift 2
      ;;
    --expected-content)
      EXPECTED_CONTENT="$2"
      shift 2
      ;;
    --expected-content-file)
      EXPECTED_CONTENT_FILE="$2"
      shift 2
      ;;
    --seed-expected-file)
      SEED_EXPECTED_FILE=1
      shift
      ;;
    --exercise-evict-rehydrate)
      EXERCISE_EVICT_REHYDRATE=1
      shift
      ;;
    --exercise-mutation)
      EXERCISE_MUTATION=1
      shift
      ;;
    --mutation-file)
      MUTATION_FILE_REL="$2"
      shift 2
      ;;
    --mutation-content)
      MUTATION_CONTENT="$2"
      shift 2
      ;;
    --mutation-content-file)
      MUTATION_CONTENT_FILE="$2"
      shift 2
      ;;
    --remote-prefix)
      REMOTE_PREFIX="$2"
      shift 2
      ;;
    --remote-spec)
      REMOTE_SPEC="$2"
      shift 2
      ;;
    --mount-point)
      MOUNT_POINT="$2"
      shift 2
      ;;
    --tcfs)
      TCFS_BIN="$2"
      shift 2
      ;;
    --tcfsd)
      TCFSD_BIN="$2"
      shift 2
      ;;
    --timeout)
      TIMEOUT_SECS="$2"
      shift 2
      ;;
    --log-dir)
      LOG_DIR_OVERRIDE="$2"
      shift 2
      ;;
    --skip-status)
      SKIP_STATUS=1
      shift
      ;;
    --skip-package-install)
      SKIP_PACKAGE_INSTALL=1
      shift
      ;;
    --systemd-unit)
      SYSTEMD_UNIT="$2"
      shift 2
      ;;
    --no-systemd)
      USE_SYSTEMD=0
      shift
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

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "scripts/linux-postinstall-smoke.sh only runs on Linux" >&2
  exit 1
fi

if [[ -n "$EXPECTED_CONTENT" && -n "$EXPECTED_CONTENT_FILE" ]]; then
  echo "--expected-content and --expected-content-file are mutually exclusive" >&2
  exit 2
fi

if [[ -n "$MUTATION_CONTENT" && -n "$MUTATION_CONTENT_FILE" ]]; then
  echo "--mutation-content and --mutation-content-file are mutually exclusive" >&2
  exit 2
fi

if [[ "$EXERCISE_EVICT_REHYDRATE" == "1" && -z "$EXPECTED_FILE_REL" && "$SEED_EXPECTED_FILE" != "1" ]]; then
  echo "--exercise-evict-rehydrate requires --expected-file or --seed-expected-file" >&2
  exit 2
fi

if [[ "$EXERCISE_EVICT_REHYDRATE" == "1" && "$SEED_EXPECTED_FILE" != "1" && -z "$EXPECTED_CONTENT" && -z "$EXPECTED_CONTENT_FILE" ]]; then
  echo "--exercise-evict-rehydrate requires --expected-content, --expected-content-file, or --seed-expected-file" >&2
  exit 2
fi

if [[ "$EXERCISE_MUTATION" == "1" && -z "$REMOTE_PREFIX" ]]; then
  echo "--exercise-mutation requires --remote-prefix" >&2
  exit 2
fi

short_pause() {
  if command -v perl >/dev/null 2>&1; then
    perl -e 'select undef, undef, undef, 1.0'
  else
    python3 -c 'import select; select.select([], [], [], 1.0)'
  fi
}

require_file() {
  local path="$1"
  [[ -f "$path" ]] || {
    echo "required file not found: $path" >&2
    exit 1
  }
}

require_dir() {
  local path="$1"
  [[ -d "$path" ]] || {
    echo "required directory not found: $path" >&2
    exit 1
  }
}

if [[ -n "$EXPECTED_CONTENT_FILE" ]]; then
  require_file "$EXPECTED_CONTENT_FILE"
fi

if [[ -n "$MUTATION_CONTENT_FILE" ]]; then
  require_file "$MUTATION_CONTENT_FILE"
fi

resolve_bin() {
  local candidate="$1"
  if [[ "$candidate" == */* ]]; then
    [[ -x "$candidate" ]] || {
      echo "binary is not executable: $candidate" >&2
      exit 1
    }
    printf '%s\n' "$candidate"
    return
  fi

  command -v "$candidate" >/dev/null 2>&1 || {
    echo "command not found: $candidate" >&2
    exit 1
  }
  command -v "$candidate"
}

assert_version() {
  local label="$1"
  local bin="$2"
  local output

  output="$("$bin" --version)"
  echo "$label binary: $bin"
  echo "$label version: $output"

  if [[ -n "$EXPECTED_VERSION" && "$output" != *"$EXPECTED_VERSION"* ]]; then
    echo "$label version mismatch: expected output containing '$EXPECTED_VERSION'" >&2
    exit 1
  fi
}

validate_relative_file_path() {
  local path="$1"

  if [[ -z "$path" || "$path" == /* || "$path" == */ ]]; then
    echo "expected relative file path, got: $path" >&2
    exit 2
  fi

  local component
  local -a components=()
  IFS='/' read -r -a components <<<"$path"
  for component in "${components[@]}"; do
    if [[ -z "$component" || "$component" == "." || "$component" == ".." ]]; then
      echo "expected normalized relative file path, got: $path" >&2
      exit 2
    fi
  done
}

# ── Package install ──────────────────────────────────────────────────────────
install_package() {
  [[ "$SKIP_PACKAGE_INSTALL" == "1" ]] && {
    echo "package install skipped (--skip-package-install)"
    return 0
  }
  [[ -n "$PACKAGE_PATH" ]] || {
    echo "no --package-path provided and --skip-package-install not set" >&2
    exit 2
  }
  require_file "$PACKAGE_PATH"

  local install_log="$LOG_DIR/package-install.log"
  case "$PACKAGE_PATH" in
    *.deb)
      echo "installing .deb: $PACKAGE_PATH"
      if command -v sudo >/dev/null 2>&1; then
        sudo -n apt-get install -y --no-install-recommends "$PACKAGE_PATH" \
          >"$install_log" 2>&1 || {
            # apt-get install on a path needs ./prefix or absolute; fall back to dpkg
            sudo -n dpkg -i "$PACKAGE_PATH" >>"$install_log" 2>&1 || {
              echo "dpkg install failed; see $install_log" >&2
              cat "$install_log" >&2 || true
              exit 1
            }
            sudo -n apt-get install -y -f >>"$install_log" 2>&1 || {
              echo "apt dependency repair failed after dpkg install; see $install_log" >&2
              cat "$install_log" >&2 || true
              exit 1
            }
          }
      else
        dpkg -i "$PACKAGE_PATH" >"$install_log" 2>&1 || {
          echo "dpkg install failed (no sudo); see $install_log" >&2
          cat "$install_log" >&2 || true
          exit 1
        }
      fi
      ;;
    *.rpm)
      echo "installing .rpm: $PACKAGE_PATH"
      if command -v sudo >/dev/null 2>&1; then
        sudo -n rpm -i --replacepkgs "$PACKAGE_PATH" >"$install_log" 2>&1 || {
          echo "rpm install failed; see $install_log" >&2
          cat "$install_log" >&2 || true
          exit 1
        }
      else
        rpm -i --replacepkgs "$PACKAGE_PATH" >"$install_log" 2>&1 || {
          echo "rpm install failed (no sudo); see $install_log" >&2
          cat "$install_log" >&2 || true
          exit 1
        }
      fi
      ;;
    *)
      echo "unsupported package extension (expected .deb or .rpm): $PACKAGE_PATH" >&2
      exit 2
      ;;
  esac
  echo "package install log: $install_log"
}

# ── Daemon control ───────────────────────────────────────────────────────────
DAEMON_PID=""

start_daemon_systemd() {
  command -v systemctl >/dev/null 2>&1 || return 2

  echo "starting tcfsd via systemd unit: $SYSTEMD_UNIT"
  if command -v sudo >/dev/null 2>&1; then
    sudo -n systemctl start "$SYSTEMD_UNIT" >"$LOG_DIR/systemctl-start.log" 2>&1 || return 1
  else
    systemctl start "$SYSTEMD_UNIT" >"$LOG_DIR/systemctl-start.log" 2>&1 || return 1
  fi

  # Wait until unit reports active. systemctl is-active prints "active" on
  # success and non-zero exit on failure, so quote defensively.
  local deadline=$((SECONDS + TIMEOUT_SECS))
  local state="unknown"
  while (( SECONDS < deadline )); do
    state="$(systemctl is-active "$SYSTEMD_UNIT" 2>/dev/null || true)"
    if [[ "$state" == "active" ]]; then
      echo "systemd unit active: $SYSTEMD_UNIT"
      return 0
    fi
    short_pause
  done

  echo "systemd unit did not become active in time: $SYSTEMD_UNIT (last state: $state)" >&2
  return 1
}

start_daemon_foreground() {
  local log="$LOG_DIR/tcfsd.log"
  echo "starting tcfsd in foreground: $TCFSD_PATH --config $CONFIG_PATH"
  "$TCFSD_PATH" --config "$CONFIG_PATH" --log-format text >"$log" 2>&1 &
  DAEMON_PID="$!"
  echo "tcfsd pid: $DAEMON_PID (log: $log)"

  wait_for_foreground_daemon_socket "$log"
}

config_daemon_socket() {
  python3 - "$CONFIG_PATH" <<'PY' 2>/dev/null || true
import sys
try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib

with open(sys.argv[1], "rb") as fh:
    cfg = tomllib.load(fh)
print(cfg.get("daemon", {}).get("socket", ""))
PY
}

wait_for_foreground_daemon_socket() {
  local log="$1"
  local socket_path
  local deadline

  socket_path="$(config_daemon_socket)"
  if [[ -z "$socket_path" ]]; then
    echo "daemon socket not configured; falling back to process liveness check"
    short_pause
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
      echo "tcfsd exited immediately; see $log" >&2
      cat "$log" >&2 || true
      exit 1
    fi
    return
  fi

  deadline=$((SECONDS + TIMEOUT_SECS))
  while (( SECONDS < deadline )); do
    if [[ -S "$socket_path" ]]; then
      echo "daemon socket ready: $socket_path"
      return
    fi
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
      echo "tcfsd exited before socket appeared: $socket_path; see $log" >&2
      cat "$log" >&2 || true
      exit 1
    fi
    short_pause
  done

  echo "timed out waiting for daemon socket: $socket_path" >&2
  cat "$log" >&2 || true
  exit 1
}

start_daemon() {
  if [[ "$USE_SYSTEMD" == "1" ]]; then
    if start_daemon_systemd; then
      return 0
    fi
    echo "systemd start failed or unavailable; falling back to foreground" >&2
  fi
  start_daemon_foreground
}

stop_daemon() {
  if [[ -n "$DAEMON_PID" ]] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
  if [[ "$USE_SYSTEMD" == "1" ]] && command -v systemctl >/dev/null 2>&1; then
    if systemctl is-active --quiet "$SYSTEMD_UNIT" 2>/dev/null; then
      if command -v sudo >/dev/null 2>&1; then
        sudo -n systemctl stop "$SYSTEMD_UNIT" >/dev/null 2>&1 || true
      else
        systemctl stop "$SYSTEMD_UNIT" >/dev/null 2>&1 || true
      fi
    fi
  fi
}

# ── Status + index gate (same shape as macOS harness) ────────────────────────
status_log() {
  local label="$1"
  printf '%s/%s-status.log\n' "$LOG_DIR" "$label"
}

run_status() {
  local label="$1"
  local log_path

  if [[ "$SKIP_STATUS" -eq 1 ]]; then
    echo "status check skipped ($label)"
    return
  fi

  require_file "$CONFIG_PATH"

  log_path="$(status_log "$label")"
  "$TCFS_PATH" --config "$CONFIG_PATH" status >"$log_path" 2>&1 || {
    echo "tcfs status failed during $label" >&2
    cat "$log_path" >&2 || true
    exit 1
  }

  grep -q "tcfsd v" "$log_path" || {
    echo "tcfs status output did not include daemon version during $label" >&2
    cat "$log_path" >&2 || true
    exit 1
  }

  grep -Eq 'storage:.*\[ok\]' "$log_path" || {
    echo "tcfs status did not report storage [ok] during $label" >&2
    cat "$log_path" >&2 || true
    exit 1
  }

  echo "tcfs status ($label):"
  cat "$log_path"
}

run_tcfs_index_inspect() {
  local rel="$1"
  local out="$2"
  local err="${out}.err"
  local -a args

  [[ -n "$TCFS_PATH" ]] || return 2

  args=(--config "$CONFIG_PATH" index inspect "$rel" --json)
  if [[ -n "$REMOTE_PREFIX" ]]; then
    args+=(--prefix "$REMOTE_PREFIX")
  fi

  "$TCFS_PATH" "${args[@]}" >"$out" 2>"$err"
}

extract_index_status() {
  local report="$1"

  python3 - "$report" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as fh:
    report = json.load(fh)
print(report.get("status", "unknown"))
PY
}

require_expected_remote_index() {
  local rel="$1"
  local report="$LOG_DIR/expected-file-index.json"
  local status

  run_tcfs_index_inspect "$rel" "$report" || {
    echo "tcfs index inspect failed for expected file: $rel" >&2
    cat "${report}.err" >&2 2>/dev/null || true
    exit 1
  }

  status="$(extract_index_status "$report")"
  echo "remote index status for expected file: $status"
  if [[ "$status" != "visible" ]]; then
    echo "expected file is not backed by a visible remote index entry: $rel" >&2
    cat "$report" >&2 || true
    exit 1
  fi
}

# ── Seed (push timestamped fixture, then use it as the expected file) ────────
prepare_seed_content_file() {
  local target="$1"

  if [[ -n "$EXPECTED_CONTENT_FILE" ]]; then
    cp "$EXPECTED_CONTENT_FILE" "$target"
  elif [[ -n "$EXPECTED_CONTENT" ]]; then
    printf '%s' "$EXPECTED_CONTENT" >"$target"
  else
    printf 'tcfs Linux seeded fixture %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" >"$target"
  fi
}

seed_expected_file_if_requested() {
  [[ "$SEED_EXPECTED_FILE" == "1" ]] || return 0

  local seed_stamp
  local seed_root="$LOG_DIR/seed-expected-root"
  local seed_content="$LOG_DIR/seed-expected-content"
  local push_log="$LOG_DIR/seed-expected-file-push.log"
  local push_state="$LOG_DIR/seed-expected-state.json"
  local -a args

  [[ -n "$TCFS_PATH" ]] || {
    echo "--seed-expected-file requires tcfs CLI status checks; remove --skip-status" >&2
    exit 2
  }

  if [[ -z "$EXPECTED_FILE_REL" ]]; then
    seed_stamp="$(date -u '+%Y%m%dT%H%M%SZ')"
    EXPECTED_FILE_REL="linux-smoke-${seed_stamp}/fixture.txt"
  fi
  validate_relative_file_path "$EXPECTED_FILE_REL"

  mkdir -p "$seed_root/$(dirname "$EXPECTED_FILE_REL")"
  prepare_seed_content_file "$seed_content"
  cp "$seed_content" "$seed_root/$EXPECTED_FILE_REL"
  EXPECTED_CONTENT_FILE="$seed_content"

  echo "seeding expected fixture: $EXPECTED_FILE_REL"
  args=(--config "$CONFIG_PATH" push "$seed_root" --state "$push_state")
  if [[ -n "$REMOTE_PREFIX" ]]; then
    args+=(--prefix "$REMOTE_PREFIX")
  fi

  "$TCFS_PATH" "${args[@]}" >"$push_log" 2>&1 || {
    echo "failed to seed expected fixture" >&2
    cat "$push_log" >&2 || true
    exit 1
  }
}

# ── FUSE mount ───────────────────────────────────────────────────────────────
MOUNT_PID=""
MOUNT_LOG=""

resolve_mount_point() {
  if [[ -z "$MOUNT_POINT" ]]; then
    MOUNT_POINT="$LOG_DIR/mount"
  fi
  mkdir -p "$MOUNT_POINT"
}

resolve_remote_spec() {
  if [[ -n "$REMOTE_SPEC" ]]; then
    return 0
  fi
  # Derive a default s3://<bucket>/<prefix> spec from the config when the
  # caller did not pass one explicitly.
  if [[ -z "$REMOTE_PREFIX" ]]; then
    echo "--remote-spec is required when --remote-prefix is not provided" >&2
    exit 2
  fi
  local bucket
  bucket="$(python3 - "$CONFIG_PATH" <<'PY' 2>/dev/null || true
import sys
try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib
with open(sys.argv[1], "rb") as fh:
    cfg = tomllib.load(fh)
print(cfg.get("storage", {}).get("bucket", ""))
PY
)"
  if [[ -z "$bucket" ]]; then
    echo "could not derive bucket from $CONFIG_PATH; pass --remote-spec" >&2
    exit 2
  fi
  REMOTE_SPEC="s3://${bucket}/${REMOTE_PREFIX}"
  echo "derived --remote-spec: $REMOTE_SPEC"
}

mount_fuse() {
  resolve_mount_point
  resolve_remote_spec

  MOUNT_LOG="$LOG_DIR/tcfs-mount.log"
  echo "mounting FUSE: $REMOTE_SPEC -> $MOUNT_POINT"
  "$TCFS_PATH" --config "$CONFIG_PATH" mount "$REMOTE_SPEC" "$MOUNT_POINT" \
    >"$MOUNT_LOG" 2>&1 &
  MOUNT_PID="$!"

  # Wait for the kernel to reflect the mount. mountpoint(1) is the most
  # reliable signal; fall back to scanning /proc/self/mountinfo.
  local deadline=$((SECONDS + TIMEOUT_SECS))
  while (( SECONDS < deadline )); do
    if command -v mountpoint >/dev/null 2>&1; then
      if mountpoint -q "$MOUNT_POINT" 2>/dev/null; then
        echo "FUSE mount ready: $MOUNT_POINT"
        return 0
      fi
    elif grep -q " $MOUNT_POINT " /proc/self/mountinfo 2>/dev/null; then
      echo "FUSE mount ready (mountinfo): $MOUNT_POINT"
      return 0
    fi
    if ! kill -0 "$MOUNT_PID" 2>/dev/null; then
      echo "tcfs mount exited before mount appeared; see $MOUNT_LOG" >&2
      cat "$MOUNT_LOG" >&2 || true
      exit 1
    fi
    short_pause
  done

  echo "timed out waiting for FUSE mount at $MOUNT_POINT" >&2
  cat "$MOUNT_LOG" >&2 || true
  exit 1
}

unmount_fuse() {
  [[ -n "$MOUNT_POINT" ]] || return 0
  if command -v fusermount3 >/dev/null 2>&1; then
    fusermount3 -u "$MOUNT_POINT" >/dev/null 2>&1 || true
  elif command -v fusermount >/dev/null 2>&1; then
    fusermount -u "$MOUNT_POINT" >/dev/null 2>&1 || true
  fi
  if [[ -n "$MOUNT_PID" ]] && kill -0 "$MOUNT_PID" 2>/dev/null; then
    kill "$MOUNT_PID" 2>/dev/null || true
    wait "$MOUNT_PID" 2>/dev/null || true
  fi
}

# ── Hydration ────────────────────────────────────────────────────────────────
wait_for_expected_file() {
  local path="$1"
  local attempt=0
  while (( attempt < TIMEOUT_SECS )); do
    if [[ -e "$path" ]]; then
      return
    fi
    short_pause
    attempt=$((attempt + 1))
  done
  echo "timed out waiting for expected file: $path" >&2
  exit 1
}

hydrate_expected_file() {
  local path="$1"
  local hydrated_copy="$LOG_DIR/hydrated-expected-file"
  local expected_copy="$LOG_DIR/expected-content"

  # On Linux the tcfs FUSE mount exposes files under their real names — no
  # .tc stub semantics — so a plain `cat` triggers hydration through the
  # driver's read path.
  if ! cat "$path" >"$hydrated_copy" 2>"$LOG_DIR/hydrate-read-error.log"; then
    echo "failed to read expected file: $path" >&2
    cat "$LOG_DIR/hydrate-read-error.log" >&2 || true
    exit 1
  fi
  echo "hydrated file: $path ($(wc -c <"$hydrated_copy" | tr -d '[:space:]') bytes)"

  if [[ -n "$EXPECTED_CONTENT_FILE" ]]; then
    if ! cmp -s "$EXPECTED_CONTENT_FILE" "$hydrated_copy"; then
      echo "hydrated file content mismatch against $EXPECTED_CONTENT_FILE" >&2
      exit 1
    fi
    echo "hydrated file content matched expected content file"
  elif [[ -n "$EXPECTED_CONTENT" ]]; then
    printf '%s' "$EXPECTED_CONTENT" >"$expected_copy"
    if ! cmp -s "$expected_copy" "$hydrated_copy"; then
      echo "hydrated file content mismatch against --expected-content" >&2
      exit 1
    fi
    echo "hydrated file content matched expected content"
  fi
}

# ── Evict / rehydrate via `tcfs unsync` ──────────────────────────────────────
exercise_evict_rehydrate_cycle() {
  local rel="$1"
  local mount_path="$MOUNT_POINT/$rel"
  local unsync_log="$LOG_DIR/unsync.log"

  # TODO(TIN-1422): on Linux the canonical "evict" path for the FUSE mount
  # is `tcfs unsync <hydrated-copy>` applied to the sync_root copy (the
  # FUSE mount itself is a read-through cache, not the eviction target).
  # For the scaffold we drive `tcfs unsync` against the mounted path and
  # rely on the CLI to convert it to a stub on disk — followup work needs
  # to confirm the right semantics for FUSE-backed flows and may need to
  # operate on a sync_root path instead.
  echo "requesting unsync (evict): $mount_path"
  "$TCFS_PATH" --config "$CONFIG_PATH" unsync "$mount_path" \
    >"$unsync_log" 2>&1 || {
    echo "tcfs unsync failed" >&2
    cat "$unsync_log" >&2 || true
    exit 1
  }
  cat "$unsync_log"

  # Read again — should rehydrate.
  hydrate_expected_file "$mount_path"
  echo "Linux evict/rehydrate cycle passed"
}

# ── Mutation ─────────────────────────────────────────────────────────────────
prepare_mutation_content_file() {
  local target="$1"

  if [[ -n "$MUTATION_CONTENT_FILE" ]]; then
    cp "$MUTATION_CONTENT_FILE" "$target"
  elif [[ -n "$MUTATION_CONTENT" ]]; then
    printf '%s' "$MUTATION_CONTENT" >"$target"
  else
    printf 'tcfs Linux mutation smoke %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" >"$target"
  fi
}

pull_mutation_from_remote() {
  local rel="$1"
  local expected_file="$2"
  local pulled_copy="$LOG_DIR/mutation-remote-pull"
  local attempt=0

  rm -f "$pulled_copy"
  while (( attempt < TIMEOUT_SECS )); do
    if "$TCFS_PATH" --config "$CONFIG_PATH" pull "$rel" "$pulled_copy" \
      --prefix "$REMOTE_PREFIX" >"$LOG_DIR/mutation-remote-pull.log" 2>&1; then
      if cmp -s "$expected_file" "$pulled_copy"; then
        echo "remote mutation pull matched expected content"
        return
      fi
      echo "remote mutation content mismatch for $rel" >&2
      exit 1
    fi
    short_pause
    attempt=$((attempt + 1))
  done

  echo "timed out waiting for mutation to appear in remote index: $rel" >&2
  cat "$LOG_DIR/mutation-remote-pull.log" >&2 || true
  exit 1
}

exercise_mutation() {
  local rel="$MUTATION_FILE_REL"
  local expected_file="$LOG_DIR/mutation-expected-content"

  if [[ -z "$rel" ]]; then
    rel="tcfs-linux-mutation-${USER:-user}-$$-${RANDOM:-0}.txt"
  fi
  validate_relative_file_path "$rel"

  local path="$MOUNT_POINT/$rel"
  local parent
  parent="$(dirname "$path")"
  mkdir -p "$parent"
  prepare_mutation_content_file "$expected_file"

  echo "writing Linux mutation fixture: $rel"
  if ! cp "$expected_file" "$path" 2>"$LOG_DIR/mutation-write.log"; then
    echo "Linux mutation write failed: $path" >&2
    cat "$LOG_DIR/mutation-write.log" >&2 || true
    exit 1
  fi

  wait_for_expected_file "$path"
  if ! cmp -s "$expected_file" "$path"; then
    echo "Linux mutation local content mismatch: $path" >&2
    exit 1
  fi
  echo "Linux mutation local content matched"

  # TODO(TIN-1422): the FUSE write path may not push to remote synchronously.
  # The macOS harness relies on the FileProvider extension queueing the push.
  # On Linux today the operator typically runs `tcfs push <sync_root>` to
  # publish mutations. Until the daemon's write-back path is wired in, the
  # caller may need to invoke `tcfs push` explicitly before this verification.
  pull_mutation_from_remote "$rel" "$expected_file"
  run_status "post-mutation"
}

# ── Entry sequencing ─────────────────────────────────────────────────────────
if [[ -n "$LOG_DIR_OVERRIDE" ]]; then
  LOG_DIR="$LOG_DIR_OVERRIDE"
  mkdir -p "$LOG_DIR"
else
  LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tcfs-linux-postinstall.XXXXXX")"
  trap 'rm -rf "$LOG_DIR"' EXIT
fi
echo "status logs: $LOG_DIR"

CLEANUP_REGISTERED=1
cleanup() {
  unmount_fuse
  stop_daemon
}
trap 'cleanup; [[ -n "$LOG_DIR_OVERRIDE" ]] || rm -rf "$LOG_DIR"' EXIT

install_package

TCFSD_PATH="$(resolve_bin "$TCFSD_BIN")"
assert_version "tcfsd" "$TCFSD_PATH"

if [[ "$SKIP_STATUS" -eq 0 ]]; then
  TCFS_PATH="$(resolve_bin "$TCFS_BIN")"
  assert_version "tcfs" "$TCFS_PATH"
else
  if [[ -n "$EXPECTED_FILE_REL" || "$SEED_EXPECTED_FILE" == "1" || "$EXERCISE_MUTATION" == "1" ]]; then
    TCFS_PATH="$(resolve_bin "$TCFS_BIN")"
  else
    TCFS_PATH=""
  fi
fi

start_daemon
run_status "preflight"

seed_expected_file_if_requested

mount_fuse

if [[ -n "$EXPECTED_FILE_REL" ]]; then
  EXPECTED_PATH="$MOUNT_POINT/$EXPECTED_FILE_REL"
  require_expected_remote_index "$EXPECTED_FILE_REL"
  wait_for_expected_file "$EXPECTED_PATH"
  hydrate_expected_file "$EXPECTED_PATH"
  if [[ "$EXERCISE_EVICT_REHYDRATE" == "1" ]]; then
    exercise_evict_rehydrate_cycle "$EXPECTED_FILE_REL"
  fi
else
  echo "warning: --expected-file not provided; hydration was not exercised" >&2
fi

if [[ "$EXERCISE_MUTATION" == "1" ]]; then
  exercise_mutation
fi

run_status "post-hydrate"

echo "Linux post-install smoke passed"
