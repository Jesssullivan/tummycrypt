# tcfs-backend Helm Chart

Helm chart for deploying the tcfs sync workers and metadata service to Kubernetes.

## Components

- **sync-worker**: Stateless NATS JetStream consumer pods (HPA-scaled via KEDA)
- **metadata-service**: Leader-elected coordination service (Kubernetes Lease API)

## Phase 4 Status

Chart implementation is scheduled for Phase 4. See `infra/tofu/modules/tcfs-backend/` for
OpenTofu module that will deploy this chart.

## Usage (future)

```bash
helm install tcfs-backend ./infra/k8s/charts/tcfs-backend \
  --namespace tcfs \
  --set image.tag=latest \
  --set nats.url=nats://nats.tcfs.svc:4222
```
