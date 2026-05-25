# TCFS Production Storage Posture Gate

Date: 2026-05-19
Updated: 2026-05-25

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
- run `26209080328` on `main` at `6990ffb` passed against
  `tcfs-storage-prod-smoke` using `https://tcfs-smoke-s3.tinyland.dev`,
  `require_https=true`, `endpoint_tls=true`, `enforce_tls=true`, allowed-prefix
  listing, write/read/delete/delete-verify, and a denied-prefix
  `PermissionDenied` probe.
- the green `26209080328` packet proves the selected production-smoke identity
  can operate under `gha/storage-posture/...` and is denied under
  `gha/storage-posture-denied/...`. It does not prove broad restore throughput,
  long soak behavior, or beta-grade transient-error recovery.
- run `26212261838` refreshed the packet on `main` at `f89096a` after PR `#434`
  merged. It passed against the same public HTTPS endpoint with public CA trust
  (`ca_cert_path_configured=false`), allowed-prefix list/write/read/delete/delete
  verification, and denied-prefix `PermissionDenied` evidence under
  `gha/storage-posture-denied/...`. Treat this as the current-main storage
  posture packet for alpha QA, not as large-restore or soak proof.
- run `26220824445` refreshed the packet on `main` at `84c7389` after PRs `#437`
  and `#438` merged. It used
  `gha/storage-posture/current-main/20260521T103646Z`, public HTTPS endpoint
  `https://tcfs-smoke-s3.tinyland.dev`, `require_https=true`,
  `endpoint_tls=true`, `enforce_tls=true`, public CA trust
  (`ca_cert_path_configured=false`), 242-byte write/read/delete/delete-verify,
  allowed-prefix listing, and denied-prefix `PermissionDenied` under
  `gha/storage-posture-denied/current-main/20260521T103646Z`. Treat this as the
  current-main storage posture packet for alpha QA, not as large-restore,
  socket/highwater, transient-recovery, or soak proof.
- run `26246264661` refreshed the packet on merged `main` at `43ce227` after
  PR `#442` merged. It used the `tcfs-storage-prod-smoke` environment with
  public HTTPS, `require_https=true`, `endpoint_tls=true`, `enforce_tls=true`,
  public CA trust, allowed-prefix list/write/read/delete/delete-verify, and
  denied-prefix `PermissionDenied`. Treat this as the current alpha scoped
  HTTPS storage posture packet. It still does not prove large-restore
  throughput, socket/highwater behavior, long soak, or transient recovery
  classification.
- run `26376987029` attempted the large-restore companion on merged `main` at
  `592d119` with the same public HTTPS production-smoke environment. It failed
  before restore because the workflow defaulted to
  `gha/storage-posture-large/...`, outside the scoped credential policy for
  `gha/storage-posture/...`. The failure confirmed that out-of-scope writes and
  index listing return `PermissionDenied`/`AccessDenied`, but it did not test
  large-restore throughput, socket/highwater behavior, or transient recovery.
  The workflow must rerun under `gha/storage-posture/large/...` and fail
  immediately if the push evidence shows zero uploaded rows or storage access
  denials.
- run `26378068281` reran the large-restore companion on merged `main` at
  `e95fffe` under the corrected `gha/storage-posture/large/...` prefix. Scoped
  credentials passed and the push reached the synthetic 1 GiB Git pack, but the
  raw log recorded repeated transient Cloudflare `502` write-close failures
  before eventual upload completion. The workflow then failed in the push
  evidence gate because the storage-summary parser did not strip ANSI-colored
  tracing fields and therefore reported `upload_rows=0` even though `push.log`
  showed `uploaded: 30 files (1.0 GB)`. Treat this as useful transient-recovery
  blocker evidence, not a restore packet. The follow-up requirement is to strip
  ANSI before summarizing push logs, preserve warning/error counts, and allow a
  successful push with transient 5xx retry noise to continue into restore so the
  beta gate can classify convergence.
- downstream public-asset smokes on `main@e9b9f82` then used the same
  production-smoke prefix family successfully:
  - Linux `.deb` run `26218940925` used
    `gha/storage-posture/linux-postinstall/v0.12.13-rc4/20260521T095553Z`
    against `https://tcfs-smoke-s3.tinyland.dev` and proved package install,
    storage `[ok]`, FUSE mount, exact hydrate, `tcfs cache evict` + rehydrate,
    and mutation remote pull.
  - macOS `.pkg` run `26218940950` used
    `gha/storage-posture/macos-postinstall/v0.12.13-rc4/20260521T095553Z` and
    proved exact signed-host hydrate, evict/rehydrate, mutation, rename, and
    conflict/status through the public release package.
  These runs prove the production-smoke storage posture is usable by package
  first-use lanes; they still do not prove large-pack restore throughput,
  socket/highwater behavior, long soak, or transient recovery classification.

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

## Large-Restore Companion

The scoped canary proves endpoint posture and credential scope, but it is too
small to prove beta-grade restore behavior. Candidate packages that claim
large-object readiness should also run `task lazy:git-repo-restore-proof` against
a completed large canary packet and archive:

- dry-run and execute elapsed seconds
- shadow and restored regular-file byte totals
- restore throughput in bytes per second
- partial restored regular-file count and bytes if restore execution fails
- the usual hash, symlink, empty-directory, unsupported-special-file, state, and
  reconcile logs

Use this companion for the next `linux-xr-fast` candidate-package restore so
multi-GB Git pack recovery can be compared across release candidates without
claiming broad home-directory ownership.

Before running a large restore, require local disk headroom explicitly:

```bash
RESTORE_REQUIRE_HEADROOM=1 \
RESTORE_HEADROOM_MARGIN_BYTES=$((2 * 1024 * 1024 * 1024)) \
task lazy:git-repo-restore-proof
```

For a repeatable GitHub-hosted packet that uses the existing
`tcfs-storage-prod-smoke` environment secrets without exposing them locally,
dispatch the large-restore workflow after it has landed on `main`:

```bash
gh workflow run storage-large-restore-canary.yml \
  --ref main \
  -f runner_label=ubuntu-24.04 \
  -f smoke_environment=tcfs-storage-prod-smoke \
  -f pack_size_mib=1024 \
  -f restore_headroom_margin_mib=2048 \
  -f reconcile_timeout_secs=1800 \
  -f require_https=true
```

Use a larger `pack_size_mib` value, or a self-hosted runner, when the goal is
to reproduce the multi-GiB `linux-xr-fast` pack profile exactly. The hosted
workflow still records socket highwater, push object/chunk counts, restore
throughput, empty-dir parity, and bounded failure classification for the
selected size.

The guard compares free bytes on the restore root filesystem against the
archived shadow regular-file byte total plus the requested margin and writes a
blocked restore packet before any reconcile dry-run or execute step if the host
cannot hold the restore. This matters for `linux-xr-fast`: the current neo
workspace has only single-digit GiB free, below the honest restore size plus
overhead.

## Failure Classification

- Missing secrets: environment configuration blocker.
- Endpoint preflight failure: runner/backend reachability blocker.
- `require_https=true` with plaintext URL: posture blocker.
- Allowed-prefix list failure: credential-scope blocker; `tcfsd status` and
  index enumeration need the same permission.
- Write/read/delete timeout: storage reliability blocker.
- Delete verification failed: storage consistency blocker.
- Restore headroom preflight failed: host-capacity blocker, not a storage
  correctness failure.
- S3 auth/access denied: credential-scope blocker unless the policy was
  intentionally read-only.

Daemon health and readiness probes must keep these classes machine-readable.
`tcfs-storage` reports scoped health failures as `timeout`,
`permission_denied`, `not_found`, `rate_limited`, or `backend_error`,
including the probed path and elapsed time. Startup logs, `/readyz`, and
`tcfs status` should use the scoped prefix health probe rather than bucket-root
listing or a stale startup boolean.

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
