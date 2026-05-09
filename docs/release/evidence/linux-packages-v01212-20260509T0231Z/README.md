# v0.12.12 Linux Package Distribution Proof

Date: 2026-05-09

Host:

- macOS host using Podman remote client
- Podman machine host: Fedora CoreOS 43, `linux/arm64`
- Podman client: see `podman-info.json`

Release:

- Tag: `v0.12.12`
- Baseline upgrade tag: `v0.12.2`
- Release URL: <https://github.com/Jesssullivan/tummycrypt/releases/tag/v0.12.12>

## Result

| Surface | Result | Evidence | Notes |
| --- | --- | --- | --- |
| Ubuntu 24.04 arm64 `.deb` fresh install | pass | `deb-ubuntu2404-arm64-fresh.log` | Installs `tcfsd` and `tcfs`, verifies release checksums, and passes `scripts/install-smoke.sh --expected-version 0.12.12`. |
| Ubuntu 24.04 arm64 `.deb` upgrade | pass | `deb-ubuntu2404-arm64-upgrade-0122-to-01212.log` | Installs and smokes `0.12.2`, upgrades both packages to `0.12.12`, then smokes again. |
| Ubuntu 24.04 amd64 `.deb` fresh install | pass | `deb-ubuntu2404-amd64-fresh.log` | Runs under Podman amd64 emulation on the arm64 host. |
| Ubuntu 24.04 amd64 `.deb` upgrade | pass | `deb-ubuntu2404-amd64-upgrade-0122-to-01212.log` | Runs under Podman amd64 emulation on the arm64 host. |
| Debian 13 trixie arm64 `.deb` fresh install | pass | `deb-debian13-arm64-fresh.log` | Confirms the documented Debian floor on the native arm64 lane. |
| Debian 13 trixie amd64 `.deb` fresh install | pass | `deb-debian13-amd64-fresh.log` | Confirms the documented Debian floor on the amd64 lane. |
| Fedora 42 x86_64 `.rpm` fresh install | pass | `rpm-fedora42-x86_64-fresh.log` | RPM is daemon-only today; smoke uses `--skip-cli`. |
| Fedora 42 x86_64 `.rpm` upgrade | pass | `rpm-fedora42-x86_64-upgrade-0122-to-01212.log` | Installs and smokes `0.12.2`, upgrades to `0.12.12`, then smokes again. |

## Scope

This packet proves Linux package install and upgrade behavior for the current
`v0.12.12` release on the documented support floor:

- Ubuntu 24.04 for both published `.deb` architectures
- Debian 13 trixie fresh install for both published `.deb` architectures
- Fedora 42 x86_64 for the daemon-only RPM

It does not prove Debian 12/bookworm. Debian 12 remains outside the current
`.deb` support floor unless a separate bookworm-targeted package is published.

It also does not prove systemd-managed service behavior. The package smoke uses
the repository's distribution helper, which starts `tcfsd` in an isolated temp
home, verifies the expected binary version, waits for the daemon socket, and
runs `tcfs status` when the package surface includes the CLI.

## Captured Files

- `SHA256SUMS.txt`: published checksum file for `v0.12.12`
- `podman-info.json`: Podman host/client metadata
- `pull-*.log`: image pulls needed for non-cached lanes
- `deb-*.log`: Debian package install/upgrade transcripts
- `rpm-*.log`: RPM install/upgrade transcripts
