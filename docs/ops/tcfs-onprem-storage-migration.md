# TCFS On-Prem Storage Migration Runbook

Date: 2026-04-29

This runbook captures the safe migration shape for moving the live TCFS NATS
and SeaweedFS state off honey-local `local-path` PVCs and onto retained
OpenEBS/ZFS storage classes.

It is intentionally not an apply script. Do not use this as approval to mutate
the live cluster without an explicit downtime window.

## Current Authority

Live readback shows `nats` and `seaweedfs` are kubectl-applied singleton
StatefulSets. They are not currently owned by Helm or OpenTofu.

Current live state:

- `statefulset/nats`: `replicas=1`, selector `app=nats`, `serviceName=nats`
- `statefulset/seaweedfs`: `replicas=1`, selector `app=seaweedfs`,
  `serviceName=seaweedfs`
- `pvc/data-nats-0`: `local-path`, retained PV on `honey`
- `pvc/data-seaweedfs-0`: `local-path`, retained PV on `honey`
- `service/nats`: canonical Tailscale annotations, no ProxyClass
- `service/seaweedfs`: canonical Tailscale annotations, no ProxyClass

Durable targets now exist:

- NATS target: `openebs-bumble-messaging-retain`
- SeaweedFS target: `openebs-bumble-s3-retain`

Source-owned target PVC scaffolding is disabled by default in
`infra/tofu/environments/onprem`:

- `tcfs-nats-openebs-target` on `openebs-bumble-messaging-retain`
- `tcfs-seaweedfs-openebs-target` on `openebs-bumble-s3-retain`

## Non-Negotiable Safety Constraints

- Do not patch `volumeClaimTemplates.storageClassName` in place. StatefulSet
  PVC templates are not a safe mutable migration surface.
- Do not delete old PVCs or PVs during the first migration pass. They are the
  rollback source.
- Do not treat adding `tailscale.com/proxy-class` to the old Services as a
  durable fix. That would hide authority drift without solving storage.
- Do not schedule one helper Pod expecting to mount both old and new PVCs. The
  old PVs are honey-local and the target OpenEBS/ZFS PVs are bumble-local, so
  a single Pod cannot satisfy both node-locality constraints.
- Do not cut canonical tailnet hostnames over until candidate endpoints pass
  smoke.

## Required Preflight

Run from `tummycrypt`:

```bash
TCFS_CONTEXT=honey just onprem-data-inventory
TCFS_CONTEXT=honey just onprem-preflight
just onprem-tofu-validate
```

Expected current inventory while this runbook was written:

- NATS JetStream: 3 streams, 7 consumers, 2,688 messages, 788,341 bytes
- SeaweedFS: 14 volumes and about 61 MiB under `/data`
- both current PVCs remain `local-path` on `honey`
- both target retained OpenEBS/ZFS classes exist on `bumble`

If those facts change materially, stop and update the plan before migrating.

## Downtime Gate

This migration needs an explicit maintenance window unless a source-owned
app-native online export/import path is added first.

Before copying state:

1. Stop external writes and TCFS worker writes.
2. Capture `just onprem-data-inventory` output.
3. Record old PVC names, PV names, node, and host paths.
4. Create target retained PVCs with distinct names by planning and applying
   `enable_stateful_migration_target_pvcs=true`.
5. Confirm rollback path uses the old StatefulSets and old retained PVCs.

## Recommended Source-Owned Implementation

The next implementation PR should add migration manifests or OpenTofu resources
that can be planned and reviewed before live use.

Minimum source-owned resources:

- target retained PVC for NATS on `openebs-bumble-messaging-retain`
  (`tcfs-nats-openebs-target`)
- target retained PVC for SeaweedFS on `openebs-bumble-s3-retain`
  (`tcfs-seaweedfs-openebs-target`)
- explicit copy/export/import mechanism that handles honey-to-bumble node
  locality
- replacement or adopted StatefulSet definitions that bind to the target PVCs
- candidate Tailscale Services using `honey-sting-tailnet`
- rollback notes that preserve the old retained PVs and old canonical Services

Source-owned command rendering now exists for the downtime copy lane:

```bash
TCFS_CONTEXT=honey just onprem-migration-plan facts
TCFS_CONTEXT=honey just onprem-migration-plan render-import-pods
TCFS_CONTEXT=honey just onprem-migration-plan render-transfer-commands
```

Those commands are render-only. They are meant to create reviewable evidence
for the maintenance window, not to authorize live mutation. The rendered
transfer path streams from the source node-local PV path over SSH into a
target-PVC import Pod, which avoids the unsafe single-Pod honey-to-bumble mount
assumption.

Acceptable data movement shapes:

- app-native export/import if NATS and SeaweedFS tooling can produce complete
  snapshots;
- two-hop copy where a honey-side reader exports old PVC contents and a
  bumble-side writer imports into target PVCs;
- operator-mediated tar transfer only if the inventory remains small and the
  transfer transcript is kept with the migration evidence.

## Cutover Order

Use this order for the eventual live migration:

1. Run preflight and data inventory.
2. Quiesce writers.
3. Create target retained PVCs.
4. Copy or import data to target PVCs.
5. Start replacement NATS and SeaweedFS workloads against target PVCs.
6. Run internal cluster smoke against replacement Services.
7. Enable candidate tailnet Services with `honey-sting-tailnet`.
8. Smoke candidate tailnet hostnames.
9. Remove canonical Tailscale annotations from old Services through the chosen
   authority path.
10. Assign canonical hostnames to source-owned Services.
11. Run fleet smoke.
12. Keep old retained PVs until rollback is explicitly declared unnecessary.

## Rollback

Rollback before canonical tailnet cutover:

1. Stop replacement workloads.
2. Keep target PVCs retained for forensic comparison.
3. Restart old StatefulSets against `data-nats-0` and `data-seaweedfs-0`.
4. Leave old canonical Services unchanged.
5. Run `just onprem-preflight` and app smoke.

Rollback after canonical tailnet cutover:

1. Stop replacement workloads.
2. Restore old canonical Tailscale annotations or Services through source
   control.
3. Restart old StatefulSets against old PVCs.
4. Re-run fleet and tailnet smoke.
5. Preserve both old and target PVs until data consistency is verified.

## Completion Criteria

The migration is not complete until all of the following are true:

- NATS and SeaweedFS StatefulSets are source-owned.
- Their state PVCs use retained OpenEBS/ZFS classes.
- `just onprem-data-inventory` reports target classes for live state PVCs.
- canonical tailnet Services are source-owned and use `honey-sting-tailnet`.
- `blahaj` tailnet proxy placement no longer fails on `tcfs/nats` or
  `tcfs/seaweedfs`.
- rollback evidence is recorded in the tracker.
