#!/usr/bin/env bash
#
# Archive neo FileProvider divergence before any cleanup, then optionally install
# the published package and quarantine stale user/build-tree registrations.
#
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/macos-fileprovider-neo-cleanup-packet.sh [options]

Archive the current macOS FileProvider divergence state before cleanup. By
default this is non-mutating inventory only.

Options:
  --evidence-dir <path>      Evidence dir. Default: docs/release/evidence/macos-fileprovider-neo-cleanup-<UTC>
  --pkg <path>               Published .pkg to install/verify
  --install-pkg              Run installer -pkg <pkg> -target /
  --install-mode <mode>      Install auth mode: sudo-n, sudo, or osascript. Default: sudo-n
  --quarantine-stale         Move stale ~/Applications/build-tree TCFSProvider apps after inventory
  --app-path <path>          Installed app path. Default: /Applications/TCFSProvider.app
  --stale-app <path>         Additional stale app path to quarantine
  --tcfs <path>              Expected tcfs binary for preflight
  --tcfsd <path>             Expected tcfsd binary for preflight
  --strict-preflight         Run TCFS_REQUIRE_PRODUCTION_SIGNING=1 task lazy:macos-finder-preflight
  -h, --help                 Show this help
EOF
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

shell_quote() {
  printf '%q' "$1"
}

sh_quote() {
  printf "'"
  printf '%s' "$1" | sed "s/'/'\\\\''/g"
  printf "'"
}

applescript_string() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  printf '%s' "$value"
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
evidence_dir="${TCFS_MACOS_CLEANUP_EVIDENCE_DIR:-$REPO_ROOT/docs/release/evidence/macos-fileprovider-neo-cleanup-${timestamp}}"
pkg_path="${PKG_PATH:-}"
install_pkg=0
install_mode="${INSTALL_MODE:-sudo-n}"
quarantine_stale=0
app_path="${APP_PATH:-/Applications/TCFSProvider.app}"
tcfs_bin="${TCFS_BIN:-}"
tcfsd_bin="${TCFSD_BIN:-}"
strict_preflight=0
install_pkg_rc=0
install_status_label=not-run
stale_apps=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --evidence-dir)
      [[ $# -ge 2 ]] || fail "--evidence-dir requires a value"
      evidence_dir="$2"
      shift 2
      ;;
    --pkg)
      [[ $# -ge 2 ]] || fail "--pkg requires a value"
      pkg_path="$2"
      shift 2
      ;;
    --install-pkg)
      install_pkg=1
      shift
      ;;
    --install-mode)
      [[ $# -ge 2 ]] || fail "--install-mode requires a value"
      install_mode="$2"
      shift 2
      ;;
    --quarantine-stale)
      quarantine_stale=1
      shift
      ;;
    --app-path)
      [[ $# -ge 2 ]] || fail "--app-path requires a value"
      app_path="$2"
      shift 2
      ;;
    --stale-app)
      [[ $# -ge 2 ]] || fail "--stale-app requires a value"
      stale_apps+=("$2")
      shift 2
      ;;
    --tcfs)
      [[ $# -ge 2 ]] || fail "--tcfs requires a value"
      tcfs_bin="$2"
      shift 2
      ;;
    --tcfsd)
      [[ $# -ge 2 ]] || fail "--tcfsd requires a value"
      tcfsd_bin="$2"
      shift 2
      ;;
    --strict-preflight)
      strict_preflight=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$install_mode" in
  sudo-n|sudo|osascript)
    ;;
  *)
    fail "unsupported install mode: $install_mode"
    ;;
esac

mkdir -p "$evidence_dir"
inventory_dir="$evidence_dir/pre-cleanup-inventory"
mkdir -p "$inventory_dir"

run_capture() {
  local name="$1"
  shift
  {
    printf '$'
    printf ' %q' "$@"
    printf '\n'
    "$@"
  } >"$inventory_dir/$name.out" 2>"$inventory_dir/$name.err" || true
}

printf 'archiving FileProvider divergence inventory: %s\n' "$inventory_dir"

{
  printf 'created_at_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'app_path=%s\n' "$app_path"
  printf 'pkg_path=%s\n' "$pkg_path"
  printf 'install_pkg=%s\n' "$install_pkg"
  printf 'install_mode=%s\n' "$install_mode"
  printf 'quarantine_stale=%s\n' "$quarantine_stale"
  printf 'strict_preflight=%s\n' "$strict_preflight"
} >"$evidence_dir/run-metadata.env"

run_capture uname uname -a
run_capture path-resolution sh -c 'command -v tcfs || true; command -v tcfsd || true; command -v TCFSProvider || true'
run_capture tcfs-version sh -c 'tcfs --version 2>/dev/null || true'
run_capture tcfsd-version sh -c 'tcfsd --version 2>/dev/null || true'
# shellcheck disable=SC2016
run_capture app-locations sh -c 'find /Applications "$HOME/Applications" "$HOME/git/tummycrypt/build" -maxdepth 4 -name TCFSProvider.app -print 2>/dev/null | sort'
run_capture pluginkit sh -c 'pluginkit -m -A -D -vvv 2>/dev/null | grep -A8 -B4 -E "io\\.tinyland\\.tcfs|TCFSProvider|TCFSFileProvider" || true'
# shellcheck disable=SC2016
run_capture launchctl sh -c 'launchctl print gui/$(id -u) 2>/dev/null | grep -E "io\\.tinyland\\.tcfs|tcfsd|TCFSProvider" || true'
# shellcheck disable=SC2016
run_capture cloudstorage sh -c 'find "$HOME/Library/CloudStorage" -maxdepth 2 -iname "*TCFS*" -print 2>/dev/null | sort'
# shellcheck disable=SC2016
run_capture configs sh -c 'find "$HOME/.config" "$HOME/Library/Application Support" "$HOME/Library/Group Containers" -maxdepth 5 \( -iname "*tcfs*" -o -iname "*tinyland*" \) -print 2>/dev/null | grep -Evi "token|secret|password|credential|api[_-]?key|auth[_-]?key" | sort'
# shellcheck disable=SC2016
run_capture sockets sh -c 'find /tmp "$HOME/Library" -maxdepth 5 \( -name "*tcfs*.sock" -o -name "*tcfs*.socket" \) -print 2>/dev/null | sort'
# shellcheck disable=SC2016
run_capture tcfs-home-inventory sh -c 'find "$HOME/tcfs" -maxdepth 4 -print 2>/dev/null | grep -Evi "/secrets($|/)|token|secret|password|credential|api[_-]?key|auth[_-]?key" | sort | head -500'

if [[ -d "$app_path" ]]; then
  run_capture installed-app-codesign codesign -dv --verbose=4 "$app_path"
  run_capture installed-app-spctl spctl -a -vv "$app_path"
  if [[ -d "$app_path/Contents/Extensions/TCFSFileProvider.appex" ]]; then
    run_capture installed-extension-codesign codesign -dv --verbose=4 "$app_path/Contents/Extensions/TCFSFileProvider.appex"
  fi
fi

if [[ -n "$pkg_path" ]]; then
  [[ -f "$pkg_path" ]] || fail "package does not exist: $pkg_path"
  run_capture pkgutil-check-signature pkgutil --check-signature "$pkg_path"
  run_capture pkgutil-expand-list sh -c "tmp=\$(mktemp -d); pkgutil --expand $(shell_quote "$pkg_path") \"\$tmp/pkg\" >/dev/null && find \"\$tmp/pkg\" -maxdepth 4 -print | sort; rm -rf \"\$tmp\""
fi

if [[ "$install_pkg" == "1" ]]; then
  [[ -n "$pkg_path" ]] || fail "--install-pkg requires --pkg"
  command -v installer >/dev/null 2>&1 || fail "installer not found"
  printf 'installing published package: %s\n' "$pkg_path"
  printf 'install_mode=%s\n' "$install_mode" >"$evidence_dir/install-pkg-command.env"
  case "$install_mode" in
    sudo-n)
      printf 'command=sudo -n installer -pkg %s -target /\n' "$(sh_quote "$pkg_path")" >>"$evidence_dir/install-pkg-command.env"
      # shellcheck disable=SC2024
      sudo -n installer -pkg "$pkg_path" -target / >"$evidence_dir/install-pkg.out" 2>"$evidence_dir/install-pkg.err" || install_pkg_rc=$?
      ;;
    sudo)
      printf 'command=sudo installer -pkg %s -target /\n' "$(sh_quote "$pkg_path")" >>"$evidence_dir/install-pkg-command.env"
      # shellcheck disable=SC2024
      sudo installer -pkg "$pkg_path" -target / >"$evidence_dir/install-pkg.out" 2>"$evidence_dir/install-pkg.err" || install_pkg_rc=$?
      ;;
    osascript)
      command -v osascript >/dev/null 2>&1 || fail "osascript not found"
      install_cmd="/usr/sbin/installer -pkg $(sh_quote "$pkg_path") -target /"
      printf 'command=osascript do shell script %s with administrator privileges\n' "$(sh_quote "$install_cmd")" >>"$evidence_dir/install-pkg-command.env"
      osascript -e "do shell script \"$(applescript_string "$install_cmd")\" with administrator privileges" >"$evidence_dir/install-pkg.out" 2>"$evidence_dir/install-pkg.err" || install_pkg_rc=$?
      ;;
  esac
  install_status_label="$install_pkg_rc"
  printf 'status=%s\n' "$install_pkg_rc" >"$evidence_dir/install-pkg-status.env"
  if [[ "$install_pkg_rc" -eq 0 ]]; then
    # shellcheck disable=SC2016
    run_capture postinstall-app-locations sh -c 'find /Applications "$HOME/Applications" "$HOME/git/tummycrypt/build" -maxdepth 4 -name TCFSProvider.app -print 2>/dev/null | sort'
    run_capture postinstall-app-codesign codesign -dv --verbose=4 "$app_path"
  fi
fi

if [[ "$quarantine_stale" == "1" ]]; then
  quarantine_dir="$evidence_dir/quarantined-stale-apps"
  mkdir -p "$quarantine_dir"
  stale_apps+=("$HOME/Applications/TCFSProvider.app" "$REPO_ROOT/build/TCFSProvider.app" "$REPO_ROOT/build/fileprovider/TCFSProvider.app")
  : >"$evidence_dir/quarantine-actions.log"
  for stale in "${stale_apps[@]}"; do
    [[ -e "$stale" ]] || continue
    if [[ "$(cd "$stale/.." && pwd -P)/$(basename "$stale")" == "$(cd "$app_path/.." 2>/dev/null && pwd -P)/$(basename "$app_path")" ]]; then
      printf 'skip installed app path: %s\n' "$stale" >>"$evidence_dir/quarantine-actions.log"
      continue
    fi
    dest="$quarantine_dir/$(basename "$(dirname "$stale")")-$(basename "$stale")"
    printf 'move %s -> %s\n' "$stale" "$dest" >>"$evidence_dir/quarantine-actions.log"
    mv "$stale" "$dest"
  done
fi

preflight_rc=0
preflight_status_label=not-run
if [[ "$strict_preflight" == "1" ]]; then
  printf 'running strict production signing preflight\n'
  (
    cd "$REPO_ROOT"
    export TCFS_REQUIRE_PRODUCTION_SIGNING=1
    export APP_PATH="$app_path"
    if [[ -n "$tcfs_bin" ]]; then
      export TCFS_BIN="$tcfs_bin"
    fi
    if [[ -n "$tcfsd_bin" ]]; then
      export TCFSD_BIN="$tcfsd_bin"
    fi
    task lazy:macos-finder-preflight
  ) >"$evidence_dir/strict-preflight.out" 2>"$evidence_dir/strict-preflight.err" || preflight_rc=$?
  preflight_status_label="$preflight_rc"
fi

cat >"$evidence_dir/README.md" <<EOF
# macOS FileProvider neo Cleanup Packet

Created: $(date -u +%Y-%m-%dT%H:%M:%SZ)

This packet archives divergence before cleanup: binary versions, PATH
resolution, app bundle locations, PlugInKit records, signing/profile state,
CloudStorage roots, configs, sockets, launchd labels, and a bounded
\`~/tcfs\` inventory. Sensitive-looking config paths are redacted from the
bounded listings by name.

The package source is the published \`.pkg\` when \`--pkg\` is provided. Stale
\`~/Applications\` or build-tree apps are moved only when
\`--quarantine-stale\` is explicitly set, after this inventory exists.

Install mode: \`$install_mode\`.

Install status: \`$install_status_label\`.

Strict production-adjacent Finder smoke remains blocked unless
\`TCFS_REQUIRE_PRODUCTION_SIGNING=1 task lazy:macos-finder-preflight\` passes.
This run's strict preflight status: \`$preflight_status_label\`.
EOF

printf 'macOS cleanup evidence: %s\n' "$evidence_dir"
if [[ "$install_pkg_rc" -ne 0 ]]; then
  printf 'package install failed; see %s\n' "$evidence_dir/install-pkg.err" >&2
  exit "$install_pkg_rc"
fi
if [[ "$preflight_rc" -ne 0 ]]; then
  printf 'strict preflight failed; see %s\n' "$evidence_dir/strict-preflight.err" >&2
  exit "$preflight_rc"
fi
