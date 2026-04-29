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

## Current Honey Readback

Live readback from the Tinyland `honey` cluster on 2026-04-28 changed the
operator call from "repair a broken worker" to "decide authority before moving
placement":

- `nats-0`, `seaweedfs-0`, and `tcfs-backend-tcfs-backend-worker` are Running.
- NATS health returns OK, SeaweedFS reports a leader, and the worker logs show
  a live NATS connection.
- `helm list -n tcfs` reports no release state.
- `tcfs/nats` and `tcfs/seaweedfs` have live Tailscale exposure annotations,
  but their last-applied Service configuration had empty annotations.
- `data-nats-0` and `data-seaweedfs-0` are `local-path` PVCs whose backing PVs
  have node affinity to `honey`.

That means a ProxyClass-only patch would be cosmetic. It could move the
generated Tailscale proxy pods, but it would not source-own the exposure, create
release state, or make NATS/SeaweedFS data drain-mobile.

## Current Decision

Treat the direct `tcfs-backend` Helm chart as a recovery authority for the
backend worker objects only. Do not use it as evidence that the whole live
namespace is Helm-owned.

The durable on-prem path is OpenTofu migration, not blind Helm adoption. The
current source-owned on-prem environment records retained target PVCs,
non-canonical candidate workloads, candidate tailnet Services, and render-only
cutover/rollback commands. Do not switch canonical tailnet hostnames or move
data outside an approved downtime window.

Reasons:

- The backend worker objects are Helm-shaped, but NATS and SeaweedFS are not.
- The OpenTofu modules already model NATS, SeaweedFS, backend workers, and
  tailnet exposure as separate source-owned concerns.
- The live backing state is honey-local `local-path`, so honey/sting mobility
  requires storage/data planning either way.
- A migration plan can preserve the current healthy singleton while building
  new source-owned objects with explicit storage class, Tailscale exposure, and
  smoke gates.

Minimum migration gates:

1. capture live NATS JetStream and SeaweedFS data inventory;
2. use the retained target storage classes for NATS and SeaweedFS instead of
   inheriting `local-path`;
3. render or apply retained target PVCs and non-canonical candidate workloads
   only through `infra/tofu/environments/onprem`;
4. smoke candidate Tailscale Services with selectors that do not point at the
   live `app=nats` or `app=seaweedfs` pods;
5. run a dry-run/plan that does not collide with live object names;
6. cut over only after smoke tests prove NATS, SeaweedFS, and worker
   connectivity on the new path;
7. remove the old live annotations and retained objects through the same
   source-controlled transition.

Use the read-only preflight before changing either path:

```bash
bash scripts/tcfs-onprem-preflight.sh
```

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

### RBAC-Only Recovery For Missing Helm Release State

Live recovery note, 2026-04-27: the on-prem namespace can contain
Helm-shaped `tcfs-backend-tcfs-backend-*` objects without Helm release
secrets. In that state a full `helm upgrade --install` cannot immediately adopt
the existing ConfigMap / Deployment, and it can also fail before adoption if
optional CRDs such as KEDA `ScaledObject` or Prometheus `ServiceMonitor` are not
installed.

If the Deployment exists but pod creation is blocked because the service
account is missing, restore only the chart-owned RBAC scaffold first:

```bash
bash scripts/tcfs-backend-deploy.sh --rbac-only --dry-run
bash scripts/tcfs-backend-deploy.sh --rbac-only
kubectl rollout restart deployment/tcfs-backend-tcfs-backend-worker -n tcfs
kubectl rollout status deployment/tcfs-backend-tcfs-backend-worker -n tcfs
```

This is a repair path, not a complete Helm adoption. After the worker is
healthy, follow the downtime-gated OpenTofu migration path for NATS,
SeaweedFS, and canonical tailnet ownership.

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
