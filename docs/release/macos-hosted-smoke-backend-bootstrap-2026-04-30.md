# macOS Hosted Smoke Backend Bootstrap - 2026-04-30

This records the temporary public backend created for the GitHub-hosted macOS
FileProvider smoke lane. It proves storage reachability and E2EE fixture
behavior, not a clean-host Finder pass.

## Scope

The bootstrap created the `tcfs-macos-smoke` GitHub environment and populated
the five secrets consumed by
`.github/workflows/macos-postinstall-smoke.yml`:

- `TCFS_SMOKE_S3_ENDPOINT`
- `TCFS_SMOKE_S3_BUCKET`
- `TCFS_SMOKE_S3_ACCESS_KEY_ID`
- `TCFS_SMOKE_S3_SECRET_ACCESS_KEY`
- `TCFS_SMOKE_MASTER_KEY_B64`

The S3 access credentials came from the existing `tcfs/seaweedfs-admin`
Kubernetes secret on the `honey` cluster. The E2EE master key is a fresh
32-byte per-environment smoke key stored only as a GitHub environment secret.

## Backend

- Kubernetes context: `honey`
- Namespace: `tcfs`
- Service: `seaweedfs`
- Bucket: `tcfs`
- Temporary tunnel deployment: `tcfs-s3-smoke-tunnel`
- Public endpoint:
  `https://follows-sega-generated-formerly.trycloudflare.com`

The tunnel is a Cloudflare quick tunnel. It is acceptable for unblocking a
hosted smoke run, but it is not a durable production endpoint. If the pod is
restarted or replaced, the URL may change and
`TCFS_SMOKE_S3_ENDPOINT` must be refreshed.

The cluster also already has a durable named Cloudflare Tunnel deployment in
the `cloudflared` namespace:

- deployment: `cloudflared`
- replicas: `2`
- tunnel ID: `da3ffda2-68ee-46d1-aa55-ec8dae2bd471`
- currently observed remote ingress hostnames:
  `search.hewow.gay`, `search.tinyland.dev`, `nix-cache.tinyland.dev`, and
  `bazel-cache.tinyland.dev`

That durable tunnel is the right long-term place for a stable TCFS smoke S3
hostname, for example `tcfs-smoke-s3.tinyland.dev`, pointing at
`http://seaweedfs.tcfs.svc.cluster.local:8333`. The current deployment is using
a connector token and remotely managed ingress config, so adding that hostname
requires a Cloudflare dashboard/API config change rather than a Kubernetes
Ingress object edit.

GitHub-hosted runners cannot reach the tailnet-only SeaweedFS endpoint
directly.

## Validation

Unauthenticated root access returned an S3 XML `403`, confirming the HTTPS
surface reaches SeaweedFS.

Authenticated S3 list through the public tunnel succeeded and showed the
existing `tcfs` bucket.

A local TCFS E2EE probe then passed against the same endpoint:

- remote prefix: `gha/bootstrap-check/20260430T203201Z`
- fixture path: `ci-smoke/bootstrap/postinstall.txt`
- keyed `tcfs push`: pass
- no-crypto `tcfs pull`: failed as expected
- keyed `tcfs pull`: pass
- pulled content matched expected content

## Remaining Bar

This bootstrap only removes the hosted storage-reachability blocker. The
workflow changes that consume these environment secrets still need to be
committed and pushed before a GitHub-hosted run can exercise them. The product
bar remains:

1. clean-host or GitHub-hosted `.pkg` install from a signed release artifact,
2. shared-Keychain FileProvider config proof,
3. Finder/CloudStorage enumeration,
4. exact-content hydrate/open proof, and
5. one unsync/dehydrate edge case.
