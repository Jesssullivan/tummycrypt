# ADR: Daemon-trusted stable root routing

- Date: 2026-07-14
- Status: Implemented in source; live enrollment and TIN-2658 execution remain
  operator-gated
- Scope: Strategy A conflict inspection and repo-group keep-both only
- Tracks: TIN-2853 (child of TIN-2658); related to TIN-1556 and TIN-2657;
  intentionally does not complete the broad root ownership/adoption program

## Context

The primary daemon cache is not the cache used by every scheduled reconcile
root. Fleet units can reconcile an enrolled repository into an isolated file
such as:

```text
~/.local/state/tcfsd/reconcile/git-roam-tool-daemon.json
```

Before this decision, `tcfs conflicts --state ...` could inspect that file, but
`tcfs resolve` always asked the daemon to mutate its primary cache. An operator
could therefore inspect six conflicts and then execute a successful-looking
resolution against a different cache. Passing a raw state path through the RPC
would fix the immediate plumbing while turning an ordinary push session into a
daemon file-write primitive.

## Decision

Non-primary roots are enrolled in daemon configuration as a stable identity:

```toml
[sync]
# Set this explicitly when the daemon socket lives in a runtime directory.
root_state_dir = "/home/jess/.local/state/tcfsd/reconcile"

[sync.roots.git-roam-tool-daemon]
local_root = "/home/jess/git/tinyland-tool-daemon"
remote_prefix = "git-roam/tool-daemon"
state_path = "/home/jess/.local/state/tcfsd/reconcile/git-roam-tool-daemon.json"
policy = "resolve"
```

The mapping is:

```text
root_id -> { local_root, remote_prefix, state_path, policy }
```

Clients send only `root_id` as the routing selector. The dedicated
`ResolveRegisteredRoot` RPC also carries a bounded repo-group mode and the
requested repo path as a checked assertion, but the daemon selects every
filesystem and object-store value from its own configuration. Keeping named
resolution off legacy `ResolveConflict` makes daemon downgrade fail with
`Unimplemented` instead of silently discarding a routing field and touching the
primary cache. The two operator verbs share this route:

```bash
tcfs conflicts --root git-roam-tool-daemon
tcfs resolve /home/jess/git/tinyland-tool-daemon \
  --root git-roam-tool-daemon --strategy keep-both
tcfs resolve /home/jess/git/tinyland-tool-daemon \
  --root git-roam-tool-daemon --strategy keep-both --execute
```

The first resolve is a dry-run. Execute requires an authenticated session with
push permission, `policy = "resolve"`, and the explicit operator-intent bit set
by the shipped CLI. That protobuf boolean is client-supplied defense in depth,
not unforgeable attestation or an authorization boundary. MCP does not expose
the bit.

## Server-side invariants

For every registered root, tcfsd fails closed unless all of these hold:

1. `root_id` is a bounded identifier and is not the reserved `primary` name.
2. `local_root` is absolute, has no `..`, exists, is a directory, and is
   canonicalized again when selected.
3. `state_path` is a regular, non-symlink JSON file. On Unix it must have no
   group/world access, exactly one hard link, a unique device/inode relative to
   the primary and peer caches, and the same owner as its trusted,
   non-group/world-writable state directory.
4. The state filename is exactly `<root_id>.json` under
   `sync.root_state_dir`, both before and after canonicalization. If that
   directory is unset, the default per-user socket layout uses the daemon
   socket's sibling `reconcile/` directory. Services whose socket is under
   systemd `%t` or `/run` must configure the existing persistent state
   directory explicitly.
5. The state directory, including keep-both undo bundles, is component-disjoint
   from the primary and every registered local root before and after
   canonicalization. State and full-history rollback material cannot roam.
6. `remote_prefix` is a normalized relative object-key prefix. Every conflict
   cache key must remain below `local_root`; every recorded remote key and
   manifest key must remain below that exact prefix. A registered root may not
   equal, contain, or sit beneath the primary `sync_root` or primary storage
   prefix; registered peers are likewise component-disjoint. Selection
   rechecks the canonical primary root and every currently canonicalizable
   peer, so symlink aliases cannot bypass the lexical startup check;
   unavailable paths are reconsidered as soon as they appear.
7. The requested repo path canonicalizes to the registered `local_root`.
8. Repo-group resolution requires Git itself to report the enrolled root,
   `<root>/.git`, and its common directory exactly. Gitfiles, `commondir`,
   attached worktree administration, external `core.worktree`, object
   alternates, and redirects under critical refs/objects/logs/config/index paths are
   rejected. Git routing/config-injection environment is removed from every
   resolver subprocess; repository hooks and fsmonitor execution are disabled.
   Topology is checked again under the Git lock before mutation, so linked and
   shared worktrees stay outside this slice.
9. The authenticated session must have pull permission to inspect, push
   permission to resolve, and an `allowed_prefixes` grant that contains the
   registered prefix at an object-key segment boundary.
10. `policy = "inspect-only"` permits inspection and dry-run but rejects
    execute; `policy = "resolve"` permits an authenticated,
    operator-deliberate execute.

Unknown, syntactically valid IDs return a generic not-found response; tcfsd
does not enumerate enrolled IDs before prefix authorization. No request field
accepts a state path or remote prefix.

Registered roots inherit the daemon's one global storage transport, credential,
CA, and TLS-enforcement configuration. This slice namespaces object keys with
`remote_prefix`; it does not let an enrolled root weaken TLS or choose a second
endpoint.

## State serialization

Executing scheduled `tcfs reconcile --state <isolated-cache>` and daemon-side
registered-root operations acquire the same non-blocking sibling lock:

```text
<state>.json.lock
```

The lock covers open, planning/inspection, remote reads, mutation, and flush.
It is separate from the JSON inode because `StateCache::flush` uses an atomic
rename. Temp and backup files are opened without following symlinks and are
mode 0600 before any state is written; hardlinked write targets fail closed.
Keep-both undo directories are 0700 and bundles are 0600 before Git streams
repository history into them. Contention fails with a retry-after-current-cycle
error instead of allowing two full-snapshot writers to clobber each other.

## NATS boundary

Current NATS state events do not carry `root_id` or remote-prefix identity.
Registered-root resolution therefore does not publish `ConflictResolved`.
Publishing the primary event shape could cause another host to apply the path
under its primary root. Root-scoped NATS subjects/events belong to the broader
root lifecycle work.

## Compatibility and migration

- Legacy `ResolveConflict` has no root selector and remains the primary-cache
  RPC. Once any roots are enrolled, tcfsd rejects that RPC for paths equal to
  or inside a named root, including symlink aliases and missing children; it
  also rejects ambiguous relative paths. Older clients remain usable for
  absolute primary paths but cannot mutate an enrolled named root.
- Named repo-group resolution uses the new dedicated
  `ResolveRegisteredRoot` RPC. Its response carries the server-selected
  `root_id`, canonical local root, remote prefix, and state path from the same
  invocation that performs the dry-run or execute. The CLI verifies and prints
  that atomic route evidence. An older daemon cannot implement the method and
  therefore fails before mutation.
- `tcfs conflicts --state <path>` remains available as a legacy, offline,
  read-only diagnostic. Named-root resolution never accepts it.
- Existing isolated caches do not move. Enrollment points the trusted registry
  at the existing `<root_id>.json` file.
- The sibling lock is cooperative. During rollout, freeze the named reconcile
  timer until its CLI is upgraded to this lock-aware build; an older process
  does not honor the new lock file.
- A configured `.db` spelling normalizes to its `.json` sibling for consistency
  with the primary state-cache convention.
- Enrollment is configuration-only in this slice. There is no adopt, remove,
  cross-machine path alias, root status, watcher, hydration, or worktree
  reconstruction lifecycle here.
- This slice does not add `tcfs reconcile --root` or named-root ordinary-file
  resolution. Existing scheduled units continue to pass their trusted
  path/prefix/state tuple directly; the shared lock only serializes that
  existing execution path with named inspection and Git keep-both.

## Evidence required for TIN-2658

Source tests prove that named inspection and execute use one isolated cache,
the primary cache remains byte-identical, lock contention fails closed, prefix
and state-path escapes are rejected, and inspect-only policy blocks execute.

Closing the live issue still requires the attended sequence on the enrolled
host:

```text
freeze named reconcile -> auth verify -> root-scoped Git dry-run -> execute
-> inspect the same named cache -> resolve or explicitly preserve every
ordinary-file conflict through an approved root-aware path
-> re-enable named reconcile -> first reconcile -> second clean reconcile
-> root-scoped conflict inspection -> git/content/state convergence evidence
```

Git keep-both success alone is not TIN-2658 closure. If ordinary-file conflicts
remain, this slice deliberately stops rather than routing them through the
primary cache or accepting a client-supplied state path. This ADR does not
claim that the ceremony has occurred.
