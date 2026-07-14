# TCFS product sequence and root-identity decision

Status: **accepted 2026-07-14**. Strategy **A → B → C** is the product order.
This record was grounded against tummycrypt `origin/main` at
`21f8df303596d1b9f6f90cc7953eb8f65f353ac3`, the live Linear/GitHub lanes,
and the named fleet evidence in [`ops/current.md`](ops/current.md).

## Decision

TCFS will first make the bounded `neo`/`honey` beachhead trustworthy. It
will then generalize enrolled roam roots. Only after those two stages pass
their evidence gates will it widen into hydratable home state and client
breadth.

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

The accepted minimal design is a versioned set of trusted, daemon-owned root
descriptors rendered through configuration. Ordinary reconcile and resolve
clients may select an ID; they may not create or rewrite its tuple.

```text
stable root_id
    ├── local path       (host-specific, absolute)
    ├── remote prefix   (shared convergence identity)
    ├── state cache     (daemon-owned location)
    └── policy/profile  (raw Git, hidden paths, deny-set)
```

The same `root_id` may map to different local paths on macOS and Linux. Its
remote prefix is the fleet-wide convergence identity. The descriptor is local,
contains no credentials, and is loaded from the daemon's trusted configuration.

Required invariants:

1. Root IDs use a bounded, validated slug and cannot contain path traversal.
2. State files are configured through the trusted descriptor and normalized by
   the daemon; an RPC never accepts an arbitrary state path.
3. The daemon reloads the selected registered state and uses its registered
   prefix for Git and ordinary-file resolution.
4. The requested path must be contained by the registered local root after
   symlink-aware validation.
5. Reconcile and resolve take the same state-adjacent operation lock so two
   processes cannot overwrite the same JSON state cache.
6. Session authentication, the `push` permission, registered-prefix
   authorization, the Git corruption fence, encryption context, and
   undo-bundle rules remain in the daemon path. The existing `operator_cli`
   request hint and MCP refusal are defense in depth, not an authorization
   boundary by themselves.
7. Dry-run and execute report the root ID, registered prefix, and plan summary
   so evidence proves which trusted root was touched.

The existing scratch implementation that opens a caller-selected cache and
runs keep-both inside the CLI is therefore a research artifact, not a landing
candidate. It bypasses daemon session authentication, cannot make `--root`
select the correct prefix by itself, and only handles the Git group while
ordinary file conflicts remain.

### Minimal A surface

Status on 2026-07-14: PR #551 implements named conflict inspection and the
bounded Git keep-both resolve path. It does **not** implement
`reconcile --root`, named-root ordinary-file resolution, or Lab
enrollment/rendering. Those lifecycle surfaces remain staged under TIN-2859
and its B0a/B0b/B0c children.

```bash
# Source-proven by PR #551
tcfs conflicts --root git-roam-tool-daemon
tcfs resolve --root git-roam-tool-daemon \
  . --strategy keep-both
tcfs resolve --root git-roam-tool-daemon \
  . --strategy keep-both --execute

# Staged B0b surface; not implemented yet
tcfs reconcile --root git-roam-tool-daemon --execute
```

Lab must eventually render the existing `extraReconcileRoots` tuple into
daemon config. Today `resolve --root` selects the daemon-trusted tuple and
`conflicts --root` is read-only; `reconcile --root` remains staged. A future
privileged root-add/update surface belongs to B. Explicit state-file inspection
may remain a diagnostic command, but it is not a mutation route.

## TIN-2658 closure ceremony

After the root-routing build is deployed to both hosts:

1. Freeze the named root's scheduled reconcile on `honey`.
2. Record the root descriptor, version, Git status, conflict groups, state
   cache fingerprint, and remote prefix.
3. Verify TOTP and obtain the short-lived session through the normal SSH path.
4. Run root-targeted keep-both dry-run and preserve its plan.
5. Execute the same plan while the root lock is held.
6. Resolve or explicitly preserve every ordinary-file conflict; do not call a
   Git-only result full convergence.
7. Re-enable reconcile and capture the first convergence cycle.
8. Capture a second cycle with zero conflicts and unchanged intended content.
9. Run `git fsck`, verify parked peer refs and undo bundle, and compare tracked
   bytes on both hosts.
10. Attach the packet to TIN-2658 and only then close the issue.

The six-digit TOTP code should be generated immediately before steps 3–5; it is
not needed while the root target is still ambiguous.

## B — Roam Roots

Once A is green, make root registration a product surface:

- enrollment and removal with explicit policy classes;
- fleet subscriptions and host allowlists;
- stable aliases for unlike local paths;
- root discovery and status in CLI, TUI, and safe MCP reads;
- cross-OS cwd mapping for SSH/IDE routing;
- generation-aware migrations and recovery;
- the linked-worktree reconstruction design and migration contract rather than
  blind roaming of Git pointer files; live support and proof remain in C;
- policy-driven pin, hydrate, unsync, and restore.

B succeeds when a fresh host can discover its authorized roots, map them to
valid local paths, hydrate one, work, unsync it, and recover without editing a
unit file or copying a state cache.

## C — Hydratable Home and client breadth

Then widen the classes and clients:

- agent sessions, prompts, and selected dot-directories;
- repository collections and user-chosen home subtrees;
- live linked-worktree reconstruction and recovery proof;
- polished Finder/FileProvider status, progress, recovery, and first run;
- NFS parity and a justified FUSE-free Linux story;
- Windows CFAPI and iOS lifecycle proofs;
- Rocky 10/10.1 RPM packaging, service policy, upgrade, rollback, and vendor
  acceptance through Rockies;
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
