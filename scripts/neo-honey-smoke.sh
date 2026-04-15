#!/usr/bin/env bash
set -euo pipefail

export TCFS_E2E_LIVE=1
export TCFS_E2E_SCENARIO="${TCFS_E2E_SCENARIO:-neo-honey}"
export TCFS_S3_ENDPOINT="${TCFS_S3_ENDPOINT:-http://100.120.66.67:8333}"
export TCFS_S3_BUCKET="${TCFS_S3_BUCKET:-tcfs}"
export TCFS_NATS_URL="${TCFS_NATS_URL:-nats://100.71.19.127:4222}"

: "${AWS_ACCESS_KEY_ID:?AWS_ACCESS_KEY_ID must be set for neo-honey smoke}"
: "${AWS_SECRET_ACCESS_KEY:?AWS_SECRET_ACCESS_KEY must be set for neo-honey smoke}"

echo "==> tcfs live acceptance lane: ${TCFS_E2E_SCENARIO}"
echo "    S3 endpoint: ${TCFS_S3_ENDPOINT}"
echo "    S3 bucket:   ${TCFS_S3_BUCKET}"
echo "    NATS URL:    ${TCFS_NATS_URL}"

tests=(
  seaweedfs_health_check
  nats_connect_and_jetstream
  live_push_pull_roundtrip
  neo_honey_two_device_sync_smoke
)

for test_name in "${tests[@]}"; do
  echo ""
  echo "==> Running ${test_name}"
  cargo test -p tcfs-e2e --test fleet_live "${test_name}" -- --nocapture
done

echo ""
echo "==> neo-honey smoke lane complete"
