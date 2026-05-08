#!/usr/bin/env bash
#
# Lifecycle-named entrypoint for the Linux TCFS parity proof harness.
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/lazy-hydration-linux-demo.sh" "$@"
