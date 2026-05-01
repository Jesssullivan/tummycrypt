#!/bin/bash
#
# macOS .pkg postinstall hook for TCFS.
#
# Package scripts run as root. Keep this script tolerant: package installation
# should not fail just because a user session is not active yet.
#
set -u

APP_PATH="${TCFS_POSTINSTALL_APP_PATH:-/Applications/TCFSProvider.app}"
LSREGISTER_BIN="${TCFS_POSTINSTALL_LSREGISTER:-/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister}"
LAUNCHCTL_BIN="${TCFS_POSTINSTALL_LAUNCHCTL:-/bin/launchctl}"
SUDO_BIN="${TCFS_POSTINSTALL_SUDO:-/usr/bin/sudo}"
STAT_BIN="${TCFS_POSTINSTALL_STAT:-/usr/bin/stat}"
ID_BIN="${TCFS_POSTINSTALL_ID:-/usr/bin/id}"
CHOWN_BIN="${TCFS_POSTINSTALL_CHOWN:-/usr/sbin/chown}"

# A LaunchAgent under /Library/LaunchAgents runs in each user session and can
# resolve that user's ~/.config/tcfs/config.toml. Do not write into $HOME here:
# package scripts run as root, so $HOME is not the installing user's home.
PLIST_DIR="${TCFS_POSTINSTALL_LAUNCHAGENTS_DIR:-/Library/LaunchAgents}"
PLIST_PATH="${PLIST_DIR}/io.tinyland.tcfsd.plist"
mkdir -p "$PLIST_DIR"
if [ ! -f "$PLIST_PATH" ]; then
  cat >"$PLIST_PATH" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>io.tinyland.tcfsd</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/sh</string>
    <string>-lc</string>
    <string>exec /usr/local/bin/tcfsd --config "$HOME/.config/tcfs/config.toml" --mode daemon</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>StandardOutPath</key>
  <string>/tmp/tcfsd.stdout.log</string>
  <key>StandardErrorPath</key>
  <string>/tmp/tcfsd.stderr.log</string>
</dict>
  </plist>
PLIST
  chmod 644 "$PLIST_PATH"
  "$CHOWN_BIN" root:wheel "$PLIST_PATH" 2>/dev/null || true
fi

CONSOLE_USER="$("$STAT_BIN" -f %Su /dev/console 2>/dev/null || true)"
if [ -n "$CONSOLE_USER" ] && [ "$CONSOLE_USER" != "root" ]; then
  CONSOLE_UID="$("$ID_BIN" -u "$CONSOLE_USER" 2>/dev/null || true)"
  if [ -n "$CONSOLE_UID" ]; then
    if [ -d "$APP_PATH" ] && [ -x "$LSREGISTER_BIN" ]; then
      "$LAUNCHCTL_BIN" asuser "$CONSOLE_UID" \
        "$SUDO_BIN" -u "$CONSOLE_USER" \
        "$LSREGISTER_BIN" -f "$APP_PATH" 2>/dev/null || true
    fi
    "$LAUNCHCTL_BIN" bootout "gui/${CONSOLE_UID}" "$PLIST_PATH" 2>/dev/null || true
    "$LAUNCHCTL_BIN" bootstrap "gui/${CONSOLE_UID}" "$PLIST_PATH" 2>/dev/null || true
    "$LAUNCHCTL_BIN" enable "gui/${CONSOLE_UID}/io.tinyland.tcfsd" 2>/dev/null || true
  fi
fi

exit 0
