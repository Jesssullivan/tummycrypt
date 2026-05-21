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

Current evidence note for `v0.12.13-rc1`: the release workflow published the
macOS `.pkg`, Linux tarballs, `.deb` packages, daemon-only `.rpm`, multi-arch
container image, Nix build/cache outputs, checksums, and Sigstore files. The
strongest product-surface proof is macOS: PZM production Dev ID run
`26062554542` proves installed strict preflight, storage `[ok]`, domain add,
CloudStorage enumeration, exact hydrate, evict/rehydrate, mutation
upload/readback, and conflict-status preservation without
`fileprovider_testing_mode=true`. Post-cut PR #389 branch run `26079830341`
then proves the exact published GitHub Release `.pkg` through install,
installed-binary smoke, storage `[ok]`, exact hydrate, evict/rehydrate,
mutation, and conflict/status. A merged-workflow main-ref rerun is still useful
for continuous release-day viability, but the release asset itself is no longer
only an inferred proof.

2026-05-21 update: PR #429 fixed mixed `.deb`/`.rpm` artifact selection in the
Linux postinstall workflow, and post-merge run `26204699596` proved the rc3
`dist-linux-x86_64` release-workflow artifact on hosted Ubuntu: `.deb` install,
daemon startup, HTTPS storage `[ok]`, FUSE mount, seeded index `visible`, exact
hydrate, evict/rehydrate, and mutation remote pull. This upgrades the Ubuntu
`.deb` path from artifact publication to package-backed first-use proof for the
tested hosted-Ubuntu lane.

Later 2026-05-21 update: `v0.12.13-rc4` public release assets are published and
the package-facing public-asset smokes are green from `main@e9b9f82`:

- Linux `.deb` run
  [`26218940925`](https://github.com/Jesssullivan/tummycrypt/actions/runs/26218940925)
  installed `tcfs-0.12.13-rc4-amd64.deb` and proved daemon start, HTTPS storage
  `[ok]`, FUSE mount, seeded index `visible`, exact hydrate, `tcfs cache evict`
  plus rehydrate, and mutation remote pull against
  `https://tcfs-smoke-s3.tinyland.dev`.
- macOS `.pkg` run
  [`26218940950`](https://github.com/Jesssullivan/tummycrypt/actions/runs/26218940950)
  installed `tcfs-0.12.13-rc4-macos-aarch64.pkg` and proved signed HostApp
  root authority, exact signed-host hydrate, evict/rehydrate, FileProvider
  mutation, rename, and conflict/status.
- Container runtime run
  [`26218940985`](https://github.com/Jesssullivan/tummycrypt/actions/runs/26218940985)
  proved `ghcr.io/jesssullivan/tcfsd:v0.12.13-rc4` on `linux/amd64` and
  `linux/arm64/v8` with manifest digest
  `sha256:4b1f235be8a20715b8eb1bb2de38a81b3628a6f56675420cdc2320cce19c20b3`.

Homebrew current-tap fresh install also passed after the rc4 release job updated
`homebrew-tap@b5877df`: run `26221252765` installed
`/opt/homebrew/Cellar/tcfs/0.12.13-rc4` and the binaries reported
`tcfs 0.12.13` / `tcfsd 0.12.13`.

Remaining distribution proof is now breadth and upgrade heavy: Homebrew
upgrade, Debian 13 `.deb` install/upgrade, Fedora daemon-only `.rpm` install,
external Nix profile install, and release-candidate package version semantics
(`0.12.13-1` installed from rc4-named `.deb` assets). Debian 12 remains blocked
by the libc/OpenSSL floor unless a separate bookworm-targeted package exists.
See
[`docs/release/v0.12.13-evidence-matrix.md`](../release/v0.12.13-evidence-matrix.md)
for the frozen rc-series evidence table.

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
bash scripts/install-smoke.sh --expected-version "${BINARY_EXPECTED_VERSION}"
```

If the surface is daemon-only and does not ship `tcfs` today, run:

```bash
bash scripts/install-smoke.sh --expected-version "${BINARY_EXPECTED_VERSION}" --skip-cli
```

Set `BINARY_EXPECTED_VERSION` from the release variables in the surface
procedures below. This intentionally drops prerelease suffixes because the
published asset can be `0.12.13-rc1` while the compiled Cargo binary reports
`tcfsd 0.12.13`.

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
export BINARY_EXPECTED_VERSION="${VERSION%%-*}"
```

`VERSION` is the artifact version and may include a prerelease suffix such as
`0.12.13-rc1`. `BINARY_EXPECTED_VERSION` is the Cargo package version reported
by installed binaries, so prerelease package smokes should still expect
`tcfsd 0.12.13`.

### Homebrew

Fresh install:

```bash
brew untap Jesssullivan/tummycrypt 2>/dev/null || true
brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
brew install Jesssullivan/tummycrypt/tcfs
bash scripts/install-smoke.sh --expected-version "${BINARY_EXPECTED_VERSION}"
```

Upgrade:

```bash
git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
brew upgrade Jesssullivan/tummycrypt/tcfs
bash scripts/install-smoke.sh --expected-version "${BINARY_EXPECTED_VERSION}"
```

### macOS `.pkg`

Fresh install:

```bash
curl -LO "https://github.com/Jesssullivan/tummycrypt/releases/download/${TAG}/tcfs-${VERSION}-macos-aarch64.pkg"
sudo installer -pkg "tcfs-${VERSION}-macos-aarch64.pkg" -target /
bash scripts/install-smoke.sh --expected-version "${BINARY_EXPECTED_VERSION}"
```

If this tag also needs packaged-install to first-real-use proof on macOS, follow
the package smoke with the named FileProvider harness:

```bash
bash scripts/macos-postinstall-smoke.sh \
  --expected-version "${BINARY_EXPECTED_VERSION}" \
  --config "$HOME/.config/tcfs/config.toml" \
  --expected-file "path/to/known/remote-backed-file" \
  --expected-content-file /tmp/tcfs-expected-content.txt
```

For a fresh one-off fixture, replace `--expected-file` with
`--seed-expected-file` or set `SEED_EXPECTED_FILE=1` on
`task lazy:macos-finder-smoke`. The harness pushes the fixture, archives the
push transcript, and fails early unless `tcfs index inspect <path> --json`
reports a `visible` remote index entry before FileProvider hydration starts.

For neo dogfood, the May 16, 2026 packet
`docs/release/evidence/macos-fileprovider-neo-pkg-install-20260516T024006Z/`
verifies the published `v0.12.12` package signature/notarization and quarantines
the stale user app, but it does not count as a fresh install: non-interactive
`sudo installer` required a password, so `/Applications/TCFSProvider.app` was
not installed.

The May 16, 2026 remote packet
`docs/release/evidence/macos-fileprovider-pkg-notarization-proof-20260516T211425Z/`
proves the source-built package can pass Apple notarization, stapling,
Gatekeeper install assessment, and strict package smoke with
`--require-signature`, `--require-gatekeeper-install`, and
`--require-stapled-ticket`. It is not a tagged release asset or host install
smoke and does not replace the fresh install command above.

The May 16, 2026 follow-up neo packets
`docs/release/evidence/macos-fileprovider-neo-notarized-pkg-inventory-20260516T222519Z/`
and
`docs/release/evidence/macos-fileprovider-neo-notarized-pkg-install-20260516T222606Z/`
verify the downloaded notarized artifact locally, then record the real local
install blocker: `sudo -n installer` requires admin authentication, so
`/Applications/TCFSProvider.app` remains absent.

The May 17, 2026 neo packets supersede that blocker for the workflow artifact:

- `docs/release/evidence/macos-fileprovider-neo-notarized-pkg-install-auth-20260517T005618Z/`
  installs the notarized artifact into `/Applications` with authenticated
  `osascript`; strict preflight still fails because PlugInKit reports both the
  canonical app and the stale user app
- `docs/release/evidence/macos-fileprovider-neo-stale-userapp-quarantine-20260517T010423Z/`
  intentionally moves the stale user app under evidence quarantine after the
  install packet exists; PlugInKit still reports the quarantined path
- `docs/release/evidence/macos-fileprovider-neo-strict-preflight-installed-20260517T010916Z/`
  proves strict installed preflight with one PlugInKit registration under
  `/Applications/TCFSProvider.app`
- `docs/release/evidence/macos-fileprovider-neo-package-daemon-env-20260517T012916Z/`
  records the daemon environment fix: the package daemon reaches storage
  `[ok]` from file-backed credentials and the stale user daemon is gone
- `docs/release/evidence/macos-fileprovider-neo-finder-release-smoke-directhost-catread-20260517T020417Z/`
  proves production-signed domain add, CloudStorage enumeration, and host-app
  `requestDownload`, then blocks on `cat` returning `Operation timed out`
- `docs/release/evidence/macos-postinstall-prod-devid-hydration-20260518T212705Z/`
  supersedes the May 17 read blocker. Run `26061402177` proves production Dev
  ID exact hydration, and run `26062554542` adds evict/rehydrate, mutation
  upload/readback, and conflict-status preservation.

When an operator reruns this lane, keep using the evidence helper with an
explicit auth mode instead of hand-running `installer` outside the packet:

```bash
EVIDENCE_DIR="docs/release/evidence/macos-fileprovider-neo-notarized-pkg-install-$(date -u +%Y%m%dT%H%M%SZ)" \
PKG_PATH="/tmp/tcfs-notarized-pkg-25973109986/tcfs-0.12.12-macos-aarch64.pkg" \
INSTALL_PKG=1 \
INSTALL_MODE=osascript \
STRICT_PREFLIGHT=1 \
task lazy:macos-fileprovider-neo-cleanup-packet
```

Use `INSTALL_MODE=sudo` instead of `osascript` when running in an interactive
terminal with sudo authentication. Do not set `QUARANTINE_STALE=1` on the first
canonical install attempt; quarantine stale user/build-tree apps only after the
install packet exists and the verbose PlugInKit inventory identifies the stale
registration target. After strict installed preflight passes, the next
distribution-facing gap is not package installation; it is exact-content
FileProvider hydration through the installed production app.

Upgrade:

```bash
sudo installer -pkg "tcfs-${VERSION}-macos-aarch64.pkg" -target /
bash scripts/install-smoke.sh --expected-version "${BINARY_EXPECTED_VERSION}"
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
bash scripts/install-smoke.sh --expected-version "${BINARY_EXPECTED_VERSION}"
```

Upgrade:

```bash
sudo dpkg -i "tcfsd-${VERSION}-amd64.deb" "tcfs-${VERSION}-amd64.deb"
bash scripts/install-smoke.sh --expected-version "${BINARY_EXPECTED_VERSION}"
```

### Fedora `.rpm`

The current `v0.12.12` proof covers Fedora 42 x86_64 daemon-only. RHEL/Rocky
remain target surfaces pending smoke.

Fresh install:

```bash
curl -LO "https://github.com/Jesssullivan/tummycrypt/releases/download/${TAG}/tcfsd-${VERSION}-x86_64.rpm"
sudo rpm -i "tcfsd-${VERSION}-x86_64.rpm"
bash scripts/install-smoke.sh --expected-version "${BINARY_EXPECTED_VERSION}" --skip-cli
```

Upgrade is sampled unless the RPM packaging path changes materially:

```bash
sudo rpm -Uvh "tcfsd-${VERSION}-x86_64.rpm"
bash scripts/install-smoke.sh --expected-version "${BINARY_EXPECTED_VERSION}" --skip-cli
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
bash scripts/install-smoke.sh --expected-version "${BINARY_EXPECTED_VERSION}"
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
