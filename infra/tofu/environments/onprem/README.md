# TCFS On-Prem OpenTofu Environment

This environment captures the Tinyland on-prem TCFS migration surface. It is
intentionally inert by default: `enable_tailnet_candidate_services=false`
and `enable_stateful_migration_candidate_workloads=false` create no resources.

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

This environment does not adopt those objects and does not move data. It adds a
source-owned candidate path for retained target PVCs, non-canonical
NATS/SeaweedFS workloads, and candidate tailnet exposure once the migration
gates are ready.

All migration resources are disabled by default. The target retained PVCs are
created only when `enable_stateful_migration_target_pvcs=true`; candidate
workloads are created only when
`enable_stateful_migration_candidate_workloads=true`.

## Safe Validation

```bash
just onprem-tofu-validate
```

Read-only data inventory before migration work:

```bash
TCFS_CONTEXT=honey just onprem-data-inventory
```

Non-mutating downtime command render:

```bash
TCFS_CONTEXT=honey just onprem-migration-plan facts
TCFS_CONTEXT=honey just onprem-migration-plan render-target-pvc-commands
TCFS_CONTEXT=honey just onprem-migration-plan render-import-pods
TCFS_CONTEXT=honey just onprem-migration-plan render-transfer-commands
TCFS_CONTEXT=honey just onprem-migration-plan render-candidate-apply-commands
TCFS_CONTEXT=honey just onprem-migration-plan render-candidate-smoke-commands
TCFS_CONTEXT=honey just onprem-migration-plan render-cutover-commands
TCFS_CONTEXT=honey just onprem-migration-plan render-rollback-commands
```

Storage migration planning:

```text
docs/ops/tcfs-onprem-storage-migration.md
```

Plan target retained PVC creation without enabling tailnet candidate Services:

```bash
tofu plan \
  -var='enable_stateful_migration_target_pvcs=true'
```

For the approved downtime window, prefer rendering the reviewed plan/apply
pair:

```bash
TCFS_CONTEXT=honey just onprem-migration-plan render-target-pvc-commands
```

Plan retained PVCs plus non-canonical candidate workloads and candidate tailnet
Services:

```bash
tofu plan \
  -var='enable_stateful_migration_target_pvcs=true' \
  -var='enable_stateful_migration_candidate_workloads=true' \
  -var='enable_tailnet_candidate_services=true'
```

For the approved downtime window, prefer rendering the reviewed candidate
plan/apply pair:

```bash
TCFS_CONTEXT=honey just onprem-migration-plan render-candidate-apply-commands
```

## Candidate Tailnet Smoke

Only after `scripts/tcfs-onprem-preflight.sh` is clean enough for migration
work, run a plan with candidate hostnames that do not collide with the live
canonical devices:

```bash
tofu plan \
  -var='enable_tailnet_candidate_services=true'
```

The candidate tailnet Services intentionally select the non-canonical candidate
labels, not the live `app=nats` or `app=seaweedfs` labels. If candidate
workloads are not enabled, the tailnet Services should have no backend
endpoints; that is safer than exposing the existing honey-local singleton pods.

Do not switch the candidate hostnames to `nats-tcfs` or `seaweedfs-tcfs` until
the live Service annotations have been removed through a source-controlled
cutover plan. Use `render-cutover-commands` to review that sequence before the
downtime window; the script only prints commands and does not mutate the
cluster.

## Migration Gates

Before any live apply:

1. capture NATS JetStream and SeaweedFS data inventory;
2. migrate or clone data to the selected retained OpenEBS/ZFS classes;
3. smoke candidate Tailscale Services with `honey-sting-tailnet` placement;
4. remove the old live annotations through the selected authority path;
5. cut over canonical tailnet hostnames only after clients pass smoke;
6. keep rollback instructions and retained data paths explicit.
