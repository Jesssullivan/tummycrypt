#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    echo "missing required env: $name" >&2
    exit 1
  fi
}

echo "==> neo-honey live acceptance smoke"
echo "repo: $ROOT"

require_env TCFS_E2E_LIVE
require_env TCFS_S3_ENDPOINT
require_env TCFS_S3_BUCKET
require_env AWS_ACCESS_KEY_ID
require_env AWS_SECRET_ACCESS_KEY
require_env TCFS_NATS_URL

if [[ "${TCFS_E2E_LIVE}" != "1" ]]; then
  echo "TCFS_E2E_LIVE must be set to 1" >&2
  exit 1
fi

echo "==> proving SeaweedFS health"
cargo test -p tcfs-e2e --test fleet_live seaweedfs_health_check -- --nocapture

echo "==> proving NATS JetStream connectivity"
cargo test -p tcfs-e2e --test fleet_live nats_connect_and_jetstream -- --nocapture

echo "==> proving named neo-honey two-device sync path"
cargo test -p tcfs-e2e --test fleet_live neo_honey_two_device_sync_smoke -- --nocapture

echo "==> neo-honey smoke passed"
