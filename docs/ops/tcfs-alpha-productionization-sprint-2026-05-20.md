# TCFS Alpha Productionization Sprint - May 20, 2026

This is the execution board for the current alpha push. The current dated todo
for the next daily-driver productionization pass is
[TCFS Daily Driver Productionization Todo - 2026-05-24](tcfs-daily-driver-productionization-todo-2026-05-24.md).

This board turns the
productionization plan into runnable gates and keeps the claim boundary strict:
macOS production FileProvider exact hydration, Linux package first-use, scoped
HTTPS storage posture, and the first 1 GiB synthetic Git-pack restore/recovery
packet are green. The remaining alpha-to-beta work is package-backed
large-restore breadth, longer storage soak, FileProvider UX hardening, and
keeping the named neo/honey transcript current.

## Current Truth

| Lane | Tracker | Current state | Next action |
| --- | --- | --- | --- |
| Production S3/storage posture | `TIN-1546` | Current scoped HTTPS posture run `26246264661` on `main@43ce227` proves public HTTPS, `enforce_tls=true`, public CA trust, allowed-prefix list/write/read/delete/delete-verify, and denied-prefix `PermissionDenied` for `tcfs-storage-prod-smoke`. PR `#448` merged the ANSI summary/transient classification fix, and mainline run `26378842677` on `main@40b4514` passed the 1 GiB synthetic Git-pack large push + fresh-tree restore under `gha/storage-posture/large/...`: 1,074,101,201 bytes uploaded/restored, restore throughput 5,774,737 B/s, socket highwater 0, and exact restore despite transient `502` read retries | Keep TIN-1546 open for package-backed multi-GiB restore, longer soak/load behavior, and benchmark rows |
| Linux package first-use | `TIN-1540`, `TIN-1422`, `TIN-131`, `#280` | Public rc4 `.deb` smoke run `26218940925` passed install, storage `[ok]`, FUSE mount, exact hydrate, `tcfs cache evict` + rehydrate, and mutation remote pull against the hosted-reachable HTTPS backend. Homebrew current tap fresh-install smoke run `26221252765` and upgrade smoke run `26221711601` passed against `homebrew-tap@b5877df` (`v0.12.13-rc4`). PR #442 run `26243913292` passed Debian 13 fresh install, Debian 13 upgrade, Ubuntu 24.04 upgrade, Fedora 42 daemon-only fresh install, and Fedora 42 daemon-only sampled upgrade smokes. Nix profile install smoke passed in run `26242122899` | Finish remaining package proof: NixOS host proof and rc package version semantics |
| Named fleet acceptance | `TIN-132` | Fresh named transcript is archived at `docs/release/evidence/neo-honey-smoke-20260521T032725Z/`; CI Live Storage remains regression coverage, not a replacement for the named operator lane | Keep the transcript current for release-day acceptance or explicitly supersede the named-lane requirement in Linear |
| FileProvider post-M10 hardening | `TIN-1547` | Public `v0.12.13-rc4` `.pkg` run `26218940950` passed signed HostApp root enumeration, exact hydrate, evict/rehydrate, mutation, rename, and conflict/status. The postinstall harness now supports repeated evict/rehydrate soak cycles for PZM evidence packets | Dispatch a longer PZM soak, then add badge/progress/recovery capture and keep first-run setup proof under TIN-1425 |
| Enrollment and beta security | `TIN-1424`, `TIN-1417` | Full invite payload signature coverage landed; self-enrollment remains unsafe as a production trust boundary | Implement single-use redemption and admin/session gating before exposing enrollment UX |

## One-Command Preflight

Run the read-only gate classifier before dispatching anything:

```bash
scripts/tcfs-alpha-gate-preflight.sh
# or
just alpha-gate-preflight
```

The expected output today should show TIN-1546 and the Linux package smoke as
`runnable`. The printed Linux package tag defaults to the newest GitHub Release
tag unless `--tag` is provided explicitly.

Use strict mode when a release checklist should fail on blocked gates:

```bash
scripts/tcfs-alpha-gate-preflight.sh --strict
# or
just alpha-gate-preflight --strict
```

## Dispatch Commands After Secrets Exist

Storage posture:

```bash
scripts/storage-posture-canary-dispatch.sh \
  --environment tcfs-storage-prod-smoke \
  --runner-label ubuntu-24.04
```

Linux package smoke:

```bash
gh workflow run linux-postinstall-smoke.yml \
  -R Jesssullivan/tummycrypt \
  --ref main \
  -f tag=<current-release-tag> \
  -f runner_label=ubuntu-24.04 \
  -f smoke_environment=tcfs-linux-smoke \
  -f exercise_evict_rehydrate=true \
  -f exercise_mutation=true
```

Named fleet acceptance, from the operator environment:

```bash
just neo-honey-smoke
```

## Close Criteria

- `TIN-1546`: attach the `storage-posture-canary-<run_id>-<attempt>` artifact;
  `storage-canary.json` must show `endpoint_tls=true`,
  `enforce_tls=true`, delete verification, and denial-prefix
  `PermissionDenied`. Attach a matching `storage-large-restore-canary-*`
  artifact for large-object recovery; the merged-main 1 GiB synthetic Git-pack
  packet is `26378842677`. Keep the larger TIN-1546 lane open for
  package-backed multi-GiB restore, soak/load behavior, and benchmark rows.
- `TIN-1540` / `TIN-1422`: the hosted HTTPS backend and Linux first-use route
  are closed. Re-run them on release-day if the release candidate changes.
- `TIN-131/#280`: closed for the current alpha install/upgrade matrix. Debian
  13, Ubuntu 24.04, Fedora 42, and Nix profile install now have
  installed-binary or profile smoke. Keep the boundary explicit: this does not
  prove live-storage/FUSE/systemd behavior for every package surface.
- `TIN-132`: fresh named neo/honey transcript exists; keep it current for
  release-day acceptance or record an explicit supersede decision.
- `TIN-1547`: keep open until badge/progress/recovery, first-run setup, and a
  longer desktop soak are archived.

## Claim Boundary

Alpha can claim trusted-operator QA on scoped roots after the storage, Linux,
and fleet packets are green. It must not claim primary home-directory takeover,
self-service enrollment, lost-device revocation, multitenant isolation, iOS
production readiness, Windows Explorer readiness, or daily-driver broad
directory ownership.
