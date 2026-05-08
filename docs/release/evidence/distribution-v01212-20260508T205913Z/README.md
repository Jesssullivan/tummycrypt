# v0.12.12 Homebrew And Nix Distribution Proof

Date: 2026-05-08

Host:

- `neo.local`
- `Darwin 25.5.0 arm64`
- Homebrew `5.1.10-33-g6839384` before auto-update; Homebrew auto-updated during the run
- Nix `(Determinate Nix 3.17.0) 2.33.3`

Release:

- Tag: `v0.12.12`
- Release URL: <https://github.com/Jesssullivan/tummycrypt/releases/tag/v0.12.12>
- Release target: `43e8721a46623011cffd66769c432a9b27a6fd79`
- Published: `2026-05-06T17:24:14Z`

Repo context:

- Proof PR branch commit: `81910244f59632fdca7e6c04a1755fd43b2d81ba`
- Homebrew tap branch: `homebrew-tap`
- Homebrew tap commit: `07dc9949f57921366c60a36728924f560a6d819a`

## Result

| Surface | Fresh install | Upgrade | Smoke output captured | Notes |
| --- | --- | --- | --- | --- |
| Homebrew | pass | pass | yes | Upgraded `tcfs` from `0.12.2` to `0.12.12`, then uninstalled and reinstalled `0.12.12`; smoke used explicit `/opt/homebrew/opt/tcfs/bin/*` paths because this host shadows `tcfs` through local and Nix paths. |
| Nix | pass | sampled / n-a | yes | Installed `tcfsd` and `tcfs-cli` from `github:Jesssullivan/tummycrypt?ref=v0.12.12` into temporary profile `/private/tmp/tcfs-nix-profile-v01212`; no user Nix profile was changed. |

## Homebrew Commands

```bash
git -C /opt/homebrew/Library/Taps/jesssullivan/homebrew-tummycrypt fetch origin homebrew-tap
git -C /opt/homebrew/Library/Taps/jesssullivan/homebrew-tummycrypt merge --ff-only origin/homebrew-tap
brew upgrade Jesssullivan/tummycrypt/tcfs
env -i PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin \
  bash scripts/install-smoke.sh \
  --expected-version 0.12.12 \
  --tcfs /opt/homebrew/opt/tcfs/bin/tcfs \
  --tcfsd /opt/homebrew/opt/tcfs/bin/tcfsd
brew uninstall --force Jesssullivan/tummycrypt/tcfs
brew install Jesssullivan/tummycrypt/tcfs
env -i PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin \
  bash scripts/install-smoke.sh \
  --expected-version 0.12.12 \
  --tcfs /opt/homebrew/opt/tcfs/bin/tcfs \
  --tcfsd /opt/homebrew/opt/tcfs/bin/tcfsd
```

Key outputs:

```text
Upgraded 1 outdated package
jesssullivan/tummycrypt/tcfs 0.12.2 -> 0.12.12
```

```text
tcfs 0.12.12
```

```text
==> jesssullivan/tummycrypt/tcfs: stable 0.12.12
Installed (on request)
/opt/homebrew/Cellar/tcfs/0.12.12 (11 files, 34.9MB) *
```

Fresh smoke transcript:

```text
tcfsd version: tcfsd 0.12.12
tcfs version: tcfs 0.12.12
starting daemon smoke with temp home: /tmp/tcfs-install-smoke.nSIvXm/home
daemon socket ready: /tmp/tcfs-install-smoke.nSIvXm/home/.local/state/tcfsd/tcfsd.sock
tcfsd v0.12.12
  uptime:        1s
  socket:        /tmp/tcfs-install-smoke.nSIvXm/home/.local/state/tcfsd/tcfsd.sock
  device:        neo.local (e38f0828)
  conflict mode: auto
  storage:       http://localhost:8333 [UNREACHABLE]
  nats:          not connected
  active mounts: 0
  credentials:   loaded (source: env)
install smoke passed
```

## Nix Commands

```bash
XDG_CACHE_HOME=/private/tmp/tcfs-nix-cache \
  nix --accept-flake-config profile install \
  --profile /private/tmp/tcfs-nix-profile-v01212 \
  'github:Jesssullivan/tummycrypt?ref=v0.12.12#tcfsd' \
  'github:Jesssullivan/tummycrypt?ref=v0.12.12#tcfs-cli'

XDG_CACHE_HOME=/private/tmp/tcfs-nix-cache \
  nix profile list --profile /private/tmp/tcfs-nix-profile-v01212

env -i PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin \
  bash scripts/install-smoke.sh \
  --expected-version 0.12.12 \
  --tcfs /private/tmp/tcfs-nix-profile-v01212/bin/tcfs \
  --tcfsd /private/tmp/tcfs-nix-profile-v01212/bin/tcfsd
```

Profile output:

```text
Name:               tcfs-cli
Flake attribute:    packages.aarch64-darwin.tcfs-cli
Original flake URL: github:Jesssullivan/tummycrypt/v0.12.12
Locked flake URL:   github:Jesssullivan/tummycrypt/43e8721a46623011cffd66769c432a9b27a6fd79
Store paths:        /nix/store/m6rxcjdb08xarb4ch9xxc0anay9pan3m-tcfs-cli-0.12.12

Name:               tcfsd
Flake attribute:    packages.aarch64-darwin.tcfsd
Original flake URL: github:Jesssullivan/tummycrypt/v0.12.12
Locked flake URL:   github:Jesssullivan/tummycrypt/43e8721a46623011cffd66769c432a9b27a6fd79
Store paths:        /nix/store/g2klcn1j1920yfg821j5v78li0i2xdva-tcfsd-0.12.12
```

Smoke transcript:

```text
tcfsd version: tcfsd 0.12.12
tcfs version: tcfs 0.12.12
starting daemon smoke with temp home: /tmp/tcfs-install-smoke.6t40Bp/home
daemon socket ready: /tmp/tcfs-install-smoke.6t40Bp/home/.local/state/tcfsd/tcfsd.sock
tcfsd v0.12.12
  uptime:        1s
  socket:        /tmp/tcfs-install-smoke.6t40Bp/home/.local/state/tcfsd/tcfsd.sock
  device:        neo.local (8bc36130)
  conflict mode: auto
  storage:       http://localhost:8333 [UNREACHABLE]
  nats:          not connected
  active mounts: 0
  credentials:   loaded (source: env)
install smoke passed
```

Notes:

- The first Nix attempt failed before evaluation because the sandbox blocked Nix's default fetcher cache under `~/.cache/nix`; rerunning with `XDG_CACHE_HOME=/private/tmp/tcfs-nix-cache` fixed that without changing the user profile.
- The successful Nix install used the configured build/substitution environment. The transcript included intermittent failures from one configured remote builder at `100.111.5.80`, successful copies/builds through `100.124.134.27`, and a final profile with both `tcfs-cli` and `tcfsd`.
- `scripts/install-smoke.sh` only requires installed binary execution, daemon socket readiness, and `tcfs status`. It does not require storage `[ok]`, so `storage: http://localhost:8333 [UNREACHABLE]` is expected in this isolated no-config smoke.
