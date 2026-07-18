# ADR: Daemon-trusted stable root routing

- Date: 2026-07-14
- Status: Implemented in source; the live Git keep-both mechanism ran before
  the TIN-2856 incident freeze, while source hardening and residual conflicts
  remain open
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

The first resolve is a dry-run and requires an authenticated session with pull
permission. Execute performs those same reads and additionally requires push
permission, `policy = "resolve"`, and the explicit operator-intent bit set by
the shipped CLI. That protobuf boolean is client-supplied defense in depth, not
unforgeable attestation or an authorization boundary. MCP exposes no conflict-
resolution tool.

The legacy primary-cache repo-group path follows the same Git mode matrix:
dry-run requires pull and execute requires both pull and push. Its mutating
per-file strategies are retired; `defer` is the only remaining per-file verb
and is a push-authorized no-op. Registered-root policy does not apply to that
legacy primary route.

## Server-side invariants

For every registered root, tcfsd fails closed unless all of these hold:

1. `root_id` is a bounded identifier and is not the reserved `primary` name.
2. `local_root` is absolute, has no `..`, exists, is a directory, and is
   canonicalized again when selected.
3. `state_path` is a regular, non-symlink JSON file. It must be owned by the
   exact tcfsd effective UID, have no group/world access (mode 0600 or
   stricter), have exactly one hard link, and have a unique device/inode
   relative to the primary and peer caches. Its trusted state directory must
   also be owned by the exact tcfsd effective UID and must not be group/world
   writable.
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
8. After route and requested-path authorization, tcfsd captures stable
   filesystem identities for both the repository and its `.git` directory.
   Git mutation subprocesses and cooperative lock acquisition bind to those
   handles rather than reopening the authorized pathname. The daemon also
   revalidates both identities and the pathname after async storage/key waits,
   around mutation, and across state persistence; a same-path root or `.git`
   replacement is never selected as the mutation target. The same
   descriptor-bound path is used for legacy primary-cache Git keep-both.
9. Repo-group resolution requires Git itself to report the enrolled root,
   `<root>/.git`, and its common directory exactly. It accepts only the
   ordinary files-ref backend; `extensions.refStorage` (including reftable),
   an enabled `core.sharedRepository` mode, Gitfiles, `commondir`, attached
   worktree administration, external `core.worktree`, object alternates, and
   redirects under critical refs/objects/logs/config/index paths are rejected.
   Git routing/config-injection environment is removed from every resolver
   subprocess; repository hooks and fsmonitor execution are disabled.
   Topology is checked again under the TCFS lock before mutation, so linked and
   shared worktrees stay outside this slice.
10. Registered-root RPCs require `auth.require_session = true`. The
    development-only synthetic admin used by auth-disabled daemons never owns
    named-root authority.
11. The authenticated session must have pull permission for conflict
    inspection and Git keep-both dry-run. Execute requires both pull and push.
    Every mode also requires an `allowed_prefixes` grant that contains the
    registered prefix at an object-key segment boundary.
12. With pull authorization, `policy = "inspect-only"` permits conflict
    inspection and Git keep-both dry-run but rejects execute even if the
    session also has push. `policy = "resolve"` permits an authenticated,
    pull+push-authorized, operator-deliberate execute.

The entire named registered-root surface in this precursor, including
read-only `conflicts --root`, is Linux/macOS-only. Trusted route selection
fails closed on other platforms before inspecting or resolving a named root.
Every Git keep-both mutation, including the legacy primary-cache route, is also
descriptor-bound and fails closed elsewhere. Linux and macOS provide the
required child-side `fchdir` and descriptor-relative `openat` with
`O_NOFOLLOW`.

The enrolled local root, every checked critical Git metadata entry, the state
directory, and the state cache must each be owned by the exact tcfsd effective
UID. The root, critical Git metadata, and state directory must not be
group/world writable; the state cache must have no group/world access (mode
0600 or stricter). Their canonical ancestor chains must contain only real
directories owned by the effective UID or root and not writable by another
principal. A root-owned sticky directory is accepted only when the next child
is real and owned by the effective UID or root, preserving a protected
boundary such as `/tmp/<euid-owned-child>`. Shared repositories and a
system/DynamicUser service pointed at another user's tree fail closed; the
resolver does not claim a privileged cross-user mutation boundary or
Windows/CFAPI parity.

The daemon's effective local user is therefore inside this precursor's trust
boundary. The descriptor and ownership fences prevent an authenticated client
from redirecting daemon mutation to another pathname or Git common directory;
they are not a sandbox against hostile, concurrent in-place edits by that same
OS principal, who can already mutate the enrolled repository directly. Native
Git busy markers are checked before mutation, and `.git/tcfs.lock` serializes
TCFS writers only. Native same-euid Git does not take that lock, so races with
it remain outside this precursor's concurrency guarantee.

Unknown, syntactically valid IDs return a generic not-found response; tcfsd
does not enumerate enrolled IDs before prefix authorization. No request field
accepts a state path or remote prefix.

Registered roots inherit the daemon's one global storage transport, credential,
CA, and TLS-enforcement configuration. This slice namespaces object keys with
`remote_prefix`; it does not let an enrolled root weaken TLS or choose a second
endpoint.

Every indexed mutation also gates itself on a bounded live conditional-write
probe for the exact storage accessor and remote prefix. The probe races two
create-if-absent requests and two updates bound to the same ETag, then requires
a stale conditional read to fail. Operators constructed with local concurrency
limits use a separate unthrottled probe accessor so client-side serialization
cannot make a non-atomic endpoint appear safe. Successful versioned probe
writes are removed by exact object version; unsupported or failed semantics
abort before chunks, staging objects, or index entries are written.

Portable namespace admission is serialized in immutable reservations below
`<remote_prefix>/.tcfs-namespace/v1/`. Each cumulative, Unicode-casefolded path
prefix maps to one canonical spelling and a file-or-directory role. Ancestors
reserve the directory role, while the internal `.tcfs_dir` marker reserves its
logical parent rather than the marker name. Reservations are atomically
create-if-absent and never rolled back or deleted: partial acquisition remains
safe, but case-only renames and file/directory reuse require a future explicit
migration protocol. The legacy index is validated before reservation so an
existing alias fails without poisoning a new spelling.

Logical removal uses the same storage contract. A live index record is replaced
with a version-4 `deleted` tombstone under ETag compare-and-swap; the legacy
empty-directory marker is likewise replaced only when its exact canonical bytes
still match. Readers preserve `missing`, `deleted`, and `live` as distinct exact
states: a physically missing object is never remote deletion authority, and a
scheduled local removal rechecks the durable tombstone immediately before its
commit. Immutable manifests, chunks, staging objects, and trash payloads remain
recovery evidence. Trash creation and restore validate every current or staged
manifest binding before the index changes. Restore conditionally replaces only
an absent key, a tombstone, or byte-identical live value; purge is likewise
logical and retains the generation. The v4 tombstone binds the immutable trash
generation key and evidence digest at the exact delete linearization point; a
separate completion marker follows it. Either proof establishes completion, so
a lost marker response remains recoverable while a failed source CAS leaves an
operator-visible indeterminate copy. Historical timestamp-only generations
remain explicitly recoverable, but unbound UUID generations cannot be restored
or purged automatically. Positive-age retention starts from the
storage-assigned timestamp of the completion marker or exact evidence-bound
tombstone, never the earlier safety-copy write; legacy generations without that
proof require explicit purge-all. A fixed storage-clock guard is removed after
sampling, while a failed cleanup blocks another sample instead of leaking
per-attempt keys. Recovery of a retained guard is deliberately manual and must
be quiesced: first prove no trash-purge process is running, inspect
`{prefix}/.tcfs-trash-clock/v1/clock`, then use storage administration tooling
to remove only its exact visible version (or the proved bytes on an unversioned
backend) before retrying. Never overwrite or blindly delete that key while a
sampler may be live. Per-object scan corruption is reported and retained
without blocking valid rows or independently proved purge claims; a partially
successful CLI purge prints every escaped issue and exits unsuccessfully so
automation cannot mistake retained corruption for an empty trash. Prefix
repair retains recovery evidence rather than physically deleting a raced
source. Physical reclamation is deferred to version-pinned, reachability-safe
GC.

## State serialization

Executing scheduled `tcfs reconcile --state <isolated-cache>` and daemon-side
registered-root operations acquire the same non-blocking sibling lock:

```text
<state>.json.lock
```

For participating TCFS operations, the state lock covers open,
planning/inspection, remote reads, mutation, and flush.
It is separate from the JSON inode because `StateCache::flush` uses an atomic
rename. Temp and backup files are opened without following symlinks and are
mode 0600 before any state is written; hardlinked write targets fail closed.
Keep-both undo directories are 0700 and bundles are 0600 before Git streams
repository history into them. Contention fails with a retry-after-current-cycle
error instead of allowing two full-snapshot writers to clobber each other.
CLI `push`, `pull`, and `rm` also take this lock when an explicit `--state`
override is supplied; executing `reconcile` uses the same helper. The lock is
acquired before storage-client construction so contention cannot begin remote
work. Those explicit-state commands remain legacy mutation routes; they are
cooperatively serialized compatibility surfaces, not named-root authorization.
Only `tcfs conflicts --state <path>` is diagnostic and read-only.

State-cache recovery distinguishes corrupt serialized content from a failed
security or I/O boundary. Only the former may fall back to a securely opened
`.json.bak`; a missing primary also recovers an orphan backup instead of
silently starting empty. The first repair writes a durable primary without
rotating corrupt bytes over the known-good recovery copy. Symlink, hardlink,
owner, mode, ACL, parent-chain, and non-regular-file failures abort the open on
the Linux/macOS authority surface covered by this decision.

tcfsd also establishes its persistent per-user data directory before device,
state, storage, watcher, NATS, or socket side effects and holds a singleton
lock there for its full lifetime. The directory must already be, or be created
as, an effective-user-owned exact `0700` directory on Linux/macOS; an existing
looser directory is an operator-visible startup error and must be inspected
before its mode is corrected. Daemon policy/auth/delete authority never falls
back to `/tmp`. Pending automatic deletes are journaled there and use
same-directory atomic rename-without-replacement, so crash replay preserves
both staged and newly recreated paths rather than choosing one destructively.

## NATS boundary

Current NATS state events do not carry `root_id` or remote-prefix identity.
Registered-root resolution therefore does not publish `ConflictResolved`.
Publishing the primary event shape could cause another host to apply the path
under its primary root. Root-scoped NATS subjects/events belong to the broader
root lifecycle work.

## Compatibility and migration

- Legacy `ResolveConflict` has no root selector and remains the primary-cache
  RPC for deliberately invoked repository-group Git resolution. Once any roots
  are enrolled, tcfsd rejects that Git route for paths equal to or inside a
  named root, including symlink aliases and missing children; it also rejects
  ambiguous relative paths. Unrooted per-file mutations are disabled uniformly
  before path routing, while `defer` returns as a push-authorized no-op before
  path inspection.
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
- Namespace reservations likewise require a quiesced writer rollout. Older
  binaries do not consult `.tcfs-namespace/v1` and must not publish while the
  upgraded fleet begins creating reservations. Once created, reservations are
  monotonic and intentionally forbid case-only and file/directory name reuse.
- Version-4 deletion tombstones also require quiesced old readers and writers.
  Older binaries fail closed when they encounter the new state, but they do not
  understand logical restore/purge markers and must not mutate the same prefix
  during rollout. Non-dry-run prefix repair requires an explicit
  `--writers-quiesced` operator assertion, validates every manifest binding
  before publication, conditionally tombstones exact double-prefixed sources,
  and retains orphan-prefix sources. Trash restore and purge acquire one
  immutable, exclusive generation claim; restore records a separate completion
  marker so an interrupted claimed restore remains retryable. A restore claim
  is deliberately never released after a destination race: releasing a
  different object cannot fence another retry already approaching its index
  CAS, so evidence stays visible until the live conflict is resolved. Delete
  completion is also bound into the tombstone itself, so a lost post-CAS
  response stays recoverable and a failed source CAS cannot create a normal
  restorable generation.
- A configured `.db` spelling normalizes to its `.json` sibling for consistency
  with the primary state-cache convention.
- Enrollment is configuration-only in this slice. There is no adopt, remove,
  cross-machine path alias, root status, watcher, hydration, or worktree
  reconstruction lifecycle here.
- This slice does not add `tcfs reconcile --root` or named-root ordinary-file
  resolution. Existing scheduled units continue to pass their trusted
  path/prefix/state tuple directly; the shared lock only serializes that
  existing execution path with named inspection and Git keep-both.

## TIN-2658 evidence status

Source tests prove that named inspection and execute use one isolated cache,
the primary cache remains byte-identical, lock contention fails closed, prefix
and state-path escapes are rejected, pull-only sessions can dry-run,
push-only sessions cannot read through either mode, and inspect-only policy
blocks execute even for a pull+push session.

The attended Honey sequence ran before the TIN-2856 incident freeze. It stopped
the named timer, ran root-scoped dry-run and execute, restored reconciliation,
and produced a second cycle with zero `.git` conflicts. The 909+ cycle Git loop
is therefore broken, and the scratch files were moved aside reversibly.

That result does not close the live issue. Remaining acceptance is:

```text
land the reviewed stable-root source -> adjudicate README.md and AGENTS.md
as deliberate user-content conflicts -> handle the stale roam-canary-wip ref
pair -> capture final root-scoped Git/content/state convergence evidence
```

Git keep-both success alone is not TIN-2658 closure. If ordinary-file conflicts
remain, this slice deliberately stops rather than routing them through the
primary cache or accepting a client-supplied state path. TIN-2856 now freezes
all further live resolver, enrollment/TOTP, deploy, and crypto ceremonies;
source review and tests may continue without making a new fleet claim.
