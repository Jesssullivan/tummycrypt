# TCFS Honey Backbone Preflight - 20260714T183629Z

Status: `blocked-g2`

Host under test: `honey`

## Daemon Gate

| Host | Storage OK | NATS Connected | Device |
| --- | --- | --- | --- |
| neo | no | no | `` |
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

- neo storage is not OK
- neo NATS is not connected

## Claim Boundary

This is a read-only preflight. It does not enroll honey, change NATS or S3 endpoints, restart daemons, or move data.
