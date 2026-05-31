# TCFS Honey Backbone Preflight - 20260531T085713Z

Status: `blocked-g3`

Host under test: `honey`

## Daemon Gate

| Host | Storage OK | NATS Connected | Device |
| --- | --- | --- | --- |
| neo | yes | yes | `neo.local` |
| honey | yes | yes | `honey` |

## Registry Gate

| Check | Result |
| --- | --- |
| neo registry includes honey | no |
| honey registry includes neo | no |
| honey public key placeholder-shaped | yes |

## NATS Endpoint Probes

See `nats-probes.tsv` for raw read-only probes from both hosts.

## Blockers

- neo device registry does not include honey
- honey device registry does not include neo
- honey device public key is placeholder-shaped

## Claim Boundary

This is a read-only preflight. It does not enroll honey, change NATS or S3 endpoints, restart daemons, or move data.
