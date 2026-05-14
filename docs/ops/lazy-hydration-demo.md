# Lazy Hydration Demo Acceptance

As of May 13, 2026, the core lazy traversal and hydration code exists and the
repo has named harnesses for Linux terminal, mounted-view, Desktop-to-honey,
and macOS Finder/FileProvider proof. Evidence coverage is now split more
precisely: PZM proves the macOS testing-mode FileProvider enumerate/hydrate
path through evict/rehydrate/mutation, Linux has archived FUSE-capable host
evidence for read, mounted write, cache clear/rehydrate, and recursive
safe-unsync, and production Finder lifecycle proof remains separate from
non-production testing-mode proof.

This runbook is the acceptance target for the persistent demo goal:

1. Traverse a remote-backed tree before file contents are hydrated.
2. `cd` and `ls` directories whose entries are represented by remote index data.
3. `cat` a remote-backed file and observe content hydrate successfully.
4. Dehydrate or unsync the item and observe a clean user-facing state.
5. Repeat the same idea through Finder/FileProvider on macOS.

## Representation Contract

tcfs has three related representations. Demo scripts and docs should name them
explicitly instead of calling all of them "stubs":

| Surface | User-facing name | Backing representation |
|---------|------------------|------------------------|
| Mounted VFS/FUSE/NFS | clean filenames such as `notes.txt` | remote `{prefix}/index/...` entries plus local cache hydration |
| Physical sync root / CLI unsync | `.tc` / `.tcf` files | sorted key/value stub metadata on disk |
| macOS FileProvider | Finder placeholders / APFS dataless files | FileProvider items that fetch content on demand |

The desired mounted UX is clean names, not raw `.tc` suffixes. `.tc` remains the
physical offline/dehydrated representation for sync roots and compatibility.

## Backend Decision

The demo backend default is a disposable S3-compatible endpoint with a
dedicated per-run prefix. For local Linux terminal proof, the docker-compose
SeaweedFS stack is acceptable. For GitHub-hosted macOS/Finder proof, the
endpoint must be publicly reachable from the runner.

Do not make the lazy traversal demo depend on the on-prem TCFS authority by
default. The on-prem OpenTofu/storage migration is a separate operational lane
with downtime, retained-PVC, candidate-service, and rollback gates. It becomes
an acceptable demo backend only after the migration is complete or an operator
intentionally chooses a self-hosted runner that can reach that private endpoint
and records that network assumption with the evidence.

Each demo run should use a unique remote prefix such as
`lazy-demo/${date-or-run-id}` so seed data can be inspected or removed without
colliding with real user state.

Project-tree correctness and storage posture are separate acceptance lanes.
The scoped isolated `linux-xr` shadow packet
`docs/release/evidence/home-canary-linux-xr-shadow-20260511T040325Z/` proves
large-tree traversal, selected hydration, mounted symlink target preservation,
and the Linux lifecycle companion on a copied project tree. The
current storage-posture packet
`docs/release/evidence/home-canary-linux-xr-storage-posture-20260514T021513Z/`
records fresh-prefix file upload concurrency, retry/timeout telemetry, socket
sampling, and the raw Git `.pack`/`.rev` object-count fixes on a completed
7.7 GB push. A follow-up against the same prefix passed honey mounted
`find -maxdepth 8`, all 85 mounted symlink target checks, and exact
`.clang-format` hydration. The later exact `.tc` filename follow-up
`docs/release/evidence/home-canary-linux-xr-storage-posture-tc-extfix-20260514T202343Z/`
dropped mounted S3 `NoSuchKey` warnings from 274 to 0 while preserving real
linux-xr ftrace `.tc` filenames. The later lifecycle companion
`docs/release/evidence/home-canary-linux-xr-storage-posture-lifecycle-20260514T213826Z/`
reused that same prefix and passed mounted traversal/hydration, all 85 symlink
targets, mounted write/readback, cache clear/rehydrate, dirty recursive
safe-unsync refusal, and clean recursive safe-unsync success. The endpoint was
still plaintext tailnet HTTP, so do not use that packet to claim production S3
posture or broad `~/git`/home-directory readiness.

## Linux Terminal Acceptance

Use a real S3/SeaweedFS-compatible backend or an explicit disposable backend,
not only an in-memory mock.

For the full Linux terminal lifecycle lane, use the Linux-only harness:

```bash
scripts/lazy-hydration-linux-lifecycle-demo.sh \
  --remote seaweedfs://localhost:8333/tcfs/lazy-demo-manual \
  --create-bucket \
  --evidence-dir docs/release/evidence/lazy-linux-$(date -u +%Y%m%dT%H%M%SZ)
```

The same harness is exposed through the task surface:

```bash
TCFS_LAZY_DEMO_REMOTE=seaweedfs://localhost:8333/tcfs/lazy-demo-manual \
TCFS_LAZY_DEMO_CREATE_BUCKET=1 \
TCFS_LAZY_DEMO_EVIDENCE_DIR=docs/release/evidence/lazy-linux-manual \
task lazy:linux-lifecycle-demo
```

The harness seeds a fixture into a dedicated remote prefix, forces a direct
mount with a temp config, runs `find` before hydration, cats a known
remote-backed file, verifies cache hydration, writes and edits through the
mounted view, verifies exact remote pullback, clears the mount cache as the
mounted-surface dehydration step, cats again to prove rehydration, and proves
recursive safe-unsync refusal/success against the physical sync root. It
requires Linux, `/dev/fuse`, `fusermount3` for the default FUSE backend, S3
credentials, and a pre-existing bucket unless `--create-bucket` can create it
with `aws`, `s5cmd`, or `mc`. The legacy `task lazy:linux-demo` entrypoint
uses the same harness. The task surface also accepts
`TCFS_LAZY_DEMO_CREATE_BUCKET=1` and `TCFS_LAZY_DEMO_BACKEND=nfs`.

When `--evidence-dir` or `TCFS_LAZY_DEMO_EVIDENCE_DIR` is set, the harness
writes a transcript, redacted run metadata, final result status, remote prefix
file, `tcfs.toml`, mount log copy, mounted-write pull log, and unsync status
logs. The metadata intentionally records endpoint, bucket, prefix, backend,
and command shape, but not S3 credentials.

Archived host evidence:

- [lazy-linux-20260508T170825Z](../release/evidence/lazy-linux-20260508T170825Z/)
  ran on `honey` through
  `nix develop --accept-flake-config --command task lazy:linux-lifecycle-demo`
  against
  `seaweedfs://100.64.48.53:8333/tcfs/lazy-linux-20260508T170825Z`.
- The run passed with `status=0`, mounted through FUSE3, listed the nested
  remote tree before content hydration, hydrated exact 77-byte content with
  `cat`, wrote and edited through the mounted view, pulled the edited file
  back from remote with exact 60-byte content, cleared the cache to 0 entries,
  rehydrated the original fixture, refused recursive `unsync` for a dirty
  descendant, then converted clean tracked descendants to `.tc` stubs with
  `sync state: not_synced`.
- [lazy-linux-20260508T151858Z](../release/evidence/lazy-linux-20260508T151858Z/)
  ran on `honey` through `nix develop --command task lazy:linux-demo` against
  `seaweedfs://100.64.48.53:8333/tcfs/lazy-linux-20260508T151858Z`.
- The run passed with `status=0`, mounted through FUSE3, listed the nested
  remote tree before content hydration, hydrated exact 77-byte content with
  `cat`, cleared the cache to 0 entries, then rehydrated the same file.
- A first ambient-host attempt failed before product proof because honey's
  non-dev-shell Rust was too old for current async dependencies. The archived
  passing run is the repo-pinned toolchain run.

For an already-mounted surface, the repo carries a small mounted-view smoke
helper:

```bash
scripts/lazy-hydration-mounted-smoke.sh \
  --mount-root /path/to/tcfs-mount \
  --expected-file docs/example.txt \
  --expected-content-file /path/to/expected-example.txt \
  --expect-entry docs
```

The same check is exposed through the repo task surface:

```bash
MOUNT_ROOT=/path/to/tcfs-mount \
EXPECTED_FILE=docs/example.txt \
EXPECTED_CONTENT_FILE=/path/to/expected-example.txt \
EXPECT_ENTRIES=docs \
task lazy:mounted-smoke
```

This helper intentionally does not seed storage, start `tcfsd`, or perform the
mount. It verifies the user-facing part of the demo: clean `ls`/`find` names and
`cat` hydration of a known remote-backed file.

Large-tree Linux FUSE mounts default to plain `readdir` so remote traversal does
not force per-entry attribute reads. Set `TCFS_FUSE_FORCE_READDIRPLUS=1` only
when explicitly debugging a kernel/client that needs `READDIRPLUS` behavior.

## Same-Fixture neo/honey Rehydrate

The first high-value cross-machine permutation is narrower than a full home
rollout: one fixture is pushed by `neo`, removed locally with `tcfs unsync`,
mutated by `honey` through a mounted clean-name view, then pulled back by `neo`.
The required result is exact honey bytes on neo and no stale adjacent `.tc`
stub after the pull.

Use the bounded helper:

```bash
PUSH=1 \
RUN_HONEY=1 \
HONEY_START_MOUNT=1 \
REMOTE=seaweedfs://HOST:8333/tcfs/unsynced-rehydrate-manual \
task lazy:neo-honey-unsynced-rehydrate-plan
```

The helper creates an isolated root under `~/TCFS Pilot/runs/`, writes a
disposable config/state directory, emits manual honey commands, and archives
evidence under the selected evidence directory. It refuses direct `HOME`,
`~/Documents`, and `~/git` roots unless `ALLOW_REAL_ROOTS=1` is explicitly set.

This lane proves a specific QA row: "remove from this machine" on neo, edit
elsewhere on honey, then rehydrate exact latest content on neo. It complements
the broader fleet pilot and does not claim production Finder behavior.

Archived host evidence:

- [neo-honey-unsynced-rehydrate-20260510T015644Z](../release/evidence/neo-honey-unsynced-rehydrate-20260510T015644Z/)
  ran against
  `seaweedfs://100.64.48.53:8333/tcfs/neo-honey-unsynced-rehydrate-20260510T015644Z`.
- The run passed with `status=0`: neo unsynced the fixture to a `.tc` stub,
  honey listed/cat-hydrated/mutated the clean mounted path, neo pulled exact
  honey bytes, `sync-status` returned `sync state: synced`, and
  `stub_after_pull=absent`.

The reverse row uses a honey physical sync root instead of the honey mounted
mutation used by M3:

```bash
PUSH=1 \
RUN_HONEY=1 \
REMOTE=seaweedfs://HOST:8333/tcfs/reverse-unsynced-rehydrate-manual \
task lazy:neo-honey-reverse-unsynced-rehydrate-plan
```

That helper seeds from neo, pulls and unsyncs on honey, mutates and pushes on
neo, then runs a honey pull and checks exact neo bytes plus absent stale `.tc`.

Archived reverse evidence:

- [neo-honey-reverse-unsynced-rehydrate-20260510T022858Z](../release/evidence/neo-honey-reverse-unsynced-rehydrate-20260510T022858Z/)
  ran against
  `seaweedfs://100.64.48.53:8333/tcfs/neo-honey-reverse-unsynced-rehydrate-20260510T022858Z`.
- The run passed with `status=0`: honey pulled the initial fixture, unsynced it
  to a `.tc` stub, neo pushed the mutation, honey pulled exact 107-byte neo
  content, `sync-status` returned `sync state: synced`, and
  `stub_after_pull=absent`.
- [neo-honey-reverse-unsynced-rehydrate-20260510T022657Z](../release/evidence/neo-honey-reverse-unsynced-rehydrate-20260510T022657Z/)
  is retained as blocker evidence: it pulled the new content and reported
  synced, but an older honey `tcfs` binary left the adjacent `.tc` stub behind.

## Mounted Reverse Read

M4 is different from M6: the final proof is not `tcfs pull` into a physical
sync root. Honey publishes bytes, neo pulls once only so it can create a
physical `.tc` stub with `tcfs unsync`, honey publishes newer bytes, and neo
then reads the latest content through a mounted clean-name view. The mounted
view must pass `ls`/`find` traversal, avoid raw `.tc` / `.tcf` suffix leaks, and
return exact honey bytes from `cat`.

Use the bounded helper:

```bash
PUSH=1 \
RUN_HONEY=1 \
NEO_START_MOUNT=1 \
NEO_NFS=1 \
REMOTE=seaweedfs://HOST:8333/tcfs/mounted-reverse-read-manual \
task lazy:neo-mounted-reverse-read-plan
```

When neo/macOS cannot mount, use the Linux-equivalent row. It inverts the
actors: honey keeps only the physical `.tc` stub, neo publishes newer bytes,
and honey reads exact neo content through a mounted Linux clean-name view:

```bash
PUSH=1 \
RUN_HONEY=1 \
HONEY_START_MOUNT=1 \
REMOTE=seaweedfs://HOST:8333/tcfs/honey-mounted-reverse-read-manual \
task lazy:honey-mounted-reverse-read-plan
```

The helper archives honey initial/mutated push transcripts, neo physical
pull/unsync/status, the mounted `lazy-hydration-mounted-smoke.sh` transcript,
and the physical root state after mounted read. A passing live run should have
`proof=mounted-reverse-read-current-behavior` and
`neo_physical_after_mounted_read=stub_present`; the mounted read should hydrate
through the mount cache, not by converting the physical sync root back to a
hydrated file.

On neo/macOS, use `NEO_NFS=1` unless macFUSE is explicitly installed. Linux
hosts with FUSE3 can omit it.

Current live M4 evidence:

- [honey-mounted-reverse-read-20260510T042203Z](../release/evidence/honey-mounted-reverse-read-20260510T042203Z/)
  ran against
  `seaweedfs://100.64.48.53:8333/tcfs/honey-mounted-reverse-read-20260510T042203Z`.
- The Linux-equivalent row passed with
  `proof=linux-mounted-reverse-read-current-behavior`: honey pulled and
  unsynced a physical copy, neo pushed the mutation, honey mounted
  `ls`/`find`/`cat` returned exact neo content, and
  `honey_physical_after_mounted_read=stub_present`.
- [neo-mounted-reverse-read-20260510T035826Z](../release/evidence/neo-mounted-reverse-read-20260510T035826Z/)
  ran against
  `seaweedfs://100.64.48.53:8333/tcfs/neo-mounted-reverse-read-20260510T035826Z`.
- The pre-mount stages passed: honey initial push, neo physical pull, neo
  `tcfs unsync`, `sync state: not_synced`, and honey mutated push are archived.
- The row remains blocked before mounted `cat`: neo/macOS has no macFUSE
  installed, and the `NEO_NFS=1` fallback failed at `mount_nfs` with
  `Operation not permitted`.
- The Linux row does not close the neo/macOS or production Finder claim.

## Delete/Rename While Peer Unsynced

The M8 row intentionally stays narrower than a clean user-facing delete/rename
claim. It stages two files, has honey pull and `tcfs unsync` both into physical
`.tc` stubs, then has neo delete one path and rename the other by publishing the
new path and removing the old remote index entry. Honey verifies current
behavior: old paths fail to rehydrate, the renamed new path hydrates exact
bytes, and stale old stubs are recorded as an open product gap.

Use the bounded helper:

```bash
PUSH=1 \
RUN_HONEY=1 \
REMOTE=seaweedfs://HOST:8333/tcfs/delete-rename-unsynced-manual \
task lazy:neo-honey-delete-rename-unsynced-plan
```

The helper writes `result.env` with
`proof=delete-rename-peer-unsynced-current-behavior` when the live run passes.
That is not the same as claiming that TCFS has accepted tombstone semantics or
automatic stale-placeholder cleanup for every peer. A stronger QA row needs the
product decision first: remove stale physical `.tc` stubs, retain them with
explicit deleted/renamed status, or introduce durable tombstones and make that
visible through CLI, mounted views, and FileProvider.

Archived M8 evidence:

- [neo-honey-delete-rename-unsynced-20260510T040456Z](../release/evidence/neo-honey-delete-rename-unsynced-20260510T040456Z/)
  ran against
  `seaweedfs://100.64.48.53:8333/tcfs/neo-honey-delete-rename-unsynced-20260510T040456Z`.
- The run passed with `status=0`: honey pulled and unsynced both fixtures, neo
  deleted one path and renamed another, honey old-path pulls failed as expected,
  the renamed new path hydrated exact bytes, and `rename_new_pull=synced`.
- The run also records the current UX gap: `delete_stub_after_failed_pull` and
  `rename_old_stub_after_new_pull` are both `present`. A same-hash rename must
  use the current safe order, delete old remote path before publishing the new
  path, until manifest/reference semantics are hardened.

## Real Project-Tree Canary

After the isolated fleet packet, the next project-tree gate is an isolated
shadow of `/Users/jess/git/linux-xr`, not live home-directory management:

```bash
PUSH=1 \
RUN_HONEY=1 \
RUN_LINUX_LIFECYCLE=1 \
REMOTE=seaweedfs://HOST:8333/tcfs/home-canary-linux-xr-shadow-manual \
task lazy:home-canary-linux-xr-shadow
```

The helper inventories the live repo read-only, copies it to
`~/TCFS Pilot/real-canaries/linux-xr-shadow-<UTC>`, writes a disposable config
with raw `.git`, hidden dirs, and empty dirs enabled, and archives evidence
under `docs/release/evidence/home-canary-linux-xr-shadow-<UTC>/`. It records
symlinks and unsupported special files as truth gates; full project parity must
stay blocked until a future packet proves inventoried links rehydrate as symlinks
with matching targets. Current helper packets include source/shadow
`symlink-targets.tsv`, a local source/shadow target comparison, and honey mounted
`readlink` verification when `--run-honey` is used.

Archived scoped canary evidence:

- [home-canary-linux-xr-shadow-20260510T023938Z](../release/evidence/home-canary-linux-xr-shadow-20260510T023938Z/)
  completed a 92,969-file / 7.7 GB shadow push to
  `seaweedfs://100.64.48.53:8333/tcfs/home-canary-linux-xr-shadow-20260510T023938Z`.
- Honey mounted the prefix, listed clean names including `.git`, ran bounded
  traversal at `max-depth=3`, and `cat` hydrated `.clang-format` with exact
  24,291-byte content.
- The Linux lifecycle companion passed mounted write/readback, cache
  clear/rehydrate, dirty safe-unsync refusal, clean recursive unsync, and exact
  rehydrate under the same disposable prefix.
- Full project parity remains unclaimed for that archived packet because the
  source inventory records 85 symlinks that were not proven as preserved links.
- [home-canary-linux-xr-shadow-20260510T201809Z](../release/evidence/home-canary-linux-xr-shadow-20260510T201809Z/)
  is the fresh `sync_symlinks = true` blocker packet. It completed the shadow
  push, preserved local source/shadow target manifests for 85 symlinks, and
  recorded 85 symlink uploads, but honey mounted symlink verification failed at
  `Documentation/Changes` and the Linux lifecycle companion failed during
  mounted `cat`. Its storage notes are pre-fix S3 posture evidence, not
  post-fix throughput proof.

Run the helper from `nix develop` or through direnv so the repo-pinned Rust,
`go-task`, shell lint tools, `jq`, and S3 helper commands are active. The dev
shell intentionally prepends the pinned toolchain and proof-helper commands
because Home Manager profiles can otherwise shadow them with older ambient
tools.

The helper's own behavior is covered by:

```bash
scripts/test-home-canary-linux-xr-shadow.sh
scripts/test-lazy-hydration-mounted-smoke.sh
# or:
task lazy:test-home-canary-linux-xr-shadow
```

The host-runnable lazy proof gate is:

```bash
task lazy:check
```

That task runs shell syntax checks, shellcheck, helper regression suites, the
focused CLI unsync/resync regressions, and the `tcfs-vfs` tests that lock the
clean-name and lazy-cache contract. It does not replace the Linux FUSE demo,
same-fixture cross-host run, or clean-host Finder acceptance runs; those still
need the appropriate host surface.

## Linux <> Finder Parity Contract

Linux and Finder should prove the same user story even though the platform
representations differ. Treat Linux FUSE/NFS as the scriptable reference lane
and Finder/FileProvider as the native desktop lane.

| User behavior | Linux mounted surface | macOS Finder/FileProvider | Current proof state |
| --- | --- | --- | --- |
| Browse before download | `find` / `ls` show clean names backed by remote index entries | CloudStorage/Finder enumerates FileProvider items/placeholders | PZM testing-mode Finder enumeration is green; archived Linux FUSE evidence `lazy-linux-20260508T170825Z` is green; production Finder evidence is still pending |
| Hydrate on open | `cat` reads exact bytes and fills the VFS cache | Finder open, coordinated read, or host-app download request hydrates exact bytes | PZM testing-mode smoke proves exact-content FileProvider hydration on `v0.12.12`; archived Linux FUSE evidence proves exact `cat` hydration |
| Free space / dehydrate | clear VFS cache or run the surface's unsync/dehydrate path, then re-`cat` | evict/dehydrate placeholder and re-open | PZM testing-mode smoke proves FileProvider evict + rehydrate on `v0.12.12`; archived Linux FUSE evidence proves cache clear + rehydrate |
| Mutate and reconcile | edit through mounted view or sync root, then prove push/pull/conflict state | edit through Finder/FileProvider and prove daemon/FileProvider upload plus conflict/status behavior | Archived Linux FUSE evidence `lazy-linux-20260508T170825Z` proves mounted write/readback and recursive safe-unsync refusal/success. PZM testing-mode smoke run `25565943781` proves CloudStorage mutation upload and exact remote pull; PZM smoke run `25569596910` proves CLI conflict state and exact FileProvider content preservation; production Finder conflict/status remains open |
| Observe health | CLI status, daemon logs, mounted-smoke transcript | Finder state, FileProvider logs, badges/progress when available | CLI/log evidence exists; PZM run `25569596910` captured that the FileProvider enumerator did not emit a conflict hydration-state log, so Finder badges/progress remain observational only |

This means the old hosted FileProvider blocker no longer freezes the read-only
Finder proof, and the Linux lifecycle no longer lacks host evidence for read,
mounted mutation, cache rehydration, or recursive safe-unsync. The PZM
testing-mode lane now proves conflict/status content preservation, while
production Developer ID clean-host evidence and reliable Finder badge/progress
assertions remain separate.

The neo/honey CLI packets now add cross-host conflict detection and manual
keep-both recovery evidence to the same parity story: detection/preservation is
archived in `neo-honey-conflict-20260510T043741Z/`, and the scriptable
keep-both pattern is archived in
`neo-honey-conflict-keep-both-20260510T045908Z/`. Independent sibling progress
while another descendant remains conflicted is archived in
`neo-honey-conflict-sibling-20260510T051328Z/`. The daemon-backed keep-both
attempt is archived as
`neo-honey-conflict-daemon-keep-both-20260510T054611Z/`: the request reaches
isolated `tcfsd 0.12.12`, but the CLI times out after partial side effects.
That still is not production Finder conflict UI or daemon-backed `tcfs resolve`
acceptance.

Required proof:

1. Seed a fixture with at least one nested directory and one file.
2. Mount the remote prefix through `tcfs mount`.
3. Run `ls`/`find` and show the fixture names before file content is hydrated.
4. Run `cat` on a fixture file and verify exact content.
5. Verify that hydration used the cache path expected by `tcfs-vfs`.
6. Write/edit through the mounted view and verify exact remote pullback.
7. Run `tcfs unsync` or the daemon/VFS dehydration path appropriate to the
   surface being tested.
8. Re-open or re-`cat` the item and verify it hydrates again.

## Desktop-Originated Cross-Host Acceptance

The dramatic Desktop demo should start with an isolated folder, not the user's
real daily-driver `~/Desktop`:

```bash
mkdir -p "$HOME/Desktop/TCFS Demo"/{Projects,Photos,Notes}
```

Treat this as arbitrary-folder sync/unsync proof. Configure that demo folder to
sync to a disposable remote prefix, then mount the same prefix on `honey` at an
explicit test path such as `~/tcfs-demo/Desktop`. Over SSH, prove `find`/`ls`
against the honey mount before hydration and `cat` hydration of a known file.

The repo carries a safe helper for preparing that fixture and emitting the
honey commands:

```bash
task lazy:desktop-honey-plan
```

Live lab evidence from April 30, 2026 is recorded in
[Lazy Desktop-to-Honey Evidence](../release/lazy-desktop-honey-evidence-2026-04-30.md).

By default it creates `~/Desktop/TCFS Demo`, writes evidence artifacts, and does
not push remote data. To push the fixture once the disposable backend is ready:

```bash
TCFS_DESKTOP_DEMO_REMOTE=seaweedfs://host:8333/tcfs/desktop-demo-manual \
TCFS_DESKTOP_DEMO_PUSH=1 \
TCFS_DESKTOP_DEMO_EVIDENCE_DIR=docs/release/evidence/desktop-honey-manual \
task lazy:desktop-honey-plan
```

To run the honey side from the same helper after push, honey must already have
`tcfs`, mount permissions, and credentials for the chosen backend:

```bash
TCFS_DESKTOP_DEMO_REMOTE=seaweedfs://host:8333/tcfs/desktop-demo-manual \
TCFS_DESKTOP_DEMO_PUSH=1 \
TCFS_DESKTOP_DEMO_RUN_HONEY=1 \
TCFS_HONEY_START_MOUNT=1 \
task lazy:desktop-honey-plan
```

If honey does not already have credentials for that backend, the helper has an
explicit `--forward-aws-env` / `TCFS_HONEY_FORWARD_AWS_ENV=1` mode. It writes a
temporary 0600 env file on honey for the smoke run and removes it afterward;
do not use that mode for durable host setup. When it is combined with
`TCFS_HONEY_START_MOUNT=1`, unmount after inspection because the mount process
inherits those environment variables.

If honey's installed `tcfs` is older than the current workspace, build a current
Linux binary on honey and pass it with `TCFS_HONEY_TCFS_BIN=/path/to/tcfs`.

For the broader fleet-pilot packet, use the dedicated helper rather than
repurposing the Desktop helper. It creates isolated `Documents` and `git`
fixtures and can attach the honey-side Linux lifecycle proof under a nested
remote prefix:

```bash
TCFS_FLEET_PILOT_REMOTE=seaweedfs://host:8333/tcfs/fleet-pilot-manual \
TCFS_FLEET_PILOT_PUSH=1 \
TCFS_FLEET_PILOT_RUN_HONEY=1 \
TCFS_FLEET_PILOT_RUN_LINUX_LIFECYCLE=1 \
TCFS_HONEY_START_MOUNT=1 \
task lazy:fleet-pilot-plan
```

The lifecycle companion proves mounted write/readback, cache clear/rehydrate,
and recursive safe-unsync on `honey`; it still uses an isolated fixture and is
not evidence that real `~/Documents` or `~/git` have been taken over.

Archived extended fleet evidence:

- [fleet-pilot-extended-20260509T2152Z](../release/evidence/fleet-pilot-extended-20260509T2152Z/)
  proves isolated `Documents`/`git` seed, honey mounted traversal/hydration,
  honey Linux lifecycle companion write/readback plus safe-unsync, and live
  `neo-honey` backend smoke.

Do not describe this as `honey:~/Desktop` unless honey is deliberately
configured with TCFS at that exact path. The Finder/FileProvider proof remains
the `~/Library/CloudStorage/TCFS*` root, not the physical Desktop directory.
The broader parity contract is documented in
[odrive Parity and Product Horizon](odrive-parity-product-horizon.md).

The helper also refuses honey's real `~/Desktop` as the mount root by default.
Use the default `~/tcfs-demo/Desktop` target for repeatable proof. Only pass
`--allow-honey-real-desktop` / `TCFS_HONEY_ALLOW_REAL_DESKTOP=1` when the remote
host has been deliberately prepared for that takeover and the evidence should
show the higher-risk choice.

## macOS Finder Acceptance

The Finder lane is tracked by GitHub issue `#309` and the named harness in
`scripts/macos-postinstall-smoke.sh`.

PZM testing-mode evidence from May 8, 2026 is the current strongest
FileProvider proof:

- testing-mode package run `25445945705`
- post-install smoke run `25446601375`
- package install, signing/profile checks, live S3/E2EE fixture proof,
  `tcfsd` startup, CloudStorage enumeration, host-app `requestDownload`,
  55-byte hydration, exact-content match, and shared-Keychain config proof
- post-install smoke run `25562087555` on `v0.12.12` with the installed
  `TCFS FileProvider Lab Gatekeeper Rules` profile proved the current
  lifecycle gate: installed host policy probe, FileProvider registration,
  CloudStorage enumeration, `requestDownload`, `evict`, re-`requestDownload`,
  and exact-content hydration
- post-install smoke run `25565943781` proved CloudStorage mutation upload and
  exact 68-byte remote pullback
- post-install smoke run `25569596910` passed
  `exercise_conflict_status=true`, proving CLI `sync state: conflict` and exact
  FileProvider content preservation while keeping Finder badges/progress as
  observational

That is a lab/testing-mode proof, not a production Developer ID clean-host
claim.

The current `v0.12.12` PZM lifecycle extension adds evict/rehydrate, mutation,
and deterministic conflict/status content preservation to that same lane.
Earlier attempts showed the package, signing, profiles, S3/E2EE, and daemon
were sound but the installed Mac Development app needed a managed runtime-policy
rule. The green PZM runs prove the profile-backed lab path; they do not prove
production Finder enablement.

Local source-tree evidence from April 30, 2026 is recorded in
[macOS FileProvider Local Evidence](../release/macos-fileprovider-local-evidence-2026-04-30.md).
That run proved CloudStorage enumeration and exact-content `cat` hydration
through FileProvider on `neo`. The latest local pass also used a Developer ID
signed host app and extension with matching embedded provisioning profiles,
disabled build-time embedded FileProvider config, and proved the extension
loaded config from the shared Keychain. It is still intentionally not a
clean-host release claim because the workstation daemon was already running and
the app was installed from the source tree rather than a notarized `.pkg`.

The task wrapper intentionally requires a fixture path so a package-only smoke
cannot be mistaken for Finder/FileProvider hydration proof:

```bash
EXPECTED_FILE=path/to/known/remote-backed-file \
EXPECTED_CONTENT_FILE=/tmp/tcfs-expected-content.txt \
TCFS_REQUIRE_KEYCHAIN_CONFIG=1 \
task lazy:macos-finder-smoke
```

For release evidence, prefer the strict wrapper:

```bash
EXPECTED_FILE=path/to/known/remote-backed-file \
EXPECTED_CONTENT_FILE=/tmp/tcfs-expected-content.txt \
task lazy:macos-finder-release-smoke
```

Before recording production Finder evidence, run the non-mutating preflight in
strict signing mode:

```bash
TCFS_REQUIRE_PRODUCTION_SIGNING=1 task lazy:macos-finder-preflight
```

That gate fails unless the host app and FileProvider extension both codesign
cleanly, carry the shared Keychain access-group entitlement, and embed matching
provisioning profiles. It decodes the profiles and checks that bundle IDs, App
Group, concrete Keychain group, Apple team prefix, and Developer ID signing
certificate match the signed bundles.
For a freshly built app that is not installed yet, use
`task lazy:macos-finder-signing-preflight` with `APP_PATH=...` to run only the
signing/profile portion first.
To locate matching local provisioning profiles before building, run
`task lazy:macos-finder-profile-inventory`; it emits the two
`TCFS_*_PROVISIONING_PROFILE` environment assignments when a compatible pair is
installed. The FileProvider build can also use those profiles automatically
when `TCFS_AUTO_PROVISIONING_PROFILES=1` and
`TCFS_REQUIRE_PRODUCTION_SIGNING=1` are set. Production signing disables
build-time embedded FileProvider config by default, so the Finder proof must use
the host-app Keychain provisioning path instead of the diagnostic embedded path.
For release evidence, keep `TCFS_REQUIRE_KEYCHAIN_CONFIG=1` on the smoke run;
that requires FileProvider extension logs proving `loadConfig: loaded from
shared Keychain` and fails if the diagnostic embedded-config path was used.

Required proof:

1. Install the released `.pkg` or app bundle on a known-clean macOS host.
2. Add/update the `io.tinyland.tcfs` FileProvider domain and signal the
   FileProvider working set.
3. Confirm a `~/Library/CloudStorage/TCFS*` root appears.
4. Enumerate remote-backed fixture entries in Finder or via the CloudStorage path.
5. Request download of the expected placeholder through the containing host app.
6. Open/read a placeholder-backed file and verify exact content hydration.
7. Prove extension config loaded from the shared Keychain, not build-time
   embedded diagnostic config.
8. Record Finder state such as badges or progress as observational evidence until
   those become release gates.

GitHub-hosted macOS runners need a public reachable S3-compatible endpoint for
this lane. Tailscale-only, RFC1918, localhost, and CGNAT endpoints are not
sufficient for the hosted executor.

The `v0.12.7` hosted smoke historically proved that a published production
`.pkg` could install, pass signing, reach storage, start the daemon, and prove a
seeded E2EE fixture before hitting
`NSFileProviderErrorDomainDisabled` (`-2011`) / `Sync is not enabled for
"TCFSProvider"`. The later `v0.12.12` hosted production attempt is the current
package-lane truth for this sprint: it passed install/signing/installed
CLI/config gates, then failed earlier because the public tunnel hostname for
the storage fixture no longer resolved from GitHub-hosted macOS. Do not keep
cutting production release tags solely to retry either state.

## Hygiene TODO

- [x] Add a mounted-surface smoke helper for clean `ls`/`cat` hydration proof.
- [x] Add regression coverage for the mounted-surface smoke helper.
- [x] Add a core VFS regression that proves `readdir` is lazy and `open`
      hydrates the disk cache.
- [x] Add one host-runnable `task lazy:check` gate for the lazy hydration proof
      checks that do not need Linux FUSE or Finder.
- [x] Ensure the repo dev shell exposes pinned Rust, `task`, and `shellcheck`
      even when a Home Manager profile has older ambient tools.
- [x] Ensure the repo dev shell owns the lazy proof helper commands `jq`,
      `aws`, `s5cmd`, and `mc` instead of depending on ambient installs.
- [x] Ensure VFS and FileProvider readers accept sync-engine JSON index entries
      as well as legacy `manifest_hash=...` records.
- [x] Add a guarded Desktop-originated honey helper that refuses the real
      `~/Desktop` by default and emits repeatable honey smoke commands.
- [x] Guard honey's real `~/Desktop` mount target by default; require an
      explicit opt-in for full-Desktop takeover demos.
- [x] Expose the mounted-surface helper through the repo task surface.
- [x] Add a full Linux terminal demo harness that seeds a real backend, mounts
      it, and records `ls`/`cat`/dehydrate/rehydrate evidence.
- [x] Add evidence-directory support to the Linux terminal harness so a
      FUSE-capable host run can self-archive transcript and metadata.
- [x] Run the Linux terminal harness on a FUSE-capable host and archive the
      command output as release/demo evidence.
- [x] Keep FileProvider proof scoped to clean-host Finder acceptance rather than
      treating package installation alone as desktop proof.
- [x] Keep `.tc` wording limited to physical stub files; mounted VFS docs should
      say clean names with remote-backed hydration.
- [x] Update `TIN-133` so Linear points at GitHub `#309` and this acceptance
      target instead of old closed issue framing.
- [x] Refresh the remaining stale M10 Linear mirror descriptions.
- [x] Record local macOS FileProvider exact-content hydration evidence while
      keeping clean-host production acceptance separate.
- [x] Add a strict macOS preflight mode that fails release evidence when
      production signing entitlements or provisioning profiles are missing.
- [x] Add a signing-only macOS preflight path for validating a built
      `TCFSProvider.app` before installation or FileProvider registration.
- [x] Add a local provisioning-profile inventory helper for finding a matching
      host app / FileProvider extension profile pair before build.
- [x] Disable build-time embedded FileProvider config by default for production
      signing so Keychain provisioning is actually exercised.
- [x] Add a Finder smoke gate that requires extension logs proving shared
      Keychain config and rejects embedded diagnostic config.
- [x] Provision/sign the production macOS FileProvider host app and extension
      so the Keychain access group works without embedded diagnostic config.
- [x] Provision a PZM Mac App Development testing-mode host profile and matching
      FileProvider extension profile.
- [x] Resolve the PZM runtime-policy termination with the profile-backed
      testing-mode lab and rerun the current-tag lifecycle smoke.
- [x] Decide whether the non-`TIN-133` M10 Linear mirrors should remain open or
      be closed/superseded.
- [x] Decide whether the demo backend is disposable public S3 or the on-prem
      TCFS authority after the OpenTofu migration work settles.
