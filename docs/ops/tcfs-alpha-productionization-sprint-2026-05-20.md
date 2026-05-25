# TCFS Alpha Productionization Sprint - May 20, 2026

This is the execution board for the current alpha push. The current dated todo
for the next daily-driver productionization pass is
[TCFS Daily Driver Productionization Todo - 2026-05-24](tcfs-daily-driver-productionization-todo-2026-05-24.md).

This board turns the
productionization plan into runnable gates and keeps the claim boundary strict:
macOS production FileProvider exact hydration, Linux package first-use, scoped
HTTPS storage posture, and the first 1 GiB synthetic Git-pack restore/recovery
packet are green. The first package-backed 3 GiB synthetic Git-pack candidate
proved the push/load side and exposed a restore-blocking transient `502` read
on the large pack. The remaining alpha-to-beta work is stronger large-restore
transient recovery, longer storage soak, FileProvider UX hardening, and keeping
the named neo/honey transcript current.

## Current Truth

| Lane | Tracker | Current state | Next action |
| --- | --- | --- | --- |
| Production S3/storage posture | `TIN-1546` | Current scoped HTTPS posture run `26246264661` on `main@43ce227` proves public HTTPS, `enforce_tls=true`, public CA trust, allowed-prefix list/write/read/delete/delete-verify, and denied-prefix `PermissionDenied` for `tcfs-storage-prod-smoke`. Mainline run `26378842677` passed the 1 GiB synthetic Git-pack large push + fresh-tree restore under `gha/storage-posture/large/...`: 1,074,101,201 bytes uploaded/restored, restore throughput 5,774,737 B/s, socket highwater 0, and exact restore despite transient `502` read retries. Run `26412362782` then exposed the package-backed 3.22 GiB restore failure. Run `26417405494` reran after PR `#462` and passed exact package-backed restore: 30 files / 3,222,239,922 bytes pushed and restored, one 3,222,208,701-byte Git pack, two empty dirs, socket highwater 0, 2,823s restore, and recovery despite 668 `502` log lines / 289 OpenDAL retries / 47 TCFS chunk retries | Keep `TIN-1546` open for beta storage. `TIN-1621` is done; `TIN-1622` tracks repeated soak/load, performance SLOs, retry/noise budget, and endpoint decision |
| Linux package first-use | `TIN-1540`, `TIN-1422`, `TIN-131`, `#280` | Public rc4 `.deb` smoke run `26218940925` passed install, storage `[ok]`, FUSE mount, exact hydrate, `tcfs cache evict` + rehydrate, and mutation remote pull against the hosted-reachable HTTPS backend. Homebrew current tap fresh-install smoke run `26221252765` and upgrade smoke run `26221711601` passed against `homebrew-tap@b5877df` (`v0.12.13-rc4`). PR #442 run `26243913292` passed Debian 13 fresh install, Debian 13 upgrade, Ubuntu 24.04 upgrade, Fedora 42 daemon-only fresh install, and Fedora 42 daemon-only sampled upgrade smokes. Nix profile install smoke passed in run `26242122899`; post-merge run `26382186102` then proved storage-backed `tcfs init --config-out`, `tcfs init check [ok]`, and live `storage: https://tcfs-smoke-s3.tinyland.dev [ok]`, with a 27-minute profile install latency caveat | Finish remaining package proof: NixOS host proof, rc package version semantics, and bounded Nix profile install latency |
| Named fleet acceptance | `TIN-132` | Fresh named transcript is archived at `docs/release/evidence/neo-honey-smoke-20260521T032725Z/`; CI Live Storage remains regression coverage, not a replacement for the named operator lane | Keep the transcript current for release-day acceptance or explicitly supersede the named-lane requirement in Linear |
| FileProvider post-M10 hardening | `TIN-1547` | Public `v0.12.13-rc4` `.pkg` run `26218940950` passed signed HostApp root enumeration, exact hydrate, evict/rehydrate, mutation, rename, and conflict/status. PZM run `26380511749` repeated the public rc4 package lane with five evict/rehydrate soak cycles against `tcfs-storage-prod-smoke` and passed exact hydrate, mutation remote pull, rename safety, and CLI conflict-status content hydrate | Add badge/progress/recovery capture and keep first-run setup proof under TIN-1425 |
| Enrollment and beta security | `TIN-1424`, `TIN-1417` | Full invite payload signature coverage landed. PR `#452` replaced placeholder local device public keys with real age/X25519 identities for init/enroll, and PR `#453` added single-use invite redemption/replay rejection. Self-enrollment remains unsafe as a production trust boundary | Implement admin/session gating, remove raw long-lived storage secrets from invite payloads, wrap bootstrap material to recipient device keys, and prove revocation-denies-new-content before exposing enrollment UX |

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
  packet is `26378842677`. PR `#461` made the dispatch default package-backed.
  Mainline run `26412362782` is the failed package-backed 3 GiB artifact: push
  passed, restore failed on a repeated `502` chunk read for the large Git pack.
  Mainline run `26417405494` is the first exact package-backed 3 GiB restore
  artifact: 30/30 files and 3,222,239,922 bytes restored, with heavy retry
  noise and low throughput. Keep the larger `TIN-1546` lane open for repeated
  soak/load behavior, retry/noise budgets, endpoint decisions, and benchmark
  rows.
- `TIN-1540` / `TIN-1422`: the hosted HTTPS backend and Linux first-use route
  are closed. Re-run them on release-day if the release candidate changes.
- `TIN-131/#280`: closed for the current alpha install/upgrade matrix. Debian
  13, Ubuntu 24.04, Fedora 42, and Nix profile install now have
  installed-binary or profile smoke. Keep the boundary explicit: this does not
  prove live-storage/FUSE/systemd behavior for every package surface.
- `TIN-132`: fresh named neo/honey transcript exists; keep it current for
  release-day acceptance or record an explicit supersede decision.
- `TIN-1547`: public rc4 exact hydrate and a five-cycle PZM desktop soak are
  archived. Keep open until badge/progress/recovery assertions are archived and
  first-run setup is proven through the user-facing path under TIN-1425.

## Claim Boundary

Alpha can claim trusted-operator QA on scoped roots after the storage, Linux,
and fleet packets are green. It must not claim primary home-directory takeover,
self-service enrollment, lost-device revocation, multitenant isolation, iOS
production readiness, Windows Explorer readiness, or daily-driver broad
directory ownership.
