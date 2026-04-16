# Distribution Smoke Matrix

Canonical post-release proof for packaged distribution surfaces.

As of 2026-04-16, release quality for `tcfs` means more than "artifacts exist."
A tagged release is not considered fully proven until the shipped install
surfaces below have passed their required smoke checks.

## Gate Decision

- **Fresh install proof is required on every shipped release surface for every `v0.12.x+` tag**:
  - Homebrew
  - macOS `.pkg`
  - Debian/Ubuntu `.deb`
  - Fedora/RHEL `.rpm`
  - container image
  - Nix package path
- **Upgrade proof is required on the primary mutable installer surfaces**:
  - Homebrew
  - macOS `.pkg`
  - Debian/Ubuntu `.deb`
- **RPM, container, and Nix upgrade proof is sampled rather than mandatory on every tag**:
  - RPM is daemon-only today, so its proof surface is narrower than Homebrew, `.pkg`, or `.deb`
  - container upgrades are normally orchestrator rollouts rather than a host-local installer flow
  - Nix installs are immutable and ref-pinned, so the per-tag fresh install gate is the most meaningful baseline

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
- Kubernetes rollout semantics

Those remain surface-specific follow-on checks, documented below.

## Surface Matrix

| Surface | Fresh install gate every tag | Upgrade gate every tag | Required smoke | Notes |
|---------|------------------------------|------------------------|----------------|-------|
| Homebrew | Yes | Yes | `scripts/install-smoke.sh` | Current manual `homebrew-tap` checkout flow |
| macOS `.pkg` | Yes | Yes | `scripts/install-smoke.sh` | Apple Silicon package path; desktop UX still experimental |
| Debian/Ubuntu `.deb` | Yes | Yes | `scripts/install-smoke.sh` | Install both `tcfsd` and `tcfs` packages |
| Fedora/RHEL `.rpm` | Yes | Sampled | `scripts/install-smoke.sh --skip-cli` | RPM ships `tcfsd` only today |
| Container image | Yes | Sampled | worker-image startup check | Pull + entrypoint/startup proof, not CLI status |
| Nix | Yes | Sampled | `scripts/install-smoke.sh` | Fresh install from the tagged flake is the primary proof |

## Surface Procedures

Set the release variables first:

```bash
export TAG=v0.12.1
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

Upgrade:

```bash
sudo installer -pkg "tcfs-${VERSION}-macos-aarch64.pkg" -target /
bash scripts/install-smoke.sh --expected-version "${VERSION}"
```

### Debian/Ubuntu `.deb`

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

### Fedora/RHEL/Rocky `.rpm`

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

Fresh install proof for the worker image is:

```bash
podman pull "ghcr.io/jesssullivan/tcfsd:${TAG}"
podman run --rm "ghcr.io/jesssullivan/tcfsd:${TAG}" --version
timeout 10s podman run --rm --entrypoint /tcfsd \
  "ghcr.io/jesssullivan/tcfsd:${TAG}" \
  --mode=worker \
  --config /tmp/missing.toml \
  --log-format text
```

Success criteria:

- the image pulls successfully
- `tcfsd --version` reports the expected release version
- the worker binary starts cleanly enough to emit startup logs before `timeout` stops it

Full cluster rollout proof remains an infra-level check and should be paired with
the live fleet runbooks when container packaging or worker behavior changes.

### Nix

Fresh install:

```bash
nix profile install \
  "github:Jesssullivan/tummycrypt?ref=${TAG}#tcfsd" \
  "github:Jesssullivan/tummycrypt?ref=${TAG}#tcfs-cli"
bash scripts/install-smoke.sh --expected-version "${VERSION}"
```

Upgrade is sampled unless the flake packaging path or install story changes
materially between releases.

## Evidence Capture

Record the outcome for each tag in the release issue, PR, or a maintainer comment
using a table like this:

| Surface | Fresh install | Upgrade | Smoke output captured | Notes |
|---------|---------------|---------|-----------------------|-------|
| Homebrew | pass/fail | pass/fail | yes/no | |
| macOS `.pkg` | pass/fail | pass/fail | yes/no | |
| Debian/Ubuntu `.deb` | pass/fail | pass/fail | yes/no | |
| Fedora/RHEL `.rpm` | pass/fail | sampled/n-a | yes/no | |
| Container image | pass/fail | sampled/n-a | yes/no | |
| Nix | pass/fail | sampled/n-a | yes/no | |

## Relationship To Other Acceptance Lanes

- Use this matrix for **distribution surface proof**
- Use [Neo-Honey Live Acceptance](neo-honey-acceptance.md) for the
  **credentialed live fleet sync path**
- Use [Lab Host Acceptance Matrix](lab-host-acceptance-matrix.md) for
  **real-host operator and end-user-ish acceptance**
- Use [Apple Surface Status](apple-surface-status.md) for the current limits of
  Finder, FileProvider, and iOS claims
