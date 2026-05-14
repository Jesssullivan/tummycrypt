# v0.12.12 Container Manifest Refresh

Date: 2026-05-14

Host:

- `neo.local`
- Darwin arm64
- Docker CLI `27.5.1`
- Podman client installed, but the configured Podman machine socket was not
  running during this refresh

Release:

- Tag: `v0.12.12`
- Image: `ghcr.io/jesssullivan/tcfsd:v0.12.12`
- Release URL: <https://github.com/Jesssullivan/tummycrypt/releases/tag/v0.12.12>

## Result

This is a registry-metadata refresh only. It does not replace the earlier amd64
runtime startup proof in
`docs/release/evidence/container-v01212-20260509T0145Z/`.

| Check | Result | Evidence | Notes |
| --- | --- | --- | --- |
| OCI index inspection | pass | `manifest.json` | The registry returned an OCI image index. |
| Native `linux/arm64/v8` manifest | fail | `manifest.json` | The index still contains no Linux arm64 image manifest. |
| Linux amd64 manifest | present | `manifest.json` | The index contains one `linux/amd64` image manifest. |
| Local container runtime pull/run | not run | `docker-version.log`, `podman-connections.log` | Docker points at the Podman socket and no daemon is running; Podman has a configured machine but its socket is not reachable. |

## Commands

```bash
docker manifest inspect ghcr.io/jesssullivan/tcfsd:v0.12.12 \
  > manifest.json
docker version > docker-version.log 2>&1
podman system connection list > podman-connections.log 2>&1
```

## Follow-Up

`v0.12.12` remains amd64-only for container runtime proof. Native
`linux/arm64/v8` registry and startup proof should be captured on the next tag
that publishes a multi-architecture image.
