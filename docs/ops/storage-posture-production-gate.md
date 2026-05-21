# TCFS Production Storage Posture Gate

Date: 2026-05-19

This is the operator handoff for `TIN-1546`. It defines the minimum storage
packet required before TCFS can claim production-like S3 readiness for alpha QA.

The gate is intentionally narrower than "storage works." A passing run proves
allowed-prefix listing, one scoped write/read/delete/delete-verify path,
endpoint TLS posture, and the credential scope used for the run. It does not
prove broad directory ownership, multitenancy, lost-device recovery, or long
soak behavior.

For the production posture packet, the canary should also include a negative
scope probe by setting `scope_deny_prefix` to a prefix outside the credential
policy. The workflow passes that to `tcfs storage canary
--expect-deny-prefix`; the run fails unless the write is rejected with
`PermissionDenied`.

## Current State

`main` has a dispatchable workflow:

- `.github/workflows/storage-posture-canary.yml`
- command under test: `tcfs storage canary --json`
- evidence artifact: `storage-posture-canary-<run_id>-<attempt>`

Known evidence:

- run `26118552975` proved the workflow and artifact shape against the private
  PZM smoke backend.
- that run used `http://seaweedfs-tcfs:8333` with `require_https=false`.
- it is not a production posture packet because the endpoint was plaintext and
  private, and the credentials were the existing smoke credentials.

## Required GitHub Environment

Create or update a GitHub environment for the production-like storage packet,
for example `tcfs-storage-prod-smoke`.

Required secrets:

- `TCFS_SMOKE_S3_ENDPOINT`
- `TCFS_SMOKE_S3_BUCKET`
- `TCFS_SMOKE_S3_ACCESS_KEY_ID`
- `TCFS_SMOKE_S3_SECRET_ACCESS_KEY`

Optional secret:

- `TCFS_SMOKE_S3_REGION`
- `TCFS_SMOKE_S3_CA_CERT_PEM` — PEM-encoded custom root CA for private HTTPS
  endpoints that are not signed by a public trust root

The endpoint must be reachable from the selected runner. For GitHub-hosted
Linux, it must be public HTTPS. For a private self-hosted runner, it may be
private, but `require_https=true` still requires the endpoint URL to start with
`https://`. If the endpoint uses a private CA, set `TCFS_SMOKE_S3_CA_CERT_PEM`;
the workflow writes it to a run-scoped file and passes it as
`storage.ca_cert_path`.

## Credential Bar

Do not use the bucket-wide SeaweedFS admin identity for this gate.

The canary identity should be scoped to:

- the selected bucket, and
- one run-specific prefix such as
  `gha/storage-posture/<run_id>-<attempt>` or a stable parent prefix such as
  `gha/storage-posture/`.

The credential must be able to:

- list the allowed parent prefix,
- write the canary object,
- read the same object,
- delete the same object, and
- verify the object no longer exists.

It should not be able to list or mutate unrelated tenant, fleet, user, or
release prefixes. If the backing S3 provider cannot express that exact policy,
record the closest available policy in the evidence packet and keep `TIN-1546`
open.

To make that scope claim machine-checkable, choose a denial prefix outside the
allowed policy, for example:

- allowed parent prefix: `gha/storage-posture/`
- canary write prefix: `gha/storage-posture/<run_id>-<attempt>`
- denial prefix: `gha/storage-posture-denied/<run_id>-<attempt>`

Downstream package smokes that reuse this production-like environment must also
use a prefix under the allowed parent. For example, dispatch Linux package
smoke with `remote_prefix=gha/storage-posture/linux-postinstall/<tag>/<run>`
instead of the workflow's private-smoke default `gha/linux-postinstall/...`.

## Dispatch

Before dispatching, classify the full alpha gate state:

```bash
scripts/tcfs-alpha-gate-preflight.sh
```

Preferred helper:

```bash
scripts/storage-posture-canary-dispatch.sh \
  --environment tcfs-storage-prod-smoke \
  --runner-label ubuntu-24.04
```

The helper checks that the GitHub environment exposes the required secret
names before dispatch. It also requires a non-empty denial prefix by default,
because production closure needs a machine-checkable scoped-credential denial
probe.

Hosted Linux runner against a public HTTPS endpoint:

```bash
gh workflow run storage-posture-canary.yml \
  --ref main \
  -f runner_label=ubuntu-24.04 \
  -f smoke_environment=tcfs-storage-prod-smoke \
  -f require_https=true \
  -f scope_deny_prefix=gha/storage-posture-denied/$(date -u +%Y%m%dT%H%M%SZ) \
  -f timeout_secs=10
```

Private runner against an HTTPS endpoint:

```bash
gh workflow run storage-posture-canary.yml \
  --ref main \
  -f runner_label=<private-runner-label> \
  -f smoke_environment=tcfs-storage-prod-smoke \
  -f require_https=true \
  -f scope_deny_prefix=gha/storage-posture-denied/$(date -u +%Y%m%dT%H%M%SZ) \
  -f timeout_secs=10
```

Only use `require_https=false` for workflow validation against a private
plaintext lab backend. That mode cannot close `TIN-1546`.

## Pass Criteria

The workflow run must complete successfully and upload its evidence artifact.

`storage-canary.json` must show:

- `listed: true`
- non-empty `list_prefix`
- integer `list_count`
- `deleted: true`
- `bytes > 0`
- `endpoint_tls: true`
- `enforce_tls: true`
- if `scope_deny_prefix` was set: `scope_deny.denied: true` and
  `scope_deny.error_kind: PermissionDenied`
- non-empty `endpoint`, `bucket`, `prefix`, `key`, and operation timings

`storage-posture.env` must record:

- runner label
- GitHub environment
- endpoint
- bucket
- remote prefix
- scope-deny prefix, if configured
- `require_https=true`
- `enforce_tls=true`
- `ca_cert_path_supported=true`
- whether a custom CA was configured for the run

The Linear/GitHub closeout comment should also include the credential scope in
plain language. Do not paste secrets.

## Failure Classification

- Missing secrets: environment configuration blocker.
- Endpoint preflight failure: runner/backend reachability blocker.
- `require_https=true` with plaintext URL: posture blocker.
- Allowed-prefix list failure: credential-scope blocker; `tcfsd status` and
  index enumeration need the same permission.
- Write/read/delete timeout: storage reliability blocker.
- Delete verification failed: storage consistency blocker.
- S3 auth/access denied: credential-scope blocker unless the policy was
  intentionally read-only.

If the failure is endpoint posture or credential scope, do not rerun the package
or FileProvider smokes. Fix the storage gate first so downstream failures are
not misclassified as product regressions.

## Relationship To Alpha/Beta Claims

This packet is required for alpha productionization QA, but it is not sufficient
for beta. Beta still requires large-object restore evidence, bounded transient
error classification, scheduled fleet acceptance, safe enrollment, and visible
recovery UX.

Related trackers:

- `TIN-1546`: production S3/storage posture gate
- `TIN-1540`: reachable Linux smoke backend or private Linux runner
- `TIN-1422`: Linux postinstall smoke parity
- `TIN-132`: neo-honey live fleet acceptance
- `#280`: distribution install/upgrade umbrella
