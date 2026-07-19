# TCFS product sequence and root-identity decision

Status: **accepted 2026-07-14**. Strategy **A → B → C** is the product order.
This record was grounded against tummycrypt `origin/main` at
`21f8df303596d1b9f6f90cc7953eb8f65f353ac3`, the live Linear/GitHub lanes,
and the named fleet evidence in [`ops/current.md`](ops/current.md).
The stable-root source boundary was refreshed on 2026-07-19 after PR #551
landed; this does not refresh or widen the live fleet evidence.

## Decision

TCFS will first make the bounded `neo`/`honey` beachhead trustworthy. It
will then generalize enrolled roam roots. Only after those two stages pass
their evidence gates will it become a shippable Linux/Rocky substrate with one
honest Finder client, then selectively widen home-state and client breadth.

This is not a mechanism-first roadmap. Each stage ends in a user workflow that
can be repeated from packaged software and leaves a durable evidence packet.

## A — Trustworthy Beachhead

The beachhead consists of two named hosts, two named Git repositories, one
bounded agent-state root, one encrypted backend, and the CLI/daemon control
plane. It is complete only when all rows below are green.

| Gate | Required outcome | Evidence |
| --- | --- | --- |
| Delivery | A host update cannot rewind the canonical release to an older downstream mirror | Pin/mirror test plus host version convergence |
| Transport | Credential-bearing S3 traffic uses authenticated TLS | Config/source proof plus live endpoint and read/write smoke |
| Root routing | `resolve` selects a registered root inside the daemon while preserving session auth | Unit/integration tests and TIN-2658 ceremony |
| Conflict closure | Dry-run, execute, next reconcile, and a second clean cycle succeed for the production root | Neo/honey packet with before/after state |
| SSH-first auth | A headless user can verify, reuse, and renew a bounded session without bypassing auth | TIN-2653 live packet |
| Enrollment | A fresh authorized device can persist the invitation/bootstrap result | Reboot/restart and revocation proof |
| Two-repo stop rule | Two small repositories pass roam, unsync, rehydrate, divergence, restore, and reverse direction | One packet per repo plus combined summary |
| Fleet coherence | Neo, honey, and sting run the intended canonical build; Bumble retains its formal R6 role | Version and topology inventory |
| Product truth | README, vision, current workstream, and tracker say the same thing | Docs/link check and tracker cross-links |

Open breadth does not block A unless it violates a beachhead invariant.
Per-device-only crypto, linked worktrees, broad home takeover, WebAuthn, NFS,
Windows, iOS, and formal Rockies adoption stay red during this phase.

## Stable root identity

TIN-2658 exposed the architectural gap: scheduled reconciles use isolated
state caches and prefixes, while daemon resolution only knows the primary
cache and prefix. A client-side `--state` escape hatch would point at the
right bytes but discard the daemon's authority and cannot safely bind the
prefix, path, or policy.

The target B0 design is a versioned set of trusted, daemon-owned root
descriptors rendered through configuration. Ordinary reconcile and file
resolution clients may eventually select an ID; they may not create or rewrite
its tuple. Landed PR #551 is deliberately narrower: it adds an unversioned,
conflict-only precursor for named inspection and Git keep-both resolution.
TIN-2863/B0a adds a separate strict V1 inventory; it does not reinterpret the
PR #551 registry or grant its entries new authority.

```text
stable root_id
    ├── fleet spec
    │   ├── remote prefix
    │   ├── profile
    │   └── generation
    └── optional host binding
        ├── local path       (host-specific, absolute)
        ├── state cache     (daemon-owned location)
        └── lifecycle and resolution policy
```

The same `root_id` may map to different local paths on macOS and Linux. Its
remote prefix is the fleet-wide convergence identity. The descriptor is local,
contains no credentials, and is loaded from the daemon's trusted configuration.
The V1 fleet spec and host binding have separate domain-separated BLAKE3
fingerprints so a host-path change cannot silently change the fleet identity.

B0a is an immutable, authorized read surface only. It supports
`git-raw-v1` and `agent-static-v1`, reports descriptor/binding availability and
persisted state counts, and always reports reconcile support as `NONE`. It adds
no mutation, MCP tool, enrollment, or live deployment. The precise contract is
recorded in
[the B0a root-registry ADR](design/versioned-root-registry-status-b0a-2026-07-19.md).

Required invariants for the complete B0 lifecycle (not claims that every item
is implemented by PR #551):

1. Root IDs use a bounded, validated slug and cannot contain path traversal.
2. State files are configured through the trusted descriptor and normalized by
   the daemon; an RPC never accepts an arbitrary state path.
3. The daemon reloads the selected registered state and uses its registered
   prefix for Git and ordinary-file resolution.
4. The requested path must be contained by the registered local root after
   symlink-aware validation.
5. Reconcile and resolve take the same state-adjacent operation lock so two
   processes cannot overwrite the same JSON state cache.
6. Named-root operations require real daemon session enforcement; an
   auth-disabled synthetic administrator never receives registered-root
   authority.
7. The daemon captures the authorized repository's stable filesystem identity,
   binds Git mutation subprocesses and cooperative lock acquisition to that
   handle, and revalidates the configured pathname across async waits and state
   persistence. A same-path replacement is never selected as the mutation
   target.
8. Session authentication, mode-specific permission checks, registered-prefix
   authorization, the Git corruption fence, encryption context, and
   undo-bundle rules remain in the daemon path. Named dry-run requires `pull`;
   execute requires both `pull` and `push`. An inspect-only root permits the
   pull-authorized dry-run but rejects execute even for a pull+push session.
   The existing `operator_cli` request hint and exclusion of conflict
   resolution from MCP are defense in depth, not an authorization boundary by
   themselves.
9. Dry-run and execute report the root ID, registered prefix, and plan summary
   so evidence proves which trusted root was touched.

The legacy primary-cache RPC now follows the same repo-group capability split:
Git dry-run requires `pull`, while execute requires both `pull` and `push`.
The unrooted legacy per-file mutations (`keep-remote`, `keep-local`, and
`keep-both`) are disabled fail-closed because they cannot bind a requested
pathname to a daemon-selected root and indexed manifest. `defer` remains a
push-authorized no-op. Ordinary-file resolution waits for the broader root
lifecycle and manifest-identity design to supply that missing authority.

PR #551's entire named registered-root surface, including read-only
`conflicts --root`, is implemented only on Linux and macOS; trusted route
selection fails closed on every other platform. Every Git keep-both mutation,
including the legacy primary-cache route, captures the repository and `.git`
directory and binds Git mutation to those descriptors with child-side
`fchdir` and descriptor-relative `openat`; it also fails closed elsewhere.

This precursor accepts only an ordinary files-ref repository. Reftable and an
enabled `core.sharedRepository` mode are rejected. The enrolled local root,
critical Git metadata, state directory, and state cache must each be owned by
the exact tcfsd effective UID. The root, critical metadata, and state directory
must not be group/world writable, and the cache must have no group/world access
(mode 0600 or stricter). Every canonical ancestor must be a real directory
owned by that effective UID or root and not writable by another principal. A
root-owned sticky directory is accepted only as a protected boundary whose
next child is real and owned by the effective UID or root. Shared repositories
and system services pointed at another user's root are outside this seam. This
is not a Windows/CFAPI or privileged cross-user root-resolution support claim.

The `.git/tcfs.lock` advisory lock serializes TCFS writers only. Native Git
busy markers are checked before mutation, but a same-euid process can still
modify the repository in place; races with native same-euid Git are outside
this precursor's concurrency guarantee.

The existing scratch implementation that opens a caller-selected cache and
runs keep-both inside the CLI is therefore a research artifact, not a landing
candidate. It bypasses daemon session authentication, cannot make `--root`
select the correct prefix by itself, and only handles the Git group while
ordinary file conflicts remain.

### Minimal A and B0a surfaces

Status on 2026-07-19: PR #551 is landed and implements named conflict
inspection and the bounded Git keep-both resolve path. TIN-2863 adds the
source-only, immutable V1 inventory/status seam. Neither surface implements
`reconcile --root`, named-root ordinary-file resolution, or Lab
enrollment/rendering. Reconcile planning and execution remain staged behind
the later B0 gates.

```bash
# Landed source from PR #551 (legacy conflict-only registry)
tcfs conflicts --root git-roam-tool-daemon
tcfs resolve --root git-roam-tool-daemon \
  . --strategy keep-both
tcfs resolve --root git-roam-tool-daemon \
  . --strategy keep-both --execute

# TIN-2863/B0a source contract (separate V1 registry, immutable reads)
tcfs roots list
tcfs roots status git-roam-tool-daemon

# Staged B0b plan surface; not implemented yet
tcfs reconcile --root git-roam-tool-daemon

# Later execution gate; not B0a or B0b
tcfs reconcile --root git-roam-tool-daemon --execute
```

For the implemented named resolver, an authenticated pull-only session may run
the dry-run. Execute requires the same pull authorization plus push permission
and a root with `policy = "resolve"`; `policy = "inspect-only"` never permits
execute. These are source-tested semantics, not evidence that a new live auth
or resolver ceremony occurred during the TIN-2856 freeze.

Lab must eventually render an accepted versioned descriptor and host binding
into daemon config. Today `resolve --root` selects the legacy
daemon-trusted tuple, `conflicts --root` is read-only, and V1 `roots
list/status` cannot reconcile or resolve anything. A future privileged
root-add/update surface belongs to B. `conflicts --state` remains a diagnostic
command. Legacy `push`, `pull`, `rm`, and executing `reconcile` with an
explicit `--state` are still mutation routes; PR #551 serializes them with the
same state lock, but they are not V1 root-registry authorization surfaces.

## TIN-2658 live evidence and residual closure

The live Honey keep-both sequence ran on 2026-07-14 before the TIN-2856
incident freeze:

1. The named timer was stopped and root-targeted dry-run/execute completed.
2. Scratch files were moved aside reversibly.
3. The first cycle pushed the kept Git internals; the second recorded zero
   `.git` conflicts, breaking the 909+ cycle loop.
4. Two deliberate user-content conflicts (`README.md`, `AGENTS.md`) and one
   stale `roam-canary-wip` ref pair remain.
5. PR #551/TIN-2853 landed on 2026-07-18 as merge commit `929bbf1`; that
   accepts the source seam but does not clear the remaining live-work freeze.

TIN-2658 therefore remains In Review rather than Done. TIN-2856 blocks any
further live resolver, enrollment/TOTP, deploy, or crypto ceremony. When that
freeze clears, closure requires explicit content decisions, stale-ref handling,
and final Git/content/state convergence evidence; Git-only success is not a
whole-root convergence claim.

## B — Roam Roots

Once A is green, make root registration a product surface:

- enrollment and removal with explicit policy classes;
- fleet subscriptions and host allowlists;
- stable aliases for unlike local paths;
- immutable authorized root discovery and status in CLI first; TUI and safe
  MCP reads require separate disclosure and authorization review;
- cross-OS cwd mapping for SSH/IDE routing;
- generation-aware migrations and recovery;
- the linked-worktree reconstruction design and migration contract rather than
  blind roaming of Git pointer files; live support and proof remain in C;
- policy-driven pin, hydrate, unsync, and restore.

B succeeds when a fresh host can discover its authorized roots, map them to
valid local paths, hydrate one, work, unsync it, and recover without editing a
unit file or copying a state cache.

## C — Shippable Substrate and Clients

Turn the proven root lifecycle into a supportable product before widening the
matrix:

- Rocky 10/10.1 RPM packaging, service policy, signed artifacts, clean install,
  upgrade, rollback, uninstall, and vendor acceptance through Rockies;
- polished Finder/FileProvider status, progress, recovery, and first run;
- one shared root/session/hydrate/unsync/recovery conformance contract across
  the Linux substrate and Finder client;
- stable support diagnostics, privacy boundaries, update/revocation policy,
  and a versioned vendor protocol/ABI contract;
- agent sessions, prompts, selected dot-directories, repository collections,
  and user-chosen home subtrees;
- live linked-worktree reconstruction and recovery proof;
- NFS parity and a justified FUSE-free Linux story;
- Windows CFAPI and iOS lifecycle proofs;
- capacity, performance, and APFS-versus-TCFS benchmark packets that measure
  the TCFS path rather than presenting an APFS baseline as a TCFS result.

C never becomes implicit whole-home ownership. Enrollment remains selective and
reversible.

## Repository ownership

| Repository or system | Owns |
| --- | --- |
| `tummycrypt` | Protocol, root registry, resolver, clients, packages, and TCFS evidence |
| `lab` | Fleet pins, generated services, secrets custody, endpoints, and attended rollout |
| `rockies` | OS dependency/adoption manifest and Rocky host acceptance |
| `prompts-enqueue` | Rebaselined product prompts and context pointers |
| Linear | Initiative order, issue state, acceptance links, and operator gates |
| GitHub | Reviewable implementation, release tags/assets, CI, and durable code history |

Cross-repository work should be a chain of small, independently reviewable
changes. Tummycrypt defines the contract; lab consumes it; Rockies vendors the
proven package.

## Cleanup rule

Git history is the archive. Current navigation contains only current product
truth, active runbooks, specialized references, and immutable evidence.
Obsolete instruction files are deleted rather than left where an agent can
mistake them for an overlay. Dated evidence stays immutable; dated plans must
either carry an explicit historical banner or leave the active index.
