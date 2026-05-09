# v0.12.12 Container Distribution Proof

Date: 2026-05-09

Host:

- macOS host using Podman remote client
- Podman machine host: Fedora CoreOS 43, `linux/arm64`
- Podman client: see `podman-info.json`

Release:

- Tag: `v0.12.12`
- Image: `ghcr.io/jesssullivan/tcfsd:v0.12.12`
- Release URL: <https://github.com/Jesssullivan/tummycrypt/releases/tag/v0.12.12>

## Result

| Surface | Result | Evidence | Notes |
| --- | --- | --- | --- |
| Native `linux/arm64/v8` pull | fail | `pull-native-arm64.log` | Image index has no matching arm64 variant. |
| Explicit `linux/amd64` pull | pass | `pull-amd64.log` | Pulled under Podman amd64 emulation on an arm64 host. |
| `tcfsd --version` | pass | `version-amd64.log` | Reports `tcfsd 0.12.12`. |
| Worker startup smoke | partial pass | `worker-startup-amd64.log` | Worker starts, logs version/config, initializes metrics, then exits because the no-config smoke has no local NATS endpoint. |

## Commands

```bash
podman pull ghcr.io/jesssullivan/tcfsd:v0.12.12
podman pull --arch amd64 ghcr.io/jesssullivan/tcfsd:v0.12.12
podman run --rm --arch amd64 ghcr.io/jesssullivan/tcfsd:v0.12.12 --version
podman run --rm --arch amd64 --entrypoint /tcfsd \
  -e AWS_ACCESS_KEY_ID=dummy \
  -e AWS_SECRET_ACCESS_KEY=dummy \
  ghcr.io/jesssullivan/tcfsd:v0.12.12 \
  --mode=worker \
  --config /tmp/missing.toml \
  --log-format text
```

## Follow-Up

This evidence is enough to mark the current-tag amd64 image as present and
versioned, with worker startup reaching process initialization. It is not full
multi-arch container parity. The release matrix or publish workflow still needs
an arm64 image manifest before an arm64 host can pull the tag without `--arch
amd64`.
