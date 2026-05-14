# Distribution Smoke Matrix

Canonical post-release proof for packaged distribution surfaces.

As of 2026-04-16, release quality for `tcfs` means more than "artifacts exist."
A tagged release is not considered fully proven until the shipped install
surfaces below have passed their required smoke checks.

## Gate Decision

- **Fresh install proof is required on every shipped release surface for every `v0.12.x+` tag**:
  - Homebrew
  - macOS `.pkg`
  - Ubuntu 24.04+ / Debian 13+ `.deb`
  - Fedora/RHEL `.rpm`
  - container image
  - Nix package path
- **Upgrade proof is required on the primary mutable installer surfaces**:
  - Homebrew
  - macOS `.pkg`
  - Ubuntu 24.04+ / Debian 13+ `.deb`
- **RPM, container, and Nix upgrade proof is sampled rather than mandatory on every tag**:
  - RPM is daemon-only today, so its proof surface is narrower than Homebrew, `.pkg`, or `.deb`
  - container upgrades are normally orchestrator rollouts rather than a host-local installer flow
  - Nix installs are immutable and ref-pinned, so the per-tag fresh install gate is the most meaningful baseline

Current evidence note for `v0.12.12`: the archived current-tag packet in this
repo proves Homebrew fresh/upgrade, Darwin Nix profile install, Ubuntu 24.04
`.deb` fresh/upgrade on arm64 and amd64, Debian 13 `.deb` fresh install on
arm64 and amd64, and Fedora 42 x86_64 daemon-only RPM fresh/upgrade. Container
evidence proves explicit amd64 pull/version/startup logs but records a missing
native `linux/arm64/v8` image manifest. The
`container-v01212-manifest-refresh-20260514T224746Z/` registry metadata refresh
reconfirmed that gap: the `v0.12.12` image index still exposes Linux amd64 plus
an unknown/unknown manifest, with no Linux arm64 image. The release workflow is
configured to publish both architectures on the next cut, but that still needs
tagged registry proof before upgrading the current evidence row. Production
macOS `.pkg` current-tag proof remains a separate follow-up check even though
older release and CI evidence exists for parts of that surface.

## Out-Of-Scope Published Helpers

The release workflow also publishes helper installers:

- `install.sh` for Linux/macOS tarball convenience installs
- `install.ps1` for Windows, currently an explicit unsupported-release stub
  while Windows artifacts are disabled

These are **not** canonical release-proof surfaces today.

- `install.sh` remains convenience tooling until a dedicated smoke lane is added
- `install.ps1` must not claim install success until the release workflow
  publishes a matching Windows zip; today it fails with an unsupported-release
  message because Windows is not yet a truthful release-grade user surface

## Shared Installed-Binary Smoke

For any release surface that places `tcfsd` on `PATH`, run:

```bash
bash scripts/install-smoke.sh --expected-version "${VERSION}"
```

If the surface is daemon-only and does not ship `tcfs` today, run:

```bash
bash scripts/install-smoke.sh --expected-version "${VERSION}" --skip-cli
```

What this helper proves:

- the installed binaries are executable
- `tcfsd` can start with an isolated default config
- the daemon creates its Unix socket
- `tcfs status` works when the CLI is present on that surface

What it does **not** prove:

- live SeaweedFS or NATS connectivity
- Finder or FileProvider behavior
- service-manager integration via systemd or launchd
- truthful first use against reachable storage
- Kubernetes rollout semantics

Those remain surface-specific follow-on checks. For the bridge from
installed-binary smoke to the first truthful user action, use
[Packaged Install To First-Real-Use Acceptance](packaged-install-first-use.md).

## Surface Matrix

| Surface | Fresh install gate every tag | Upgrade gate every tag | Required smoke | Notes |
|---------|------------------------------|------------------------|----------------|-------|
| Homebrew | Yes | Yes | `scripts/install-smoke.sh` | Current manual `homebrew-tap` checkout flow |
| macOS `.pkg` | Yes | Yes | `scripts/install-smoke.sh` | Apple Silicon package path; desktop UX still experimental |
| Ubuntu 24.04+ / Debian 13+ `.deb` | Yes | Yes | `scripts/install-smoke.sh` | Install both `tcfsd` and `tcfs` packages |
| Fedora/RHEL `.rpm` | Yes | Sampled | `scripts/install-smoke.sh --skip-cli` | Fedora 42 x86_64 is currently proven; RHEL/Rocky remain target surfaces pending smoke |
| Container image | Yes | Sampled | worker-image startup check | prove amd64 and arm64 pulls + entrypoint/startup, not CLI status or cluster rollout |
| Nix | Yes | Sampled | `scripts/install-smoke.sh` | Current `v0.12.12` proof is Darwin profile install; Linux/NixOS host proof is separate |

## Surface Procedures

Set the release variables first:

```bash
export TAG=v0.12.12
export VERSION="${TAG#v}"
```

### Homebrew

Fresh install:

```bash
brew untap Jesssullivan/tummycrypt 2>/dev/null || true
brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
brew install Jesssullivan/tummycrypt/tcfs
bash scripts/install-smoke.sh --expected-version "${VERSION}"
```

Upgrade:

```bash
git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
brew upgrade Jesssullivan/tummycrypt/tcfs
bash scripts/install-smoke.sh --expected-version "${VERSION}"
```

### macOS `.pkg`

Fresh install:

```bash
curl -LO "https://github.com/Jesssullivan/tummycrypt/releases/download/${TAG}/tcfs-${VERSION}-macos-aarch64.pkg"
sudo installer -pkg "tcfs-${VERSION}-macos-aarch64.pkg" -target /
bash scripts/install-smoke.sh --expected-version "${VERSION}"
```

If this tag also needs packaged-install to first-real-use proof on macOS, follow
the package smoke with the named FileProvider harness:

```bash
bash scripts/macos-postinstall-smoke.sh \
  --expected-version "${VERSION}" \
  --config "$HOME/.config/tcfs/config.toml" \
  --expected-file "path/to/known/remote-backed-file"
```

Upgrade:

```bash
sudo installer -pkg "tcfs-${VERSION}-macos-aarch64.pkg" -target /
bash scripts/install-smoke.sh --expected-version "${VERSION}"
```

### Ubuntu 24.04+ / Debian 13+ `.deb`

The current `.deb` support floor is Ubuntu 24.04+ and Debian 13 `trixie`+.
Do not count Debian 12 `bookworm` as a passing `.deb` surface unless the release
adds a separate bookworm-targeted package variant. The observed `v0.12.2`
failure matches Debian's package floor: bookworm ships `libc6 2.36`, while
trixie ships `libc6 2.41` and `libssl3t64`. References:

- <https://packages.debian.org/bookworm/libc6>
- <https://packages.debian.org/trixie/libc6>
- <https://packages.debian.org/trixie/libssl3t64>

Fresh install:

```bash
curl -LO "https://github.com/Jesssullivan/tummycrypt/releases/download/${TAG}/tcfsd-${VERSION}-amd64.deb"
curl -LO "https://github.com/Jesssullivan/tummycrypt/releases/download/${TAG}/tcfs-${VERSION}-amd64.deb"
sudo dpkg -i "tcfsd-${VERSION}-amd64.deb" "tcfs-${VERSION}-amd64.deb"
bash scripts/install-smoke.sh --expected-version "${VERSION}"
```

Upgrade:

```bash
sudo dpkg -i "tcfsd-${VERSION}-amd64.deb" "tcfs-${VERSION}-amd64.deb"
bash scripts/install-smoke.sh --expected-version "${VERSION}"
```

### Fedora `.rpm`

The current `v0.12.12` proof covers Fedora 42 x86_64 daemon-only. RHEL/Rocky
remain target surfaces pending smoke.

Fresh install:

```bash
curl -LO "https://github.com/Jesssullivan/tummycrypt/releases/download/${TAG}/tcfsd-${VERSION}-x86_64.rpm"
sudo rpm -i "tcfsd-${VERSION}-x86_64.rpm"
bash scripts/install-smoke.sh --expected-version "${VERSION}" --skip-cli
```

Upgrade is sampled unless the RPM packaging path changes materially:

```bash
sudo rpm -Uvh "tcfsd-${VERSION}-x86_64.rpm"
bash scripts/install-smoke.sh --expected-version "${VERSION}" --skip-cli
```

### Container Image

Fresh install proof for the worker image is both architecture presence and a
minimal startup check:

For the current `v0.12.12` evidence packet, run and archive the amd64 lane:

```bash
podman pull --arch amd64 "ghcr.io/jesssullivan/tcfsd:${TAG}"
podman run --rm --arch amd64 \
  "ghcr.io/jesssullivan/tcfsd:${TAG}" --version
podman run --rm --entrypoint /tcfsd \
  --arch amd64 \
  -e AWS_ACCESS_KEY_ID=dummy \
  -e AWS_SECRET_ACCESS_KEY=dummy \
  "ghcr.io/jesssullivan/tcfsd:${TAG}" \
  --mode=worker \
  --config /tmp/missing.toml \
  --log-format text
```

Native `linux/arm64/v8` should be attempted and recorded, but it remains an open
current-tag gap for `v0.12.12` until a new multi-arch image is published.

For the next release tag that is expected to publish both manifests, require the
full architecture loop:

```bash
for ARCH in amd64 arm64; do
  podman pull --arch "${ARCH}" "ghcr.io/jesssullivan/tcfsd:${TAG}"
  podman run --rm --arch "${ARCH}" \
    "ghcr.io/jesssullivan/tcfsd:${TAG}" --version
  podman run --rm --entrypoint /tcfsd \
    --arch "${ARCH}" \
    -e AWS_ACCESS_KEY_ID=dummy \
    -e AWS_SECRET_ACCESS_KEY=dummy \
    "ghcr.io/jesssullivan/tcfsd:${TAG}" \
    --mode=worker \
    --config /tmp/missing.toml \
    --log-format text
done
```

Success criteria:

- the image pulls successfully on the architecture under test
- `tcfsd --version` reports the expected release version
- the worker binary starts cleanly enough to emit version/config, worker-mode,
  and metrics initialization logs before the no-config smoke exits on the
  expected missing local NATS endpoint

Full cluster rollout proof remains an infra-level check and should be paired with
the live fleet runbooks when container packaging or worker behavior changes.
The current `v0.12.12` evidence proves amd64 only. The 2026-05-14 manifest
refresh still shows no native `linux/arm64/v8` image manifest, so arm64 pulls
are not yet published for this tag.

### Nix

Fresh install:

```bash
nix profile install \
  "github:Jesssullivan/tummycrypt?ref=${TAG}#tcfsd" \
  "github:Jesssullivan/tummycrypt?ref=${TAG}#tcfs-cli"
bash scripts/install-smoke.sh --expected-version "${VERSION}"
```

Record the host platform in evidence. The current `v0.12.12` packet proves a
Darwin temporary profile install; Linux Nix profile and NixOS module host proof
remain separate acceptance lanes.

Upgrade is sampled unless the flake packaging path or install story changes
materially between releases.

## Evidence Capture

Record the outcome for each tag in the release issue, PR, or a maintainer comment
using a table like this:

| Surface | Fresh install | Upgrade | Smoke output captured | Notes |
|---------|---------------|---------|-----------------------|-------|
| Homebrew | pass/fail | pass/fail | yes/no | |
| macOS `.pkg` | pass/fail | pass/fail | yes/no | |
| Ubuntu 24.04+ / Debian 13+ `.deb` | pass/fail | pass/fail | yes/no | Debian 12 excluded unless a bookworm package exists |
| Fedora/RHEL `.rpm` | pass/fail | sampled/n-a | yes/no | |
| Container image | pass/fail | sampled/n-a | yes/no | |
| Nix | pass/fail | sampled/n-a | yes/no | |

## Relationship To Other Acceptance Lanes

- Use this matrix for **distribution surface proof**
- Use [Packaged Install To First-Real-Use Acceptance](packaged-install-first-use.md)
  for the **install-to-first-action bar** after artifact smoke passes
- Use [`scripts/macos-postinstall-smoke.sh`](../../scripts/macos-postinstall-smoke.sh)
  for the current macOS package-to-FileProvider harness
- Use [Neo-Honey Live Acceptance](neo-honey-acceptance.md) for the
  **credentialed live fleet sync path**
- Use [Lab Host Acceptance Matrix](lab-host-acceptance-matrix.md) for
  **real-host operator and end-user-ish acceptance**
- Use [Apple Surface Status](apple-surface-status.md) for the current limits of
  Finder, FileProvider, and iOS claims
