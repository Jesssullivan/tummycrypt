# TCFS honey cluster pause (2026-09-29, TIN-5136)

## Rulings

- "Cluster scale-to-0 only" (TIN-5136 comment): at 2026-09-29T16:45Z the five
  tcfs cluster workloads in namespace `tcfs` on the honey RKE2 cluster were
  scaled to 0 replicas imperatively.
- "Close the tcfs source drift" (operator ruling 2026-09-29): source must
  declare the paused state so a reconcile does not restart what was paused.
- "Follow-up PR: capture at 0 replicas" (TIN-5136): the four workloads that
  had no source declaration are captured from their live specs as manifests
  at 0 replicas under `infra/k8s/honey/tcfs-paused/`.

Host clients were deliberately left alone: `tcfsd` on honey, bumble and neo,
and the honey FUSE mount keep running.

## What was paused and where it is declared

| Workload | Kind | Source declaration | Paused in source |
|----------|------|--------------------|------------------|
| `tcfs-backend-tcfs-backend-worker` | Deployment | `infra/k8s/charts/tcfs-backend` (direct release `tcfs-backend`, via `scripts/tcfs-backend-deploy.sh`) | yes: `replicaCount: 0`, `autoscaling.minReplicas: 0` |
| `seaweedfs` | StatefulSet | `infra/k8s/honey/tcfs-paused/01-seaweedfs-statefulset.yaml` (captured; live object from `kubectl apply` on 2026-04-07) | yes: `replicas: 0` |
| `nats` | StatefulSet | `infra/k8s/honey/tcfs-paused/02-nats-statefulset.yaml` (captured; live object from `kubectl apply` on 2026-04-07) | yes: `replicas: 0` |
| `tcfs-s3-posture-gateway` | Deployment | retired from source 2026-10-15 (was `03-tcfs-s3-posture-gateway-deployment.yaml`; live object from `kubectl apply` on 2026-05-20) | live object still at 0; deletion is a separate ruling |
| `tcfs-s3-smoke-tunnel` | Deployment | retired from source 2026-10-15 (was `04-tcfs-s3-smoke-tunnel-deployment.yaml`; live object from `kubectl apply` on 2026-04-30) | live object still at 0; deletion is a separate ruling |

`helm list -n tcfs` shows no release state. The worker carries Helm labels
because it was rendered from the chart, but it was applied with kubectl.
The other four were created with `kubectl apply` from manifests that were
never committed, and their `last-applied-configuration` annotation still says
`replicas: 1`. `infra/k8s/honey/tcfs-paused/` now declares them. The manifests were captured on
2026-09-29 with a read-only `kubectl get -o yaml` and had these server-set
fields removed: `status`, `uid`, `resourceVersion`, `creationTimestamp`,
`managedFields`, `generation`, the kubectl last-applied annotation and the
`deployment.kubernetes.io/revision` annotation. The rest is the live spec,
including the image references (see below). A read-only `kubectl diff` of the
directory against honey showed no differences when they were captured. The
files were numbered in the old restore order.

Image references are copied exactly as the live specs give them, and none of
them is pinned to a digest: `chrislusf/seaweedfs:latest` (seaweedfs and the
posture gateway), `nats:2.10-alpine` and `cloudflare/cloudflared:latest`.
Pinning them would roll the pods, so it is a separate act (see PR #603 for
the SeaweedFS 4.47 digest).

The captured workloads reference no Secrets and no ConfigMaps. Services
`seaweedfs`, `seaweedfs-filer`, `nats` and `tcfs-s3-posture-gateway` are not
captured. They are live and untouched by the pause, so a restore does not
need them, and `seaweedfs` and `nats` carry Tailscale annotations that the
onprem migration plan treats as drift to remove, not to codify.

The `seaweedfs-openebs-candidate` and `nats-openebs-candidate` StatefulSets
in `infra/tofu/environments/onprem` are migration candidates. They are
disabled by default and are not the paused workloads, so this pause leaves
them alone.

## Kept

Nothing was deleted. Both PVCs stay bound and both PVs are `Retain`:

- `data-seaweedfs-0`: `local-path`, 10Gi, on honey
- `data-nats-0`: `local-path`, 5Gi, on honey

Services, Secrets, ConfigMaps, RBAC and the Tailscale annotations on
`tcfs/nats` and `tcfs/seaweedfs` are unchanged.

## Restore: superseded (2026-10-15)

The old `tcfs` stack on honey is never restored. Operator rulings on
TIN-4543 (OI-1003-Q153 and the P9 rollback ruling OI-1003-Q175) replace the
restore steps this record used to carry:

- **tcfs moves to the new SeaweedFS stack.** The old `seaweedfs` server is
  never scaled up again, not even briefly, and no client is pointed at it.
- **Rollback for the new stack's cutover** means re-pausing (hold the new S3
  gateway and revert the host cutover) or restoring from the new stack's own
  backups. It never means reviving the old server.
- **The old data stays preserved cold.** PVCs `data-seaweedfs-0` and
  `data-nats-0` and their `Retain` PVs are kept under the TIN-5268
  preservation hold until the migration closes. Reading them offline needs
  its own ruling.
- **The worker is not restored** with the cutover (OI-1003-Q177). Nothing
  publishes the queue it consumes, and its live image is stale. Restoring it
  later is its own act, with an explicit image tag.
- **NATS is a fresh deployment**, not a revival of `statefulset/nats`
  (OI-1003-Q177).
- **The posture gateway and the smoke tunnel are retired.** Their manifests
  were removed from `infra/k8s/honey/tcfs-paused/` on 2026-10-15
  (OI-1003-Q178): the tunnel was an unauthenticated public quick tunnel to
  the old S3, and the gateway ran with writes enabled. The two live
  Deployments are still at 0; deleting them, and the Tailscale annotations
  on `tcfs/nats` and `tcfs/seaweedfs`, is a separate attended ruling.

`01-seaweedfs-statefulset.yaml` and `02-nats-statefulset.yaml` stay in source
only to declare the paused live objects at `replicas: 0`. Do not apply them
with any other replica count.

## Test

```bash
just tcfs-backend-paused-test
```

The test renders the chart with its defaults and checks that the worker has
`replicas: 0` and the KEDA floor is 0. It checks that this record marks the
restore superseded and carries no revival commands, that the retired
gateway and tunnel manifests stay out of source, and that every manifest in
`infra/k8s/honey/tcfs-paused/` declares `replicas: 0` and contains no
server-set fields and no Secret.
