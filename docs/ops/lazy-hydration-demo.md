# Lazy Hydration Demo Acceptance

As of April 29, 2026, the core lazy traversal and hydration code exists, but the
project does not yet have one continuously repeatable demo that proves the
terminal and Finder user stories end to end.

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

## Linux Terminal Acceptance

Use a real S3/SeaweedFS-compatible backend or an explicit disposable backend,
not only an in-memory mock.

For the full Linux terminal lane, use the Linux-only harness:

```bash
scripts/lazy-hydration-linux-demo.sh \
  --remote seaweedfs://localhost:8333/tcfs/lazy-demo-manual \
  --create-bucket \
  --evidence-dir docs/release/evidence/lazy-linux-$(date -u +%Y%m%dT%H%M%SZ)
```

The same harness is exposed through the task surface:

```bash
TCFS_LAZY_DEMO_REMOTE=seaweedfs://localhost:8333/tcfs/lazy-demo-manual \
TCFS_LAZY_DEMO_CREATE_BUCKET=1 \
TCFS_LAZY_DEMO_EVIDENCE_DIR=docs/release/evidence/lazy-linux-manual \
task lazy:linux-demo
```

The harness seeds a fixture into a dedicated remote prefix, forces a direct
mount with a temp config, runs `find` before hydration, cats a known
remote-backed file, verifies cache hydration, clears the mount cache as the
mounted-surface dehydration step, and cats again to prove rehydration. It
requires Linux, `/dev/fuse`, `fusermount3` for the default FUSE backend, S3
credentials, and a pre-existing bucket unless `--create-bucket` can create it
with `aws`, `s5cmd`, or `mc`. The task surface also accepts
`TCFS_LAZY_DEMO_CREATE_BUCKET=1` and `TCFS_LAZY_DEMO_BACKEND=nfs`.

When `--evidence-dir` or `TCFS_LAZY_DEMO_EVIDENCE_DIR` is set, the harness
writes a transcript, redacted run metadata, final result status, `tcfs.toml`,
and mount log copy. The metadata intentionally records endpoint, bucket,
prefix, backend, and command shape, but not S3 credentials.

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

Run the helper from `nix develop` or through direnv so the repo-pinned Rust,
`go-task`, shell lint tools, `jq`, and S3 helper commands are active. The dev
shell intentionally prepends the pinned toolchain and proof-helper commands
because Home Manager profiles can otherwise shadow them with older ambient
tools.

The helper's own behavior is covered by:

```bash
scripts/test-lazy-hydration-mounted-smoke.sh
# or:
task lazy:test-mounted-smoke
```

The host-runnable lazy proof gate is:

```bash
task lazy:check
```

That task runs shell syntax checks, shellcheck, the mounted-smoke helper
regression suite, and the `tcfs-vfs` tests that lock the clean-name and
lazy-cache contract. It does not replace the Linux FUSE demo or clean-host
Finder acceptance runs; those still need the appropriate host surface.

Required proof:

1. Seed a fixture with at least one nested directory and one file.
2. Mount the remote prefix through `tcfs mount`.
3. Run `ls`/`find` and show the fixture names before file content is hydrated.
4. Run `cat` on a fixture file and verify exact content.
5. Verify that hydration used the cache path expected by `tcfs-vfs`.
6. Run `tcfs unsync` or the daemon/VFS dehydration path appropriate to the
   surface being tested.
7. Re-open or re-`cat` the item and verify it hydrates again.

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
- [ ] Run the Linux terminal harness on a FUSE-capable host and archive the
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
- [ ] Provision/sign the macOS FileProvider host app and extension so the
      Keychain access group works without embedded diagnostic config.
- [x] Decide whether the non-`TIN-133` M10 Linear mirrors should remain open or
      be closed/superseded.
- [x] Decide whether the demo backend is disposable public S3 or the on-prem
      TCFS authority after the OpenTofu migration work settles.
