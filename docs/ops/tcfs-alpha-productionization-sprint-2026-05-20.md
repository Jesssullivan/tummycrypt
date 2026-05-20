# TCFS Alpha Productionization Sprint - May 20, 2026

This is the execution board for the current alpha push. It turns the
productionization plan into runnable gates and keeps the claim boundary strict:
macOS production FileProvider exact hydration is green, while storage posture,
Linux first-use, and fresh neo/honey evidence remain open until their packets
exist.

## Current Truth

| Lane | Tracker | Current state | Next action |
| --- | --- | --- | --- |
| Production S3/storage posture | `TIN-1546` | Workflow, custom CA support, scoped denial proof, and dispatch helper are on `main`; `tcfs-storage-prod-smoke` has no secrets yet | Populate scoped HTTPS S3-compatible secrets, then run the storage canary dispatch helper |
| Linux package first-use | `TIN-1540`, `TIN-1422`, `TIN-131`, `#280` | Linux workflow and harness are present; hosted Linux rejects the current private/plaintext endpoint by design; `tcfs-linux-smoke` has no secrets yet | Reuse the hosted-reachable HTTPS storage backend, then run Linux smoke with evict/rehydrate and mutation enabled |
| Named fleet acceptance | `TIN-132` | CI Live Storage is green regression coverage, but no fresh named `neo`/`honey` transcript is archived for this sprint | Run `just neo-honey-smoke` from the operator environment and archive the transcript |
| FileProvider post-M10 hardening | `TIN-1547` | Public `v0.12.13-rc2` `.pkg` passed signed HostApp root enumeration, exact hydrate, evict/rehydrate, mutation, and conflict/status; PR #412 landed rename/unsync safety | Add signed-package hardening proof for rename/unsync behavior, badge/progress/recovery capture, and a longer desktop soak |
| Enrollment and beta security | `TIN-1424`, `TIN-1417` | Full invite payload signature coverage landed; self-enrollment remains unsafe as a production trust boundary | Implement single-use redemption and admin/session gating before exposing enrollment UX |

## One-Command Preflight

Run the read-only gate classifier before dispatching anything:

```bash
scripts/tcfs-alpha-gate-preflight.sh
# or
just alpha-gate-preflight
```

The expected blocked output today is:

- `TIN-1546`: missing required secrets in `tcfs-storage-prod-smoke`
- `TIN-1540/TIN-1422`: missing required secrets in `tcfs-linux-smoke`
- `TIN-132`: operator-run-required because named neo/honey evidence cannot be
  inferred from CI Live Storage

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
  -f tag=v0.12.13-rc2 \
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
  `PermissionDenied`.
- `TIN-1540`: close only after the selected route is real: hosted-reachable
  HTTPS secrets in `tcfs-linux-smoke`, or an online Linux self-hosted runner.
- `TIN-1422`: close only after package install, config, FUSE mount,
  seeded-index visibility, exact hydrate, evict/rehydrate, and mutation are
  green from the Linux workflow.
- `TIN-131/#280`: keep open until the distribution matrix records Linux
  first-use and production storage posture, not just macOS/Homebrew/container
  rows.
- `TIN-132`: close only from a fresh named neo/honey transcript or an explicit
  tracker decision to supersede that gate. The current decision is to keep the
  fresh transcript as required.
- `TIN-1547`: close the current hardening slice only after rename/unsync safety
  is proven through a signed package and badge/progress/recovery or soak
  evidence is archived.

## Claim Boundary

Alpha can claim trusted-operator QA on scoped roots after the storage, Linux,
and fleet packets are green. It must not claim primary home-directory takeover,
self-service enrollment, lost-device revocation, multitenant isolation, iOS
production readiness, Windows Explorer readiness, or daily-driver broad
directory ownership.
