# Lab Host Acceptance Matrix

As of April 16, 2026, release proof for `tcfs` needs three distinct layers:

1. **Packaged artifact proof** from [Distribution Smoke Matrix](distribution-smoke-matrix.md)
2. **Live backend proof** from [Neo-Honey Live Acceptance](neo-honey-acceptance.md)
3. **Real host acceptance** on the Tinyland lab fleet

This runbook defines the third layer.

Its purpose is to answer a different question than release smoke:

Can the currently supported lab hosts exercise `tcfs` as operators and end users
would actually experience it, without stale harness state or historical fleet
artifacts contaminating the result?

## Active Host Pool

Use only hosts that are operationally real today.

| Host | Role in acceptance | What it is good for | Current limits |
| --- | --- | --- | --- |
| `honey` | canonical Linux control point | high-volume push/pull, daemon/service checks, conflict and stress lanes, Linux-first operator truth | none beyond normal fleet drift |
| `neo` | canonical release-adjacent Darwin lane | package upgrade proof, live `neo-honey` smoke, regression reproduction on the maintainer workstation | not a good target for destructive “fresh machine” cleanup |
| `petting-zoo-mini` | canonical headless Darwin endpoint | FileProvider packaging presence, Darwin daemon parity, Linux-to-Darwin and Darwin-to-Linux operator acceptance, end-user-ish desktop surface checks | external SSD, PPPC, and MDM work can interfere; preflight first |
| `sting` | none yet | none until it is real | blocked on lab-side hardware stabilization and onboarding; do not treat it as an acceptance target |

## Lane Map

These lanes are meant to stack, not replace one another.

### 1. Artifact Gate

Run [Distribution Smoke Matrix](distribution-smoke-matrix.md) first.

This proves the released binaries install and start. It does **not** prove that a
real host with an existing daemon, real credentials, real launchd/systemd
state, or real FileProvider baggage behaves correctly.

### 2. Live Backend Gate

Run [Neo-Honey Live Acceptance](neo-honey-acceptance.md) next.

This is the canonical “SeaweedFS + NATS + two real identities” smoke lane.
It is still valuable even when host-lab acceptance expands, because it gives a
fast signal on the fleet-sync path without dragging in Darwin UI or packaging
state.

### 3. Real Host Acceptance

Use the lab hosts to cover the operational and end-user surfaces that the first
two lanes intentionally miss.

Recommended lane order:

1. `honey -> petting-zoo-mini`
   - proves Linux-originated content reaches the active Darwin endpoint
   - good default for cross-platform sync, hydration, parity, and conflict checks
2. `petting-zoo-mini -> honey`
   - proves Darwin-originated content can still publish and round-trip cleanly
   - useful after package upgrades, daemon identity changes, or FileProvider-adjacent work
3. `neo -> honey`
   - keep as the release-adjacent control lane from [Neo-Honey Live Acceptance](neo-honey-acceptance.md)
4. `honey` solo stress / edge-mode work
   - use for higher-volume conflict, retry, daemon resilience, or batch object tests where Linux is the least ambiguous runtime

Do **not** use `sting` in any required acceptance matrix until the lab repo has
resolved its host onboarding and hardware issues with real host facts.

## Reset And Cleanup Contract

Before any host-backed acceptance run, clean the harness state enough that the
result reflects current code and current infra, not previous test residue.

Minimum reset contract per host:

1. Remove stale temp files created by the fleet tests.
   Use patterns like:
   - `/tmp/tcfs-e2e*`
   - `/tmp/tcfs-golden*`
   - `/tmp/tcfs-conflict*`
   - `/tmp/tcfs-rt-*`
   - `/tmp/tcfs-rh-*`
   - `/tmp/tcfs-large-*`
   - `/tmp/tcfs-e2e-helper.sh`
2. Confirm `tcfsd` is the daemon you intend to test.
   - `tcfs status`
   - service/agent health
   - expected binary path and version
3. Confirm credentials and fleet backends are live.
   - S3 / SeaweedFS reachable
   - NATS reachable
   - `tcfs` credential helper or env injection still valid
4. Confirm the harness repo state is intentional.
   - `~/git/tinyland-e2e-harness`
   - `~/tcfs/repos/tinyland-e2e-harness` if the mirrored path is under test
5. Review remote-object hygiene if historical index churn is suspected.
   - dry-run orphan scan first
   - only run destructive orphan cleanup intentionally

For `petting-zoo-mini`, add one more gate:

6. Confirm the host-specific Darwin workstream is not already degraded.
   - PPPC / MDM expectations are known
   - external SSD rollout is not currently in a bad state if the acceptance lane depends on it
   - the machine is not already under runner or storage distress

## Current Operator Commands

The current operator surface lives in the Tinyland `lab` repo. As of April 16,
2026, the useful commands are:

```bash
# Fleet preflight on the hosts you actually plan to use
just tcfs-preflight honey petting-zoo-mini neo

# Core TCFS cross-host pytest lane
just fleet-test-tcfs "--source-host=honey --target-host=petting-zoo-mini"
just fleet-test-tcfs "--source-host=petting-zoo-mini --target-host=honey"

# Conflict and placeholder lifecycle lane
just fleet-test-conflicts "--source-host=honey --target-host=petting-zoo-mini"

# Cross-host parity lane, including Darwin FileProvider presence checks
just fleet-test-parity

# Legacy shell smoke, still useful for quick operator confirmation
just tcfs-e2e-test honey petting-zoo-mini
just tcfs-golden-test honey
```

Use `neo-honey-smoke` from this repo for the live backend lane, and use the
`lab` repo recipes for host-backed acceptance. They answer different questions.

## Evidence To Capture

For each host-backed run, record:

| Lane | Source host | Target host | Result | Notes |
| --- | --- | --- | --- | --- |
| live backend | `neo` | `honey` | pass/fail | SeaweedFS + NATS + two-device smoke |
| host sync | `honey` | `petting-zoo-mini` | pass/fail | Linux -> Darwin acceptance |
| host sync | `petting-zoo-mini` | `honey` | pass/fail | Darwin -> Linux acceptance |
| conflicts | `honey` | `petting-zoo-mini` | pass/fail | placeholder + conflict lifecycle |
| parity | fleet | fleet | pass/fail | config, repo, and FileProvider presence |

Capture exact host names and dates in the evidence. Avoid statements like
“Darwin passed” without naming which Darwin host actually ran the lane.

## Scope Boundaries

This matrix is still not the same thing as a fully automated desktop UX claim.

Even after `petting-zoo-mini` becomes a routine acceptance target:

- Finder badges are not automatically release-proven
- full FileProvider mutation UX is not automatically release-proven
- iOS is still separate from Darwin desktop acceptance
- `sting` remains out of scope until it is a real host, not a placeholder

Use this runbook to tighten the bridge from packaged artifacts to real operator
and user-ish host behavior, not to over-claim what the product has proven.
