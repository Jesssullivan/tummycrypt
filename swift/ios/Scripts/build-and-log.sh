#!/usr/bin/env bash
# Wrapper: runs full-build.sh with full logging
set -euo pipefail

LOG="/tmp/tcfs-ios-build-$(date +%Y%m%d-%H%M%S).log"
echo "==> TCFS iOS Build — logging to $LOG"
echo "==> Run from Terminal.app (keychain access required)"
echo ""

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Tee all output to log file
"$SCRIPT_DIR/full-build.sh" 2>&1 | tee "$LOG"
RC=${PIPESTATUS[0]}

echo ""
if [ $RC -eq 0 ]; then
  echo "==> BUILD SUCCEEDED — log at $LOG"
else
  echo "==> BUILD FAILED (exit $RC) — log at $LOG"
fi
exit $RC
