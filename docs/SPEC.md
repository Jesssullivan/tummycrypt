# tcfs product spec

Status: **proposed 2026-10-16** (PR "docs(spec): tcfs product spec and
truthing"). Once merged, this file says *what tcfs must do* and *how to tell
it is done*. It does not track live state: [ops/current.md](ops/current.md)
does that. It does not set the delivery order: [PRODUCT.md](PRODUCT.md)
(A → B → C) does that. When this file and another document disagree about
purpose, requirements or acceptance, this file wins. Change it only by a
dated operator ruling.

Each requirement cites the operator's words or selections by entry ID (for
example `PUR-01`) from a private intent ledger. The ledger is kept outside
this public repository because it names machines, sessions and projects.
Entries are labelled by source:

- **operator-typed**: the operator's own words;
- **operator-selection**: the operator picked an option an agent had written;
- **agent paraphrase**: an agent wrote it. This is not an operator
  requirement.

A line that rests only on agent design says so (**design, not an operator
requirement**). Machines appear here by role, not by name.

## What tcfs is for

The operator, 2026-10-15 (PUR-01, operator-typed):

> the real intent of tcfs (files anywhere, no need to copy whillyneilly, the
> real antidote to what bulkload provides asa interm soution [...]

The operator, 2026-07-14 (PUR-02, operator-typed). A 2026-07-15 synthesis
quoted this whole prompt as the canonical operator statement; excerpts:

> Eventually, tummycrypt will be the univeral filesystem- allow user a
> unified FS and hydrate on demand / unsync / integrated / auto traversable
> with paceholder / JIT hydration, etc (similar to and expanding way beyond
> odrive) [...] the machine itself / terminal will melt away.

The operator, 2026-07-17 (PUR-04, operator-typed):

> S3 backed FS suitable for workling accross many heterogenious machines,
> ballowing the constraints of FS / disk size to melt away). this product is
> years deep [...] eventually this migration work wil become completely
> obsolete, as this very housekeeping work is what tcfs seeks to seamlssly
> fix as a entire class of work.

In plain terms: **tcfs is an encrypted, S3-backed, remote-first userspace
filesystem.** A user enrolls a directory once. Every enrolled machine then
sees the same tree, files hydrate when opened and can be released again, and
nobody copies trees between machines by hand. The intended audience is
engineers whose work spans many heterogeneous machines, where "a monolithic
dev machine simple cannot ever cut it" (PUR-03, operator-typed; paraphrased
where the quote names hosts). It is meant to become the userspace filesystem
for the Rockies OS project (PUR-03). It is a long-run product: "years in
development and posibbly yeasr out" (BND-03, operator interview text).

## Requirements

| ID | Requirement | Source |
| --- | --- | --- |
| T1 | **No pre-copy.** Work on an enrolled tree can continue from any enrolled machine with no manual copy step: "without needing to pre-copy over dots, dirs and worktrees" | PUR-01, PUR-05, PUR-06 (operator-typed) |
| T2 | **The same state, including work in progress.** Tracked and untracked files, worktrees, and agent sessions and prompts arrive intact, "just magically available wherever I go" | PUR-03, PUR-05 (operator-typed) |
| T3 | **Hydrate on demand and unsync.** Remote names are browsable before download; opening a file hydrates verified plaintext; unsync releases the local body and keeps the remote copy | PUR-02, PUR-08 (operator-typed) |
| T4 | **Disk size stops mattering.** Large directories expand and contract as cloud offload, so "no machine needs to deal with FS preassure" | PUR-02, PUR-04 (operator-typed). Bounded by T10 for the first-user cohort |
| T5 | **Heterogeneous clients.** macOS, Linux and iOS; Finder and odrive-style parity on macOS | PUR-08, PUR-10 (operator-typed) |
| T6 | **Scope grows to the home directory.** Dot files, home directory and `.local` are the eventual scope, starting from canaries | PUR-07, PUR-06, PUR-09 (operator-typed) |
| T7 | **S3-first storage.** The backend is an S3 object store. The store itself is replaceable | BCK-06 (operator-typed). "Replaceable" rests on BCK-07 (operator-typed) and BCK-02 (operator-selection, Q153): the operator's chosen S3 store has already changed once. The current store is a ruling, not a product property |
| T8 | **Encrypted before it leaves the device, with safe enrollment.** Content is end-to-end encrypted, and machine enrollment is part of the security model: "encryption, enrollment, security, e2ee" | KEY-01, PUR-09 (operator-typed). The stronger "self-enrollment without leaking credentials" wording is REC-02 (agent paraphrase) |
| T9 | **Never lose or silently drop data.** Refuse with a typed reason instead of skipping; fail closed instead of showing ciphertext; never mirror-delete without evidence | **Design, not an operator requirement.** It is consistent with the operator's "drop nothign. ever." for bulkload |
| T10 | **The first-user cohort keeps its local copies pinned until there is a second copy.** As approved: "until 010 replication or a verified off-site restore exists, the first-user cohort keeps its local copies pinned. Nothing is evicted to cloud-only." (`010`: a second replica on another rack.) This covers the first-user cohort only, not all enrolled data. **Open: not yet confirmed as a gate (open decision 9)** | USR-04 (agent paraphrase, approved by selection 2026-10-15) |
| T11 | **No builds on the maintainer's Mac workstation.** macOS builds run on CI or the remote build host | PRC-06 (operator-typed) |
| T12 | **Stay in its lane.** tcfs work is not introduced into unrelated lanes, and migration tooling does not live in this repository | PRC-01, PRC-02, SUP-01 (operator-typed); tummycrypt #601 removed the bulkload crates |
| T13 | **v1 covers Git linked worktrees.** Every linked worktree of an enrolled repository arrives on another enrolled machine with the same HEAD, index and working state, rebuilt from a consistent snapshot. Defined in [Worktrees and live SQLite](#worktrees-and-live-sqlite-t13-t14) | Operator ruling BL2-Q8, 2026-10-27, Linear TIN-3692 comment `36925e2d`: "TCFS v1 must cover them" (operator-selection) |
| T14 | **v1 covers live SQLite stores.** A SQLite database in use on one machine arrives on another as a transactionally consistent whole file, through a lock-aware snapshot and restore. Defined in [Worktrees and live SQLite](#worktrees-and-live-sqlite-t13-t14) | Operator ruling BL2-Q8, as T13 |

## Acceptance

tcfs is accepted in stages. Each stage needs a dated evidence packet under
[release/evidence/](release/evidence/README.md) and a row in
[ops/current.md](ops/current.md).

1. **Backend back.** The new backend meets every precondition in
   [ops/current.md](ops/current.md#status-2026-10-15), and `tcfs health`
   reports `UP` on an enrolled client.
2. **First real user.** The acceptance test in
   [ops/first-user-mac-cohort.md](ops/first-user-mac-cohort.md#acceptance-test)
   passes: inventory, ingest, enumerate, hydrate and verify, evict and
   rehydrate, and the outage drill.
3. **Beachhead (A).** Every gate in [PRODUCT.md](PRODUCT.md) section A is
   green, including the two-repository stop rule.
4. **Launch.** **Design, not an operator requirement; needs a ruling.** tcfs
   counts as launched when stages 1 to 3 pass on a published release (not a
   source build), and a second machine resumes work on an enrolled tree
   with no manual copy (T1). bulkload's own spec says it becomes
   maintenance-only "once tcfs launches", so the word needs one agreed
   meaning.
   Whatever "launch" comes to mean, v1 includes T13 and T14 (ruling BL2-Q8,
   TIN-3692 `36925e2d`): a release that refuses linked worktrees or live
   SQLite is not v1.
5. **Breadth (B, C).** Roam roots, the home directory (T6), writable macOS
   surfaces and the other clients (T5), as sequenced in
   [PRODUCT.md](PRODUCT.md).

## Worktrees and live SQLite (T13, T14)

Ruling: Jess, 2026-10-27, Linear TIN-3692 comment `36925e2d`, BL2-Q8
("Worktrees and live SQLite"), selected answer: "TCFS v1 must cover them".
Until v1 ships, the sync blacklist keeps refusing both classes and
bulkload carries them ([ops/tcfs-bulkload-boundary.md](ops/tcfs-bulkload-boundary.md)).
The mechanism below is **design, not an operator requirement**. The
requirement is the ruling. The failure modes and the tests are what make
"cover" checkable.

### What "cover" means

**Linked worktrees (T13): a consistent snapshot, then a rebuild.**

- tcfs captures each enrolled repository as one snapshot: refs and objects
  (through the existing bundle/ref transport), plus a logical worktree
  record. For each linked worktree the record holds its path relative to an
  enrolled root, HEAD (branch or detached commit), lock state and reason,
  the index content, and the tracked, untracked and enrolled-ignored files.
- The capture reads Git state under Git's own lock protocol (`index.lock`,
  `HEAD.lock`, `packed-refs.lock`, ref locks). If a lock is held, it waits
  for a bounded time and then refuses that repository with a typed reason.
- The destination rebuilds each worktree with Git's own worktree machinery
  at the mapped path, then writes the index and working files from the
  record. It never copies `.git/worktrees/<name>/` admin files or gitfile
  pointers as bytes. The blacklist fences for those paths stay in place.
- Paths are mapped by enrolled-root rules. No rebuilt pointer may name
  another machine's absolute path.

**Live SQLite (T14): a lock-aware snapshot, then a whole-file restore.**

- tcfs detects a SQLite database by its header (`SQLite format 3` plus a
  NUL byte), not only by its file name.
- The capture uses SQLite's online backup API, or `VACUUM INTO`, through
  SQLite's own locking. The snapshot therefore holds every transaction
  committed before it started, including any that still live only in the
  `-wal` file. It uses a bounded busy timeout and yields to writers.
- The snapshot is stored as one immutable, versioned, encrypted object. The
  live main file, `-wal` and `-shm` are never synced chunk by chunk or one
  at a time.
- A restore replaces the destination file as a whole, by an atomic rename,
  and only when no process holds the database open. Otherwise it stages the
  snapshot beside the file and reports a typed conflict.
- Edits on two machines since a common snapshot are kept as two versions.
  tcfs never merges pages and never picks a winner silently.

### Failure modes v1 must not have

| ID | Class | Must not |
| --- | --- | --- |
| F1 | Worktree | Change, prune or remove any worktree or admin area on the source machine |
| F2 | Worktree | Leave a rebuilt worktree whose HEAD, branch, index or `git status` differs from the snapshot |
| F3 | Worktree | Skip a locked, detached, dirty or untracked-only worktree without a typed refusal |
| F4 | Worktree | Let `git worktree prune` on one machine remove a worktree on another |
| F5 | Worktree | Copy an index that was being written (a held `index.lock`) |
| F6 | Worktree | Run `git gc`, `git prune`, `fetch --prune` or any ref deletion as a side effect |
| F7 | Worktree | Leave a pointer or config value that names another machine's absolute path |
| F8 | SQLite | Produce a torn copy: a main file without the committed WAL content it depends on |
| F9 | SQLite | Lose a transaction that committed before the snapshot started |
| F10 | SQLite | Replace a database that a process holds open |
| F11 | SQLite | Mix pages from two versions, or overwrite a newer remote version silently |
| F12 | SQLite | Delete or rewrite the source's `-wal` or `-shm`, or block the source's writers beyond the bounded busy timeout |
| F13 | Both | Leave a partial result at the final path after a crash or a kill; output is staged and renamed |
| F14 | Both | Store any of this state in plaintext in the object store (T8) |

### Tests that prove it

A blacklist entry for linked worktrees or SQLite is removed only in the PR
that adds the matching tests below, and only when they pass. Live rows need
a dated evidence packet under [release/evidence/](release/evidence/README.md).

| ID | Kind | Test | Proves |
| --- | --- | --- | --- |
| W1 | unit | Capture a repository with a main worktree and linked worktrees that are on a branch, detached, locked, dirty and untracked-only. The record round-trips | F3 |
| W2 | integration | Capture on home A, rebuild on home B over an in-process object store. Per worktree, HEAD, branch, `ls-files -s` and the sha256 of `git status --porcelain=v2 -z` are equal. `git worktree list --porcelain` lists each one, and `git fsck --connectivity-only` exits 0 | F2, F7 |
| W3 | integration | After W2 and a `git worktree prune` on B, the sha256 of A's `.git/worktrees` tree and of each source worktree is unchanged | F1, F4 |
| W4 | integration | Hold `index.lock` during capture. The result is a bounded wait or a typed refusal, never the half-written index | F5 |
| W5 | integration | A command recorder sees no `gc`, `prune`, `fetch --prune` or ref deletion. The ref count after is at least the ref count before | F6 |
| S1 | integration | A writer commits a counter row in a loop, in WAL mode and in rollback-journal mode. The restored copy passes `PRAGMA integrity_check` and holds a committed prefix at least as long as the count at snapshot start | F8, F9 |
| S2 | integration | Rows committed only to the WAL, with no checkpoint, are in the snapshot | F8 |
| S3 | integration | The destination database is held open by another process. The restore stages beside and leaves the open file unchanged | F10 |
| S4 | integration | Two homes edit the same database after a common snapshot. Both versions are kept | F11 |
| S5 | integration | The writer's commit latency during a snapshot stays within the busy timeout, and the source's `-wal` and `-shm` are intact afterwards | F12 |
| S6 | unit | A database with no file extension is detected by its header | detection |
| K1 | integration | SIGKILL the capture and the restore at random points. A rerun converges, and the final path never holds a partial result | F13 |
| E1 | unit | The stored snapshot objects decrypt only with the content key. The bucket holds no plaintext of them | F14 |
| L1 | live | A repository with more than 100 linked worktrees, and an agent-state SQLite store in daily use, roam between two enrolled machines. The application opens the store, and every worktree matches W2's checks | T13, T14 |

## Boundary with bulkload

bulkload is the interim mover. It stages work data onto a new machine before
tcfs launches, and its bar is beating rclone on complex, many-file estates
(operator, 2026-10-16: "staging work data pre TCFS launch. thats really it."
BND-04). tcfs is the product that makes that class of work unnecessary
(PUR-04). The two products live in separate repositories and do not share a
chunk store. bulkload's purpose, requirements and acceptance live in
`docs/SPEC.md` in [Jesssullivan/bulkload](https://github.com/Jesssullivan/bulkload),
and its spec outranks anything here about bulkload. The data-class split
(what each product carries today) is in
[ops/tcfs-bulkload-boundary.md](ops/tcfs-bulkload-boundary.md).

## Current status

As of 2026-10-16, origin/main `11fc85e`. Each claim carries its tier from
[VISION.md](VISION.md#claim-tiers).
[ops/current.md](ops/current.md) is the live record.

- **The backend is paused.** It has been at zero replicas since 2026-09-29.
  Data volumes are retained. The old deployment will not be revived: tcfs
  moves to a new SeaweedFS deployment once its preconditions hold. Those are
  identity answers, `filer_backup` with an off-site copy, a fresh unpublished
  JWT signing key, NATS `sync_always` with token auth, and then the storage
  hold is lifted. Installed clients have nothing to sync with.
- **Releases.** The latest release is `v0.12.17` (2026-07-09). `main` is 177
  commits ahead. CI (including cargo-deny) is green. The Nix CI build jobs
  are red.
- **Proven while the backend ran** (proven live, historical): Linux FUSE
  browse, hydrate, unsync and rehydrate; one Git repository roamed between
  two hosts with history and work in progress intact; a bounded agent-session
  subtree roamed; a bounded signed macOS FileProvider lifecycle.
- **Merged, unproven:** single-read ingest with typed refusals; bounded-memory
  streaming hydration; `tcfs health` verdicts and a reconcile that backs off
  during outages; NATS token from a file.
- **Read-only on macOS.** FileProvider rejects every mutation, and NFS
  loopback returns `NFS3ERR_ROFS`. No writable macOS surface exists.
- **Crypto mode.** File keys are wrapped by the shared master key on every
  host (`wrap_mode = master`). Per-device wrapping exists in code but is not
  deployed.
- **Gaps between the documented security model and the code:**
  - **Metadata is not encrypted.** Manifests are stored as plaintext JSON
    that holds the relative path, file size, the plaintext file BLAKE3 and
    the chunk list. Index keys are plaintext relative paths. The manifest-key
    and AES-SIV filename-key primitives in `tcfs-crypto` have no caller.
    Anyone who can read the bucket sees file names, sizes and content hashes,
    but not content.
  - **The OS keychain is not used.** The `keyring` dependency is built
    without a platform backend, so it falls back to its in-process mock
    store, and session tokens do not persist across processes.
  - **No compression.** Chunks are encrypted uncompressed.
    `tcfs-chunks::seekable_zstd` has no caller.
  - **Data-key escrow is not named.** If the content passphrase is lost, the
    bucket is unreadable.
  - **Device secret keys are plaintext on disk.** `save_device_secret_key`
    writes each `device-<id>.age` file as the raw age identity, mode 0600.
    Older text that called the file "age-encrypted" was wrong. Encryption at
    rest is ruled for the next release (BL2-Q10, TIN-3692 `36925e2d`).
- **NixOS and Home Manager options that do nothing:** `deviceName`,
  `conflictMode`, `syncGitDirs`, `gitSyncMode` and `excludePatterns` render
  `TCFS_*` variables that no code reads. The NixOS `syncRoot` and `mounts`
  options are not wired to tcfsd. The Home Manager module also writes the
  literal string `%t` into `daemon.socket`, which tcfsd does not expand.

## Single-source map

One file owns each topic. Other files link to it and do not restate it.

| Topic | Canonical file |
| --- | --- |
| Purpose, requirements, acceptance | `docs/SPEC.md` (this file) |
| North star and claim tiers | [VISION.md](VISION.md) |
| Delivery order (A → B → C), root identity | [PRODUCT.md](PRODUCT.md) |
| Live status, blockers, path back from the pause | [ops/current.md](ops/current.md) |
| First real user: path, gaps, acceptance test | [ops/first-user-mac-cohort.md](ops/first-user-mac-cohort.md) |
| What tcfs carries and what bulkload carries | [ops/tcfs-bulkload-boundary.md](ops/tcfs-bulkload-boundary.md) |
| bulkload purpose and requirements | `docs/SPEC.md` in the bulkload repository |
| The pause and the retired restore path | [ops/tcfs-honey-cluster-pause-2026-09-29.md](ops/tcfs-honey-cluster-pause-2026-09-29.md) |
| Per-client maturity | [platform-support.md](platform-support.md) |
| Evidence index | [release/evidence/README.md](release/evidence/README.md) |
| Architecture, data path, compression status | [ARCHITECTURE.md](ARCHITECTURE.md) and `docs/tex/architecture.tex` |
| Security model and key hierarchy as built | `docs/tex/security.tex` ([SECURITY.md](SECURITY.md) is the summary) |
| Wire and storage formats | `docs/tex/protocol.tex` ([PROTOCOL.md](PROTOCOL.md) is the summary) |
| NATS token-file rules | `crates/tcfs-core/src/nats_token.rs` module docs. Other copies point there |
| Configuration keys | `crates/tcfs-core/src/config.rs`. [`config/tcfs.example.toml`](../config/tcfs.example.toml) is an annotated example |
| Per-device crypto sequencing | [ops/per-device-crypto-migration-2026-06-06.md](ops/per-device-crypto-migration-2026-06-06.md) |
| Build, test, CI, agent rules | [AGENTS.md](../AGENTS.md) |
| Release notes | [CHANGELOG.md](../CHANGELOG.md) |

Dated files under `docs/ops/` and `docs/archive/` are historical records
unless `ops/current.md` promotes them. Working notes go under
`docs/agent-notes/`.

## Open decisions

These are recorded for the operator. Agents do not resolve them.

1. **Escrow for the data key.** The operator chose to name an offline escrow
   reference (Q176). None is recorded yet. It is needed before the backend
   comes back.
2. **Per-device wrapping date.** A "committed by 2026-08-31" decision is
   agent-recorded, and that date has passed. Confirm it with a new date, or
   withdraw it.
3. **Encrypted metadata.** Implement manifest and filename encryption before
   the first user, or accept plaintext paths, sizes and hashes in the bucket
   for that user.
4. **What "launch" means** (acceptance stage 4).
5. **Which copy is authoritative for the first user.** bulkload now carries
   the first-user data to the new machine, and that data is also tcfs's
   first user. Is the carried copy retired once tcfs acceptance passes?
6. **Milestone dates.** The tracker's 2026-10-31 target for stage C predates
   the pause.
7. **Roam-root path.** The canonical mount prefix (`/tcfs/<root_id>` or
   `~/tcfs/<root_id>`): the ruling is recorded in an unmerged draft.
8. **tcfsd `master.key`: carried or re-derived. Decided, not open.**
   bulkload rulings OI-1003-Q185 and OI-1003-Q194 (C7): nothing is excluded
   by content, credentials are carried, and re-authentication happens only
   for a host-bound credential. The tcfsd `master.key` is carried like any
   other file; it is re-derived only if it proves host-bound. Recorded here
   because the ledger raised it.
9. **Pinning gate (T10).** The rule that the first-user cohort keeps its
   local copies pinned was agent-drafted and approved by selection (USR-04).
   Confirm it as an explicit gate on eviction, or withdraw it. It limits the
   cloud offload in T4.
