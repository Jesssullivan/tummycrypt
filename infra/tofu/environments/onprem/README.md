# TCFS On-Prem OpenTofu Environment

This environment captures the Tinyland on-prem TCFS migration surface. It is
intentionally inert by default: `enable_tailnet_candidate_services=false`
creates no resources.

## Current Live Boundary

Live readback on 2026-04-29 shows:

- `nats-0`, `seaweedfs-0`, and `tcfs-backend-tcfs-backend-worker` are healthy.
- `helm list -n tcfs` has no release state.
- `tcfs/nats` and `tcfs/seaweedfs` carry live Tailscale annotations as drift.
- `data-nats-0` and `data-seaweedfs-0` are `local-path` PVCs pinned to
  `honey`.
- Durable storage targets now exist in `blahaj` and live Kubernetes:
  `openebs-bumble-messaging-retain` for NATS and
  `openebs-bumble-s3-retain` for SeaweedFS.

This environment does not adopt those objects and does not move data. It only
adds a source-owned candidate path for tailnet exposure once the migration
gates are ready.

## Safe Validation

```bash
just onprem-tofu-validate
```

Read-only data inventory before migration work:

```bash
TCFS_CONTEXT=honey just onprem-data-inventory
```

Storage migration planning:

```text
docs/ops/tcfs-onprem-storage-migration.md
```

## Candidate Tailnet Smoke

Only after `scripts/tcfs-onprem-preflight.sh` is clean enough for migration
work, run a plan with candidate hostnames that do not collide with the live
canonical devices:

```bash
tofu plan \
  -var='enable_tailnet_candidate_services=true'
```

Do not switch the candidate hostnames to `nats-tcfs` or `seaweedfs-tcfs` until
the live Service annotations have been removed through a source-controlled
cutover plan.

## Migration Gates

Before any live apply:

1. capture NATS JetStream and SeaweedFS data inventory;
2. migrate or clone data to the selected retained OpenEBS/ZFS classes;
3. smoke candidate Tailscale Services with `honey-sting-tailnet` placement;
4. remove the old live annotations through the selected authority path;
5. cut over canonical tailnet hostnames only after clients pass smoke;
6. keep rollback instructions and retained data paths explicit.
