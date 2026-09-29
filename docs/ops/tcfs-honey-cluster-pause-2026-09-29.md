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
| `tcfs-s3-posture-gateway` | Deployment | `infra/k8s/honey/tcfs-paused/03-tcfs-s3-posture-gateway-deployment.yaml` (captured; live object from `kubectl apply` on 2026-05-20) | yes: `replicas: 0` |
| `tcfs-s3-smoke-tunnel` | Deployment | `infra/k8s/honey/tcfs-paused/04-tcfs-s3-smoke-tunnel-deployment.yaml` (captured; live object from `kubectl apply` on 2026-04-30) | yes: `replicas: 0` |

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
files are numbered in restore order.

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

## Restore

Restoring is a separate attended act and needs its own ruling on TIN-5136.
Bring the storage up first, then messaging, then the clients of both. Wait
for each step to be Ready before the next.

```bash
K="kubectl --context honey -n tcfs"
```

Restore through source. In each step, set `replicas: 1` in the manifest or
value in the same change, then apply that one file. Do not apply the whole
directory at once.

1. Restore `statefulset/seaweedfs`: set `replicas: 1` in
   `infra/k8s/honey/tcfs-paused/01-seaweedfs-statefulset.yaml`, then
   `$K apply -f infra/k8s/honey/tcfs-paused/01-seaweedfs-statefulset.yaml && $K rollout status statefulset/seaweedfs`.
   Its image is still the floating `chrislusf/seaweedfs:latest`, so check that
   the pod comes back on the 4.47 digest
   (`chrislusf/seaweedfs:4.47@sha256:ce9e796f...`, pinned for the candidate
   in PR #603, TIN-5136) before going on.
2. Restore `statefulset/nats`: set `replicas: 1` in
   `infra/k8s/honey/tcfs-paused/02-nats-statefulset.yaml`, then
   `$K apply -f infra/k8s/honey/tcfs-paused/02-nats-statefulset.yaml && $K rollout status statefulset/nats`.
3. Restore `deployment/tcfs-backend-tcfs-backend-worker` in source first:
   revert `replicaCount` and `autoscaling.minReplicas` to 1 in
   `infra/k8s/charts/tcfs-backend/values.yaml` (or pass
   `--set replicaCount=1 --set autoscaling.minReplicas=1`). Then run
   `$K scale deployment/tcfs-backend-tcfs-backend-worker --replicas=1`.
4. Restore `deployment/tcfs-s3-posture-gateway`
   (`infra/k8s/honey/tcfs-paused/03-tcfs-s3-posture-gateway-deployment.yaml`) and
   `deployment/tcfs-s3-smoke-tunnel`
   (`infra/k8s/honey/tcfs-paused/04-tcfs-s3-smoke-tunnel-deployment.yaml`) the same way, but only
   if the storage-posture canary and the hosted smoke still need them.

Then update this record and `scripts/test-tcfs-backend-paused-replicas.sh`
in the same change.

## Test

```bash
just tcfs-backend-paused-test
```

The test renders the chart with its defaults and checks that the worker has
`replicas: 0` and the KEDA floor is 0. It also checks that the restore values
render `1`, and that this record keeps the restore order. It also checks
that every manifest in `infra/k8s/honey/tcfs-paused/` declares `replicas: 0`, contains no server-set
fields and no Secret, and that the file numbering follows the restore order.
