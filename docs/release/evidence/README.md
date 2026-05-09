# Release Evidence Index

This directory stores repo-archived evidence packets and links to external CI
run logs when the evidence lives in GitHub Actions artifacts instead of files in
the repository.

## Current `v0.12.12` Packets

| Packet | Scope | Evidence |
| --- | --- | --- |
| `distribution-v01212-20260508T205913Z/` | Homebrew fresh/upgrade and Darwin Nix tagged-profile install | repo-archived README and metadata |
| `container-v01212-20260509T0145Z/` | Container image current-tag smoke | native arm64 pull fails because the image index lacks `linux/arm64/v8`; explicit amd64 pull/version pass and worker startup reaches process/metrics initialization before failing on missing local NATS |
| `lazy-linux-20260508T170825Z/` | Linux FUSE lifecycle on `honey`: browse before hydration, exact `cat`, mounted write/readback, cache clear/rehydrate, dirty recursive `unsync` refusal, clean recursive `.tc` conversion, persisted `NotSynced` state | repo-archived transcript, config, mount log, remote prefix, remote pullback, unsync outputs, redacted metadata |
| PZM testing-mode FileProvider package run | Mac App Development/testing-mode package build for deterministic conflict/status proof | <https://github.com/Jesssullivan/tummycrypt/actions/runs/25569345240> |
| PZM testing-mode FileProvider smoke run | Enumerate/hydrate/evict/rehydrate, mutation proof already present from prior run, deterministic CLI conflict/status and exact FileProvider content preservation | <https://github.com/Jesssullivan/tummycrypt/actions/runs/25569596910> |
| PZM testing-mode mutation package run | Mac App Development/testing-mode package build for mutation proof | <https://github.com/Jesssullivan/tummycrypt/actions/runs/25565895586> |
| PZM testing-mode mutation smoke run | CloudStorage mutation upload and exact 68-byte remote pullback | <https://github.com/Jesssullivan/tummycrypt/actions/runs/25565943781> |
| PZM testing-mode evict/rehydrate smoke run | Installed host policy probe, E2EE, daemon startup, FileProvider registration, enumeration, requestDownload, evict, re-requestDownload, exact 55-byte hydration | <https://github.com/Jesssullivan/tummycrypt/actions/runs/25562087555> |

## Scope Notes

- PZM FileProvider runs are non-production Mac App Development/testing-mode
  evidence with the lab `SystemPolicyRule` profile. They do not prove a
  production Developer ID clean-host Finder lane.
- `distribution-v01212-20260508T205913Z/` covers Homebrew and Nix only. It does
  not cover current-tag `.deb`, `.rpm`, container, or production macOS `.pkg`
  smoke.
- `container-v01212-20260509T0145Z/` covers the `v0.12.12` container image
  only. It proves amd64 image presence/version/startup logs and records a
  missing native arm64 manifest.
- Linux `lazy-linux-20260508T170825Z/` proves the mounted lifecycle and
  recursive safe-unsync behavior, not Linux package fresh/upgrade install.
