# ADR: Versioned root registry and immutable status (B0a)

- **Date:** 2026-07-19
- **Status:** Accepted for source implementation under TIN-2863; no live
  deployment or lifecycle claim
- **Scope:** Authorized root discovery and immutable persisted-state status
- **Predecessor:** [Daemon-trusted stable root routing](stable-root-routing-2026-07-14.md)
- **Historical rejected design:** [CLI-local root identity](root-identity-adr-2026-07-14.md)

## Context

PR #551 established a narrow daemon-trusted route for conflict inspection and
Git keep-both resolution. Its unversioned `[sync.roots.<id>]` entries bind a
local path, remote prefix, state cache, and conflict-resolution policy. That
surface is intentionally coupled to the production conflict-recovery seam; it
is not a versioned product registry and cannot safely be promoted into one by
changing the meaning of existing configuration.

B0a needs a smaller, read-only foundation for the later root lifecycle:

- one fleet-stable identity that survives unlike host paths;
- an optional host-local binding with its own identity;
- authorized discovery without disclosing other enrolled roots;
- deterministic status from an immutable persisted-state snapshot; and
- an explicit statement that reconcile, mutation, and MCP exposure have not
  arrived.

## Decision

B0a introduces a separate, strict registry at
`[sync.root_registry.<root_id>]`. The existing `[sync.roots.<root_id>]`
registry remains the PR #551 conflict-only compatibility surface. Neither
registry is inferred from, merged with, or reinterpreted as the other.

```toml
[sync.root_registry.work.spec]
version = 1
remote_prefix = "roots/work"
profile = "git-raw-v1"
generation = 1

[sync.root_registry.work.binding]
version = 1
local_root = "/srv/fast-local/jess/git/work"
state_path = "/home/jess/.local/state/tcfsd/reconcile/work.json"
lifecycle_policy = "inspect-only"
resolution_policy = "inspect-only"
```

The map key is the authoritative `root_id`. Version 1 accepts only the
`git-raw-v1` and `agent-static-v1` profiles. Unknown fields, unsupported
versions and profiles, zero generations, invalid IDs, malformed prefixes, and
non-absolute or parent-traversing binding paths fail closed.

### Fleet-stable specification

`RootSpecV1` contains:

- `version`;
- `root_id`;
- `remote_prefix`;
- `profile`; and
- a non-zero `generation`.

Its `identity_fingerprint` is a domain-separated, tagged, length-delimited
BLAKE3 digest rendered as `b3v1:<hex>`. It deliberately excludes host-local
paths and policy so the same versioned root has the same identity on
heterogeneous machines.

### Host-local binding

`RootBindingV1` contains:

- `version`;
- canonical local-root and state-cache paths;
- `lifecycle_policy`; and
- `resolution_policy`.

Its `binding_fingerprint` is a separately domain-separated
`b3v1:<hex>` digest over those canonical host-local fields. A path or binding
that cannot be validated and canonicalized does not receive a trusted binding
fingerprint.

Specification generations are explicit and are not inferred from timestamps
or filesystem metadata.

## Read surface

tcfsd exposes two authenticated gRPC methods:

- `ListRegisteredRoots`, which returns authorized V1 rows in ASCII `root_id`
  order; and
- `GetRegisteredRootStatus`, which returns one authorized V1 row.

The CLI exposes the same daemon-owned surface:

```text
tcfs roots list [--json]
tcfs roots status <root_id> [--json]
```

There is no client-side state-file fallback. Clients never send a local path,
state path, remote prefix, policy, or fingerprint. A syntactically valid
unknown ID and an ID outside the session's component-bounded
`allowed_prefixes` grant return the same generic not-found response. List
filtering happens before host filesystem probes, so inventory does not become
an enrollment oracle.

Every B0a row reports one availability:

- `UNBOUND`;
- `UNSUPPORTED_PLATFORM`;
- `LOCAL_ROOT_MISSING`;
- `STATE_MISSING`;
- `INVALID_BINDING`;
- `BUSY`; or
- `READY`.

`reconcile_support` is always `NONE` in B0a, including for a descriptor whose
configured lifecycle policy says `reconcile`. Policy records future operator
intent; it does not grant a source or runtime capability.

### Immutable persisted-state status

A `READY` row may include exclusive counts for `not_synced`, `synced`,
`active`, `locked`, and `conflict`; their sum is `total`. They describe one
validated snapshot of the registered root's primary state-cache file. They are
not live task telemetry.

Status inspection:

1. never creates, truncates, chmods, repairs, or rewrites the state cache or
   its sibling lock;
2. reads the primary cache only, without backup recovery;
3. holds an existing state lock while reading, or uses the non-creating
   missing-lock stability check;
4. reports contention as `BUSY`; and
5. validates every indexed local path, remote key, manifest key, and conflict
   record against the daemon-selected root and prefix before returning counts.

A missing or corrupt primary cache remains visible as unavailable. Read-only
inventory must not silently repair state or promote a backup, because either
action would turn status into mutation and could hide the exact condition an
operator is inspecting.

## Explicit exclusions

B0a does **not** add:

- `reconcile --root`, a reconcile plan, or reconcile execution;
- root add, adopt, update, remove, hydrate, unsync, restore, or migration;
- ordinary-file or Git conflict mutation through the V1 registry;
- any MCP tool, TUI surface, watcher, NATS event, or fleet subscription;
- a bridge from V1 rows into the legacy `ListConflicts` or
  `ResolveRegisteredRoot` RPCs; or
- a live Lab/Rockies rollout, enrollment, authentication, TOTP, resolver,
  crypto, or credential ceremony.

The legacy `[sync.roots]` route keeps its PR #551 behavior. B0a inventory
enumerates only `[sync.root_registry]`; a legacy entry with the same ID is a
configuration error rather than an implicit migration.

## Security and live-work boundary

The daemon remains the authority for configuration, authentication, prefix
authorization, canonical paths, state-file validation, and status
classification. An auth-disabled synthetic administrator receives no V1
inventory authority.

TIN-2856 remains the live-work fence. TIN-2863 is source, test, and
documentation work only. A green build or immutable-status test is not
evidence that any host was enrolled, reconciled, mutated, deployed, or
authenticated through this surface.

## Follow-on gates

B0b may add a reconcile plan only after the registry, authorization,
state-snapshot, and route-evidence contract is reviewed. Execution remains a
separate gate. B0c may connect the accepted contract to rendered fleet
configuration and attended evidence. TUI or safe MCP reads require their own
explicit schema, authorization, disclosure, and downgrade review; they do not
inherit approval from these local CLI/gRPC reads.
