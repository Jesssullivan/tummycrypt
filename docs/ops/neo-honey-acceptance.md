# Neo-Honey Live Acceptance

Date: 2026-04-15

The canonical live fleet acceptance lane is `neo-honey`.

Within the broader acceptance stack, this lane sits between packaged artifact
smoke and the real-host matrix in
[Lab Host Acceptance Matrix](lab-host-acceptance-matrix.md).

This lane is meant to answer one operational question:

Can one real device (`neo`) push a change through live SeaweedFS + NATS
JetStream, and can a second real device (`honey`) observe and pull that change
successfully?

## Required environment

Set these before running the lane:

```bash
export TCFS_E2E_LIVE=1
export TCFS_S3_ENDPOINT=http://seaweedfs-tcfs:8333
export TCFS_S3_BUCKET=tcfs
export AWS_ACCESS_KEY_ID=<from seaweedfs-admin secret>
export AWS_SECRET_ACCESS_KEY=<from seaweedfs-admin secret>
export TCFS_NATS_URL=nats://nats-tcfs:4222
```

## Canonical operator command

Before running the mutating smoke, run the read-only G2/G3 preflight:

```bash
task lazy:honey-backbone-preflight
```

That helper writes an evidence packet under
`docs/release/evidence/honey-backbone-preflight-<UTC>/` and checks daemon
storage/NATS status, NATS endpoint reachability from both hosts, device
registry convergence, and placeholder-shaped device public keys. Use
`--strict` directly when a failing shell status is desired:

```bash
scripts/honey-backbone-preflight.sh --strict
```

The preflight does not enroll devices, edit configs, restart services, or move
data.

```bash
just neo-honey-smoke
```

Equivalent direct commands:

```bash
cargo test -p tcfs-e2e --test fleet_live seaweedfs_health_check -- --nocapture
cargo test -p tcfs-e2e --test fleet_live nats_connect_and_jetstream -- --nocapture
cargo test -p tcfs-e2e --test fleet_live neo_honey_two_device_sync_smoke -- --nocapture
```

## Pass criteria

The lane passes only if all of the following are true:

1. SeaweedFS health check succeeds against the live endpoint.
2. NATS connection succeeds and JetStream listing works.
3. `neo_honey_two_device_sync_smoke` succeeds end to end:
   - `neo` uploads a unique test file
   - a sync event is published to NATS
   - `honey` receives the event and pulls the manifest-backed file
   - pulled content matches the uploaded content exactly
4. The test cleans up the temporary remote objects it created.

## Failure interpretation

- If SeaweedFS health fails, treat the lane as infrastructure-unavailable.
- If NATS connectivity or JetStream listing fails, treat the lane as fleet-sync-unavailable.
- If the read-only preflight reports split NATS endpoints or divergent device
  registries, treat G2/G3 as blocked before running the mutating smoke.
- If the two-device smoke fails after both backends are reachable, treat the lane
  as a product regression in the live sync path.

## Cadence

Current recommendation:

1. Run manually before any release that changes sync, fleet, packaging, or macOS surfaces.
2. Run manually after any infrastructure changes to SeaweedFS, NATS, or Tailscale exposure.
3. Promote to a scheduled lane only after the environment and credentials are stable enough to avoid noisy false failures.
