# On-Prem Authority Recovery

Runbook for restoring or reality-checking the on-prem `tcfs` namespace when
`honey` (or another local cluster) is intended to be the active authority.

## Current Repo Truth

There are currently three adjacent deployment paths in this repo, and they are
not resource-name compatible with each other:

1. `infra/k8s/charts/tcfs-backend`
   - Direct Helm chart for the backend worker deployment and namespace
     scaffolding.
   - Default live release name: `tcfs-backend`
   - Expected resource names:
     - service account: `tcfs-backend-tcfs-backend`
     - deployment: `tcfs-backend-tcfs-backend-worker`
     - config map: `tcfs-backend-tcfs-backend-config`
   - If Helm is the authority, release state should exist in the namespace as
     `sh.helm.release.v1.tcfs-backend.vN` secrets.

2. `infra/k8s/charts/tcfs-stack`
   - Umbrella chart for blank-cluster bootstrap.
   - Useful when you want Helm to create the broader stack, not when you are
     reconciling an already-existing direct `tcfs-backend` release.

3. `infra/tofu/modules/tcfs-backend`
   - OpenTofu-managed backend worker deployment.
   - Uses different object names (`tcfsd`, `tcfs-sync-worker`) and should not
     be treated as an in-place reconciler for a Helm-managed
     `tcfs-backend-tcfs-backend-*` namespace.

The practical consequence is simple: if the live namespace already contains
Helm-managed `tcfs-backend-tcfs-backend-*` objects, the direct Helm chart is
the source of truth for restoring that path.

## Preflight

Confirm which cluster you are talking to before making changes:

```bash
kubectl config current-context
kubectl get ns tcfs
helm list -n tcfs
kubectl get sa,deploy,secret -n tcfs | \
  rg 'tcfs-backend|sh.helm.release'
```

Expected direct-chart signs:

- Helm release `tcfs-backend` exists in namespace `tcfs`
- service account `tcfs-backend-tcfs-backend` is present
- deployment `tcfs-backend-tcfs-backend-worker` exists
- Helm release secrets `sh.helm.release.v1.tcfs-backend.vN` exist

If instead the namespace contains `tcfsd` or `tcfs-sync-worker`, you are on
the OpenTofu-managed path and should not force the Helm recovery flow onto it.

## Recover the Namespace Scaffolding

Use the direct backend chart reconciler from this repo:

```bash
bash scripts/tcfs-backend-deploy.sh
```

Defaults:

- release: `tcfs-backend`
- namespace: `tcfs`
- chart: `infra/k8s/charts/tcfs-backend`

This path is intentionally narrow: it restores the backend worker deployment,
service account, role binding, config map, and Helm release state for the
existing direct chart authority. It does not recreate SeaweedFS or NATS.

Useful flags:

```bash
bash scripts/tcfs-backend-deploy.sh --dry-run
TCFS_NAMESPACE=tcfs bash scripts/tcfs-backend-deploy.sh \
  --set image.tag=v0.12.2
```

## Validate Recovery

```bash
helm list -n tcfs
kubectl get sa tcfs-backend-tcfs-backend -n tcfs
kubectl rollout status deployment/tcfs-backend-tcfs-backend-worker -n tcfs
kubectl logs deployment/tcfs-backend-tcfs-backend-worker -n tcfs --tail=50
```

If the service account is missing but the deployment still references
`tcfs-backend-tcfs-backend`, re-running the direct Helm release should recreate
it because `serviceAccount.create` defaults to `true` in the chart values.

## Civo Keep or Retire Signal

Do not retire the preserved Civo PVC tail until the on-prem namespace has all
of the following:

- an explicit deployment authority (`tcfs-backend` Helm release or an explicit
  alternative)
- live Helm release state in the namespace if Helm owns it
- restored namespace scaffolding for the backend worker path
- an operator decision recorded on whether Civo remains standby state or can be
  retired

Once those conditions are true, the residual Civo `tcfs` PVCs stop being a
guess-driven safety blanket and can be evaluated deliberately.
