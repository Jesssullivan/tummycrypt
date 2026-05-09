# tcfs-backend Helm Chart

Helm chart for deploying tcfs sync workers to Kubernetes.

## Components

- **sync-worker**: Stateless NATS JetStream consumer pods (HPA-scaled via KEDA)
- **coordination RBAC**: Kubernetes Lease permissions for worker coordination

There is no separate metadata-service Deployment in this chart today. Treat any
older metadata-service references as planned or historical until an actual
Deployment exists.

## Current Status

This chart is the direct Helm authority for the backend-worker namespace
scaffolding when the live cluster already contains Helm-managed
`tcfs-backend-tcfs-backend-*` objects.

Do not assume it is interchangeable with `infra/tofu/modules/tcfs-backend`.
The OpenTofu module uses different object names (`tcfsd`, `tcfs-sync-worker`)
and should be treated as a separate deployment path.

The chart defaults to the mutable `latest` container tag for operator
convenience. Use an explicit release tag, such as `v0.12.12`, for production
reconcile or evidence runs.

## Expected Objects

With the default release name `tcfs-backend`, this chart creates:

- service account: `tcfs-backend-tcfs-backend`
- deployment: `tcfs-backend-tcfs-backend-worker`
- config map: `tcfs-backend-tcfs-backend-config`
- Helm release secrets: `sh.helm.release.v1.tcfs-backend.vN`

## Usage

This direct chart can render KEDA `ScaledObject` and Prometheus
`ServiceMonitor` resources. Before using the defaults, confirm those CRDs are
installed. For a plain backend-worker reconcile without those CRDs, disable the
optional resources explicitly:

```bash
--set autoscaling.enabled=false \
--set metrics.serviceMonitor.enabled=false
```

```bash
bash scripts/tcfs-backend-deploy.sh
```

Or directly:

```bash
helm upgrade --install tcfs-backend ./infra/k8s/charts/tcfs-backend \
  --namespace tcfs \
  --create-namespace \
  --set image.tag=v0.12.12 \
  --set autoscaling.enabled=false \
  --set metrics.serviceMonitor.enabled=false \
  --set config.natsUrl=nats://nats.tcfs.svc.cluster.local:4222
```

After reconcile:

```bash
helm list -n tcfs
kubectl get sa tcfs-backend-tcfs-backend -n tcfs
kubectl rollout status deployment/tcfs-backend-tcfs-backend-worker -n tcfs
```

If live Helm release state is missing but an existing worker Deployment still
references `tcfs-backend-tcfs-backend`, use the repo script's RBAC-only repair
path before attempting a full adoption:

```bash
bash scripts/tcfs-backend-deploy.sh --rbac-only --dry-run
bash scripts/tcfs-backend-deploy.sh --rbac-only
kubectl rollout restart deployment/tcfs-backend-tcfs-backend-worker -n tcfs
```
