# TCFS Large Workdir Onboarding Design - 2026-05-25

Status: design recon plus first inventory helper for the next productionization
pass.

Tracker: `TIN-1617`.

This document defines the first claimable version of a TCFS onboarding flow for
large workdirs. It is deliberately narrower than "manage all of `~/git` and
`/tmp` everywhere". The safe target is a trusted, operator-managed pilot that
can enroll selected project trees across Linux and macOS machines without
moving the live source first.

## Problem Statement

The requested user story is roughly:

- a senior systems engineer works across many Rocky and macOS machines
- TCFS should be the overlay for selected `~/git` trees and agent state dirs
- later, perhaps `/tmp`
- onboarding should not require moving files around first
- selective unsync / resync, remove-from-machine, and on-demand rehydrate must
  work across hosts
- enrollment must be secure, encrypted in transit, encrypted at rest in S3,
  and support adding more machines through a controlled join path

The repo already has the building blocks, but not the full claim:

- scoped S3 posture and prefix isolation exist, with TLS enforcement and
  bounded concurrency in the storage operator ([operator.rs](../../crates/tcfs-storage/src/operator.rs),
  [storage-posture-production-gate.md](storage-posture-production-gate.md))
- lazy hydration, cache eviction, and unsync/resync exist for mounted views
  and CLI roots ([hydrate.rs](../../crates/tcfs-vfs/src/hydrate.rs),
  [vfs_lifecycle_test.rs](../../crates/tcfs-vfs/tests/vfs_lifecycle_test.rs))
- real age/X25519 device keys and age-wrapped enrollment bootstrap exist, but
  revocation is not yet cryptographic revocation ([device.rs](../../crates/tcfs-secrets/src/device.rs),
  [grpc.rs](../../crates/tcfsd/src/grpc.rs))
- production Finder/FileProvider lifecycle is proven only for the published
  `.pkg`, not for broad home takeover ([macos-fileprovider-reality.md](macos-fileprovider-reality.md))
- large raw-Git restore is still blocked on package-backed multi-GB proof
  ([git-repo-canary-dogfood.md](git-repo-canary-dogfood.md),
  [storage-posture-production-gate.md](storage-posture-production-gate.md))

## Claim Boundary

TCFS can claim:

- selected project-tree onboarding for trusted named testers
- on-demand hydration and selective unsync / remove-from-machine
- cross-host browse, hydrate, edit, and rehydrate for shadow roots
- scoped S3-backed sync with TLS and prefix isolation
- secure local enrollment for real devices

TCFS cannot yet claim:

- broad `~/git` ownership
- primary home-directory takeover
- broad `~/Documents` or `.local` ownership
- broad dotdir takeover
- `/tmp` ownership
- self-service join for arbitrary users
- cryptographic lost-device revocation
- production Finder readiness for arbitrary hosts
- full multi-GiB package-backed restore and soak for large repos

## Design Goal

Make the smallest useful onboarding system that supports a real workflow:

1. inventory candidate roots without mutating them
2. build isolated shadows and prove the lifecycle there first
3. let trusted users expand to one expendable live repo
4. only then grow to selected `~/git` subtrees and agent state dirs
5. leave `/tmp` as a later, special-case root with explicit churn controls

## Architecture

```text
candidate roots
  -> inventory only
  -> shadow copy
  -> tcfs config + state + policy
  -> sync engine
  -> scoped S3 prefix
  -> remote peer / other machine
  -> rehydrate / unsync / conflict resolution
```

The core contract is that TCFS owns representation and state, not the source
tree itself. The live source stays put until the pilot proves the behavior.

## Phased Rollout

| Phase | Scope | Success criteria | Still not allowed |
| --- | --- | --- | --- |
| 0. Inventory | Read-only scan of selected roots | Count files, dirs, symlinks, hidden dirs, `.git`, xattrs, special files, dirty Git state, and unsupported paths | Any mutation |
| 1. Shadow pilot | Isolated shadow copy of one large workdir | Traverse before hydrate, selective hydrate, cache clear / rehydrate, clean unsync, dirty refusal, exact rehydrate | Live source mutation |
| 2. Single live repo | One expendable repo on two machines | Same as shadow plus cross-host edit / pullback and rollback | Broad `~/git` ownership |
| 3. Selected subtrees | Chosen `~/git` subtrees and agent dirs | Stable root policy, remove-from-machine semantics, conflict UX, repeatable joins | Whole home takeover |
| 4. `/tmp` pilot | Churn-heavy special root | TTL / cap policy, explicit exclusions, special-file handling | Unbounded persistence |

## Security Model

### Enrollment

Use a split between local setup and fleet join:

- `tcfs init` stays the local first-run flow
- `tcfs enroll` or an explicit join flow handles invite-based fleet entry

The implementation basis is already present:

- local real device keys are generated and stored `0600`
  ([device.rs](../../crates/tcfs-secrets/src/device.rs))
- the daemon can wrap bootstrap material to the joining device's public key
  ([grpc.rs](../../crates/tcfsd/src/grpc.rs))
- invites already have single-use redemption and admin/session gates
  ([tcfs-daily-driver-productionization-todo-2026-05-24.md](tcfs-daily-driver-productionization-todo-2026-05-24.md))

Required before broad self-service claims:

- per-device content-key wrapping
- revocation denies new content
- signed device registry or equivalent authority model
- bootstrap persistence on the client side
- join approval / pairing UX that does not expose raw long-lived storage secrets

### Storage

Use scoped prefixes, TLS, and per-environment credentials:

- `storage.enforce_tls = true`
- dedicated bucket or scoped prefix
- machine-checkable deny prefix for proof packets
- public CA or configured private CA

The storage posture is already implemented at the operator layer and proved in
the storage gate docs ([operator.rs](../../crates/tcfs-storage/src/operator.rs),
[storage-posture-production-gate.md](storage-posture-production-gate.md)).

### Encryption

Current state:

- content encryption exists
- manifests still expose metadata that is not fully E2EE
- local cache stores decrypted assembled files
- file-level concurrent upload is disabled when encryption is present

That means the onboarding story can say "encrypted content sync" but not "all
metadata is hidden" or "encryption is free at large scale".

## Onboarding Flow

### 1. Inventory

Record:

- path
- size
- file count
- dir count
- symlink count
- hidden dir count
- `.git` and Git state
- xattrs and mode coverage
- special files and unsupported entries
- dirty / clean state

This step is read-only. It should produce a machine-readable manifest and a
human summary. Nothing is synced yet.

### 2. Shadow Copy

For the first proof, the system creates a shadow under a dedicated pilot root,
not inside the real source tree. This lets us prove:

- traverse without hydration
- hydrate on demand
- remove-from-machine and rehydrate
- dirty unsync refusal
- clean recursive unsync
- exact cross-host rehydrate
- symlink parity or explicit unsupported blocker

### 3. Live Repo

Only after package-backed large restore is green should the system move to one
expendable live repo. That proof must include rollback and fresh-tree restore.

### 4. Broader `~/git`

Only selected subtrees should be enrolled. Broad takeover remains out of claim
until stable root identity, policy UX, and large-object proof are all in place.

### 5. Agent Dirs

Agent dotdirs are attractive but high-risk. Treat them as a separate root class
with an explicit allowlist, because they often contain credentials, caches, and
local state that users do not expect to lose or rehydrate casually.

### 6. `/tmp`

`/tmp` is not a normal workdir. If TCFS enrolls it later, it needs explicit TTL
and churn handling, special-file rules, and a strict size cap. Do not fold it
into the general onboarding path.

## UX Contract

The user-facing verbs should be stable and plain:

- `browse`
- `hydrate`
- `unsync`
- `remove from machine`
- `resync`
- `pin` or `keep synced`

The product still needs a real pin / keep-synced model. Until that exists, the
phrase should remain a planning term, not a release claim.

## QA Matrix

| Row | Scenario | Required proof |
| --- | --- | --- |
| T1 | `ls/find` before hydration | Enumerate remote names without hydrating all bodies |
| T2 | `cat` on demand | Exact bytes hydrate on first open |
| T3 | cache clear / evict / rehydrate | Second read returns exact latest content |
| T4 | clean file unsync | Stub or placeholder state is correct |
| T5 | clean directory unsync | Recursive behavior is deterministic |
| T6 | dirty unsync refusal | No silent local data loss |
| T7 | force unsync | Force path is explicit and auditable |
| T8 | peer edit while unsynced | First machine rehydrates latest remote bytes |
| T9 | peer delete / rename while unsynced | Old path fails deterministically, new path hydrates exact bytes |
| T10 | same-file conflict | Conflict state is visible and local bytes are preserved |
| T11 | keep-both recovery | Both versions can be preserved and rehydrated |
| T12 | symlink parity | Preserve or explicitly fail with a recorded blocker |
| T13 | xattrs / modes | Values round-trip or remain documented as unsupported |
| T14 | large file | Streaming behavior is verified without memory blow-up |
| T15 | multi-machine soak | Multiple hosts can stay in sync across repeated operations |

The current repo already covers many of these as lower-level tests and archived
packets ([vfs_lifecycle_test.rs](../../crates/tcfs-vfs/tests/vfs_lifecycle_test.rs),
[multi_machine_sim.rs](../../crates/tcfs-sync/tests/multi_machine_sim.rs),
[lazy-traversal-qa-permutation-matrix-2026-05-09.md](lazy-traversal-qa-permutation-matrix-2026-05-09.md)).

### Onboarding Pilot Rows

The large-workdir onboarding plan should explicitly inherit the following rows
from the existing matrix instead of redefining them:

| Pilot | Minimum rows that must be called out | Why |
| --- | --- | --- |
| `TIN-1619` shadow pilot | T1, T2, T3, T4, T5, T6, T12, plus the Linux-mounted multi-machine rows M1, M2, M3, M6 | This is the smallest claimable surface for browse, `cat`, cache clear, clean unsync, symlink truth, and cross-host rehydrate without live-source ownership. |
| `TIN-1620` expendable live repo | T1, T3, T4, T5, T6, T10, T11, plus M3, M5, M5-R, M6, M8 | This is the first live-source step, so it must prove exact content, safe unsync / resync, conflict recovery, and peer edit/delete/rename behavior before anything broader is implied. |

Rows that should stay explicitly out of claim for this onboarding plan:

- M4 / M4-L reverse-read behavior, because the neo/macOS mounted reverse-read
  path remains open
- Finder production rows F1-F7, except as separate package-backed macOS proof
  outside this onboarding track
- T7, T13, T14, and T15, because they are force-unsync, metadata/perf/soak
  breadth rows rather than the minimal onboarding bar
- keep-synced / pin semantics until they are a named product contract
- broad `~/git`, `~/Documents`, `.local`, dotdir, or `/tmp` ownership

## Deliverable Map

| Workstream | What it closes | Why it matters |
| --- | --- | --- |
| `TIN-1617` | selected large-workdir onboarding pilot from inventory to shadow to one live repo | ties storage, first-run, enrollment, selective sync, and root identity into one claimable user workflow |
| `TIN-1618` | read-only large-workdir inventory packet | prevents onboarding unknown filesystem shapes or mutating the source tree before proof |
| `TIN-1619` | selected large-workdir shadow pilot packet | proves browse/hydrate/unsync/rehydrate/conflict behavior without live-source ownership |
| `TIN-1620` | one expendable live repo two-machine pilot | creates the first live-source claim while keeping rollback and scope narrow |
| `TIN-1546` | package-backed multi-GiB restore, soak, and storage posture breadth | required for large live repo onboarding |
| `TIN-1417` | per-device keys and per-device file-key wrapping | required for real lost-device and trust boundaries |
| `TIN-1424` | pairing and admin-gated enrollment | required for safe multi-machine onboarding |
| `TIN-1425` | first-run wizard | required for no-editor onboarding |
| `TIN-1416` | selective sync / pin semantics | required for remove-from-machine and keep-synced UX |
| `TIN-1556` | stable root identity and broad-directory ownership | required for selected `~/git` expansion |
| `TIN-1419` | streaming large-file IO | required for big repos and encrypted workdirs |
| `TIN-1420` | xattrs and metadata replay | required for real filesystem parity |
| `TIN-1549` | status, progress, conflict, and recovery UX | required for users to trust the surface |

## Open Decisions

1. Should the first live step be one expendable repo or one agent-state dir?
   The repo path is more valuable as proof, the agent-dir path is cheaper.
2. Should `keep synced` be implemented as a true pinned policy or as a higher
   level policy alias over `Always`?
3. Should `/tmp` be excluded entirely from the first product release?
4. Should broad `~/git` remain a subtree opt-in forever, or is the long-term
   goal a policy-driven home overlay with explicit exclusions?

## Recommended Next Step

Start with the inventory and shadow-pilot packet for one large repo, and keep
the user-facing claim narrow until the package-backed restore and revocation
story are both proven. That is the smallest version that still gets real work
done without overclaiming.

## Implementation Seed

PR `#462` adds `scripts/large-workdir-inventory.py`, a read-only packet
generator for `TIN-1618`.

Task entrypoints:

- `task lazy:large-workdir-inventory` for the read-only inventory packet
- `task lazy:large-workdir-onboarding` for the default shadow-first packet,
  which currently points at `~/git/linux-xr-fast`

Outputs:

- `inventory.json`
- `inventory.env`
- `summary.md`

The first packet records total bytes, entry counts, regular files,
directories, symlinks, hidden directories, special files, xattr probe results,
scan errors, Git presence/dirty state, unsupported entries, and a conservative
recommendation such as `shadow_pilot_ready`, `shadow_pilot_only_dirty_git`, or
`blocked_special_files`.

Validation:

```bash
python3 scripts/test-large-workdir-inventory.py
task lazy:test-large-workdir-inventory
just large-workdir-inventory-test
```

## Packet Shape

Keep the packet layout boring and close to the existing canary evidence
structure. Reuse the same naming style as the current `home-canary-linux-xr`
and `git-repo-canary` lanes so the archive is easy to compare.

For the first shadow pilot, use `/Users/jess/git/linux-xr-fast` as the primary
workdir when host headroom allows. If the host cannot hold the full stress
packet yet, use `/Users/jess/git/oauth-mux` only to verify the command path and
evidence layout. The packet should still stay shadow-first and must not mutate
the live source tree.

### `TIN-1618` Inventory Packet

Inputs:

- candidate root path
- optional `--skip-xattrs` for speed on hosts where xattr probing is noisy
- optional `--max-unsupported` cap for retaining unsupported paths in the JSON

Outputs:

- `inventory.json`
- `inventory.env`
- `summary.md`
- optional `source-inventory/` folder when the packet is embedded in a larger
  canary directory

Required evidence:

- source root recorded read-only
- total bytes, counts, symlink count, hidden directory count, and special file
  count
- Git presence and dirty / clean state
- xattr probe state
- scan errors and unsupported entries
- recommendation bucket such as `shadow_pilot_ready` or
  `blocked_special_files`

### `TIN-1619` Shadow Pilot Packet

Preferred shape:

- source inventory archived first
- isolated shadow root under `~/TCFS Pilot/real-canaries/`
- disposable remote prefix under the existing evidence namespace
- state and config rooted under the packet directory, not under the live source

Concrete command path:

```bash
run_id="large-workdir-<UTC>"
evidence_dir="docs/release/evidence/${run_id}"

python3 scripts/large-workdir-inventory.py \
  /Users/jess/git/linux-xr-fast \
  --out-dir "${evidence_dir}/source-inventory"

TCFS_HOME_CANARY_SOURCE=/Users/jess/git/linux-xr-fast \
TCFS_HOME_CANARY_EVIDENCE_DIR="${evidence_dir}" \
TCFS_HOME_CANARY_SHADOW_ROOT="$HOME/TCFS Pilot/real-canaries/linux-xr-fast-shadow-<UTC>" \
TCFS_HOME_CANARY_REMOTE="seaweedfs+https://<host>/<bucket>/tcfs/${run_id}" \
TCFS_HOME_CANARY_STATE_DIR="${evidence_dir}/state" \
TCFS_HOME_CANARY_PUSH=1 \
TCFS_HOME_CANARY_RUN_HONEY=1 \
TCFS_HOME_CANARY_RUN_LINUX_LIFECYCLE=1 \
TCFS_HONEY_START_MOUNT=1 \
scripts/home-canary-linux-xr-shadow.sh
```

Packet outputs to archive:

- `source-inventory/inventory.json`
- `source-inventory/inventory.env`
- `source-inventory/summary.md`
- `source-inventory/root.txt`, `counts.env`, `tree-maxdepth-12.txt`,
  `symlinks.txt`, `symlink-targets.tsv`, `hidden-dirs.txt`,
  `unsupported-special-files.txt`, `git-status-porcelain.txt`,
  `git-remotes.txt`, and `git-summary.env`
- `shadow-inventory/` with the same inventory files
- `shadow-copy.log`
- `selected-hydration-file.txt`, `selected-hydration-file.content`, and
  `selected-hydration-file.sha256`
- `push.log` or `push.log.gz`
- `push-storage-summary.env` and `push-storage-summary.md`
- `push-run-metadata.env`, `run-metadata.env`, `result.env`, and
  `parity-gates.env`
- `honey-linux-xr-shadow.log`, `honey-mount.log`, and
  `honey-linux-xr-shadow-commands.txt`
- `linux-lifecycle-companion.log` and `linux-lifecycle/`

First execution packet archived at
`docs/release/evidence/large-workdir-20260526T000907Z/`. That run proves the
inventory + isolated shadow shape and leaves push/honey/lifecycle pending until
the local backend stack is available.

Required proof rows:

- browse before hydrate
- selected file hydrate
- mounted or CLI `ls` / `cat`
- cache clear and exact rehydrate
- clean file unsync and recursive clean unsync
- dirty unsync refusal
- peer edit while unsynced
- peer delete / rename while unsynced
- symlink parity or explicit blocker
- `parity-gates.env` reports `scoped-project-tree-parity-evidence-complete`
- `push.log` does not contain skipped symlink rows
- no unsupported special files remain unaddressed

### `TIN-1620` One Expendable Live Repo Packet

Only after the shadow packet is boring and the acceptance rows above are
proven:

- choose one live repo the operator can lose temporarily
- keep the live source repo immutable until the packet starts, then treat the
  live tree as expendable only for the duration of the pilot
- run the same two-machine browse / hydrate / unsync / rehydrate pattern using
  the repo-local equivalent of `task lazy:git-repo-canary`
- archive a matching restore / rollback packet with
  `task lazy:git-repo-restore-proof`
- capture live-source inventory, shadow inventory, remote prefix, config,
  state, and command transcript under the same evidence naming convention as
  the current git-repo canary packets

Packet evidence must include:

- source inventory before any mutation
- exact source and shadow paths
- remote prefix and restore root
- per-host command transcripts for `ls`, `find`, `cat`, `unsync`, `resync`,
  and `sync-status`
- peer-edit and peer-delete/rename observations while the first host remains
  unsynced
- rollback proof showing the repo can return to a clean, exact tree after the
  live-source experiment

Claim boundary for `TIN-1620`:

- still not broad `~/git`
- still not home-directory takeover
- still not production Finder readiness
- still not `keep synced` / pin semantics
- still not `/tmp`
- still not a general self-service onboarding story for arbitrary users

### Evidence Directory Convention

Use a timestamped evidence directory under `docs/release/evidence/` and keep
the familiar subdirectories:

- `source-inventory/`
- `shadow-inventory/`
- `push/`
- `honey/`
- `lifecycle/`
- `restore-proof/`

The exact packet may omit subdirectories that do not apply, but the naming
pattern should stay stable so the archive compares cleanly across runs.
