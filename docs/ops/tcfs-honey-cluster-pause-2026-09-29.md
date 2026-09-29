# TCFS honey cluster pause (2026-09-29, TIN-5136)

## Rulings

- "Cluster scale-to-0 only" (TIN-5136 comment): at 2026-09-29T16:45Z the five
  tcfs cluster workloads in namespace `tcfs` on the honey RKE2 cluster were
  scaled to 0 replicas imperatively.
- "Close the tcfs source drift" (operator ruling 2026-09-29): source must
  declare the paused state so a reconcile does not restart what was paused.

Host clients were deliberately left alone: `tcfsd` on honey, bumble and neo,
and the honey FUSE mount keep running.

## What was paused and where it is declared

| Workload | Kind | Source declaration | Paused in source |
|----------|------|--------------------|------------------|
| `tcfs-backend-tcfs-backend-worker` | Deployment | `infra/k8s/charts/tcfs-backend` (direct release `tcfs-backend`, via `scripts/tcfs-backend-deploy.sh`) | yes: `replicaCount: 0`, `autoscaling.minReplicas: 0` |
| `seaweedfs` | StatefulSet | none; live object created by `kubectl apply` on 2026-04-07 | not declared |
| `nats` | StatefulSet | none; live object created by `kubectl apply` on 2026-04-07 | not declared |
| `tcfs-s3-posture-gateway` | Deployment | none; live object created by `kubectl apply` on 2026-05-20 | not declared |
| `tcfs-s3-smoke-tunnel` | Deployment | none; live object created by `kubectl apply` on 2026-04-30 | not declared |

`helm list -n tcfs` shows no release state. The worker carries Helm labels
because it was rendered from the chart, but it was applied with kubectl.
The other four have no manifest in this repo. Their only record is their
`kubectl.kubernetes.io/last-applied-configuration` annotation, which still
says `replicas: 1`. A `kubectl apply` of the manifest they came from would
bring them back to 1 replica.

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

1. Scale `statefulset/seaweedfs` to 1:
   `$K scale statefulset/seaweedfs --replicas=1 && $K rollout status statefulset/seaweedfs`.
   Its image is still the floating `chrislusf/seaweedfs:latest`, so check that
   the pod comes back on the 4.47 digest
   (`chrislusf/seaweedfs:4.47@sha256:ce9e796f...`, pinned for the candidate
   in PR #603, TIN-5136) before going on.
2. Scale `statefulset/nats` to 1:
   `$K scale statefulset/nats --replicas=1 && $K rollout status statefulset/nats`.
3. Restore `deployment/tcfs-backend-tcfs-backend-worker` in source first:
   revert `replicaCount` and `autoscaling.minReplicas` to 1 in
   `infra/k8s/charts/tcfs-backend/values.yaml` (or pass
   `--set replicaCount=1 --set autoscaling.minReplicas=1`). Then run
   `$K scale deployment/tcfs-backend-tcfs-backend-worker --replicas=1`.
4. Scale `deployment/tcfs-s3-posture-gateway` and
   `deployment/tcfs-s3-smoke-tunnel` to 1, but only if the storage-posture
   canary and the hosted smoke still need them.

Then update this record and `scripts/test-tcfs-backend-paused-replicas.sh`
in the same change.

## Test

```bash
just tcfs-backend-paused-test
```

The test renders the chart with its defaults and checks that the worker has
`replicas: 0` and the KEDA floor is 0. It also checks that the restore values
render `1`, and that this record keeps the restore order.
