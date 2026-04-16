# tcfs-backend Helm Chart

Helm chart for deploying the tcfs sync workers and metadata service to Kubernetes.

## Components

- **sync-worker**: Stateless NATS JetStream consumer pods (HPA-scaled via KEDA)
- **metadata-service**: Leader-elected coordination service (Kubernetes Lease API)

## Current Status

This chart is the direct Helm authority for the backend-worker namespace
scaffolding when the live cluster already contains Helm-managed
`tcfs-backend-tcfs-backend-*` objects.

Do not assume it is interchangeable with `infra/tofu/modules/tcfs-backend`.
The OpenTofu module uses different object names (`tcfsd`, `tcfs-sync-worker`)
and should be treated as a separate deployment path.

## Expected Objects

With the default release name `tcfs-backend`, this chart creates:

- service account: `tcfs-backend-tcfs-backend`
- deployment: `tcfs-backend-tcfs-backend-worker`
- config map: `tcfs-backend-tcfs-backend-config`
- Helm release secrets: `sh.helm.release.v1.tcfs-backend.vN`

## Usage

```bash
bash scripts/tcfs-backend-deploy.sh
```

Or directly:

```bash
helm upgrade --install tcfs-backend ./infra/k8s/charts/tcfs-backend \
  --namespace tcfs \
  --create-namespace \
  --set image.tag=latest \
  --set config.natsUrl=nats://nats.tcfs.svc.cluster.local:4222
```

After reconcile:

```bash
helm list -n tcfs
kubectl get sa tcfs-backend-tcfs-backend -n tcfs
kubectl rollout status deployment/tcfs-backend-tcfs-backend-worker -n tcfs
```
