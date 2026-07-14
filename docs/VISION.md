# TCFS vision: hydratable anywhere

TCFS is being built so the machine you happen to be using does not determine
which files, repositories, prompts, or agent sessions you can continue.
Selected state should be enrollable once, encrypted before it leaves the
device, present as browsable names on every authorized host, hydrated when
needed, and safely unsynced when local space is better used elsewhere.

This is the direction, not a blanket capability claim. Read
[`ops/current.md`](ops/current.md) for the current proof boundary and
[`PRODUCT.md`](PRODUCT.md) for the approved delivery sequence.

## Claim tiers

- **Proven live** — reproduced on real hosts with a dated evidence packet or a
  named live canary.
- **Merged, unproven** — code is on `main`, but the intended live proof has not
  passed.
- **Designed only** — an accepted design exists without landed implementation.
- **Direction only** — desired product behavior without an accepted design.

Every product statement below is bounded by one of these tiers. A passing unit
test does not become a fleet claim, and a successful canary does not
automatically widen from one root or client to every root or client.

## The user experience

The target experience has three ordinary workflows.

### Continue work after SSH

Enroll a Git repository or agent-state directory. SSH from `neo` to `honey`,
`sting`, or another enrolled host, `cd` into the corresponding root, and
continue with the same committed and uncommitted state. Branches, index state,
unstaged edits, untracked files, stashes, prompts, and session content should
not need a manual copy step.

One full repository has a proven `neo → honey` roam, and automatic divergent
keep-both has a two-host live proof. Arbitrary repositories, sessions, and
linked worktrees remain outside the claim.

### Hydrate and release space

Browse remote-backed names without downloading every body. Opening a file
hydrates verified plaintext locally. `unsync` releases the local body while
retaining the remote copy and a usable local representation.

The lifecycle is proven live through Linux FUSE. FileProvider has a bounded
signed macOS lab proof. Plain sync roots, NFS, Windows CFAPI, and iOS do not yet
share one proven lifecycle.

### Remotify selected home state

Choose explicit classes such as agent projects, prompt libraries, selected
dot-directories, and repositories. Apply per-host policies so a directory may
live hot on `honey`, remain hydratable on `neo`, and be absent from another
machine. The product must make scope, hydration state, conflict state, and
recovery visible.

Whole-home takeover is not the design. Selective, reversible enrollment is.
The current fleet only roams one bounded agent subtree and one repository.

## Where TCFS fits

TCFS is one range in a larger remote-first system:

| Program or component | Responsibility |
| --- | --- |
| **Cordillera** | Remote-everything umbrella and cross-project product program |
| **Rockies** | OS composition, host policy, enrollment, and fleet rollout |
| **TCFS** | Encrypted persistent state, roam, hydration, conflict safety, and local unsync |
| **SSH / cmux** | Terminal and live-process transport |
| **IDE routing** | Host-aware editor connection and cwd mapping |
| **GloriousFlywheel** | Remote build and execution substrate |
| **prompts-enqueue** | Prompt and context library; not the file transport or dispatcher |
| **APFS / FUSE / FileProvider / NFS / CFAPI** | Platform substrates and client surfaces |

APFS is therefore neither a TCFS competitor nor proof of TCFS performance. It
is the native macOS baseline and a substrate under client behavior. TCFS owns
the portable encrypted state contract across unlike filesystems and operating
systems.

Rockies adoption remains designed only until it carries a dependency manifest,
a justified Rocky package lane, and real host acceptance. TCFS should be
vendorable by Rockies without making the OS repo the filesystem's source of
truth.

## Product invariants

Breadth is earned only while these remain true:

1. **No silent loss.** Divergence preserves every committed side and exposes
   unresolved ordinary files.
2. **Authenticated mutation.** Read-only inspection may be local; conflict
   execution remains an authenticated daemon operation.
3. **Stable root identity.** A root is not an arbitrary state-file path. A
   trusted daemon descriptor binds a stable root ID to its host-local path,
   remote prefix, state cache, and policy.
4. **Encrypted transport and storage.** Credential-bearing object-store traffic
   must use authenticated TLS; stored content remains client-encrypted.
5. **Reversible enrollment.** Unsync, rehydrate, restore, and rollback are part
   of the acceptance contract.
6. **Explicit scope.** Repositories, agent directories, dotdirs, and home
   classes widen independently through named proofs.
7. **Fail-closed clients.** Unsupported crypto or policy state is an error, not
   ciphertext presented as user content.
8. **Evidence before adjectives.** Packaging, platform, and daily-driver claims
   name the exact packet that supports them.

## Client direction

| Surface | Product role | Current tier |
| --- | --- | --- |
| CLI and daemon | Canonical control and data plane | Proven live on the bounded Linux and fleet lanes |
| Linux FUSE | Transparent browse, hydrate, write, and unsync | Proven live |
| macOS FileProvider | Finder-native placeholders and hydration | Bounded signed lifecycle proven; product UX incomplete |
| NFS loopback | FUSE-free Linux mount option | Merged, unproven |
| Windows CFAPI | Explorer-native placeholders | Designed/skeleton only |
| iOS FileProvider | Files integration | Proof of concept |
| MCP | Agent-readable control surface | Available, but destructive Git resolution is deliberately excluded |

The daemon owns authorization, stable-root routing, storage identity, and
mutation. Clients should not independently reconstruct those security-sensitive
decisions.

## The sequence

The approved product sequence is:

- **A — Trustworthy Beachhead.** Make delivery monotonic, put storage behind
  TLS, add stable root routing, close the live resolver and headless enrollment
  gaps, and prove two repositories end to end on coherent hosts.
- **B — Roam Roots.** Turn the minimal root identity into product-owned
  enrollment, subscriptions, path aliases, and cross-OS mapping, and design the
  linked-worktree reconstruction contract.
- **C — Hydratable Home and client breadth.** Add selective home and agent
  classes, Finder ease, broader hydration/unsync UX, Rocky packaging, Windows,
  iOS, live linked-worktree reconstruction proof, and other client
  integrations.

Strategy A deliberately borrows the stable identity primitive from B. It does
not pull B's broad policy and subscription scope into the beachhead.

## Success

TCFS becomes a daily driver when a fresh authorized host can enroll without a
credential bypass, discover its allowed roots, materialize or hydrate them at
host-appropriate paths, survive concurrent work without loss, release local
space, restore from remote truth, and show evidence that the next reconcile
cycle is clean.

The final test is mundane: SSH in, `cd`, work, disconnect, and continue
elsewhere. No pre-copy. No mystery state file. No special rescue ceremony for
normal operation.

## Pointers

- [Product sequence and gates](PRODUCT.md)
- [Current workstream truth](ops/current.md)
- [Git repo dogfood canary](ops/git-repo-canary-dogfood.md)
- [Repo-roam acceptance](ops/git-roam-daily-driver-acceptance-2026-06-08.md)
- [Distribution smoke matrix](ops/distribution-smoke-matrix.md)
- [Platform support](platform-support.md)
- [RFC 0002: Darwin/FileProvider strategy](rfc/0002-darwin-fuse-strategy.md)
- [RFC 0004: FUSE-free architecture](rfc/0004-fuse-free-architecture.md)
- [Evidence index](release/evidence/README.md)
