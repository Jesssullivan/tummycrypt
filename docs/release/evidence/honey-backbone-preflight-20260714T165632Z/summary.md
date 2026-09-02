# TCFS Honey Backbone Preflight - 20260714T165632Z

Status: `honey-backbone-preflight-complete`

Host under test: `honey`

## Daemon Gate

| Host | Storage OK | NATS Connected | Device |
| --- | --- | --- | --- |
| neo | yes | yes | `neo.local` |
| honey | yes | yes | `honey` |

## Registry Gate

| Check | Result |
| --- | --- |
| neo registry includes honey | yes |
| honey registry includes neo/neo.local | yes |
| honey public key placeholder-shaped | no |

## NATS Endpoint Probes

See `nats-probes.tsv` for raw read-only probes from both hosts.

## Blockers

None.

## Claim Boundary

This is a read-only preflight. It does not enroll honey, change NATS or S3 endpoints, restart daemons, or move data.
