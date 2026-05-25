# TCFS Daily Driver Productionization Todo - 2026-05-24

Status timestamp: 2026-05-25T20:05:00Z

This is the current execution checklist for moving TCFS from an
evidence-backed alpha surface toward a robust daily-driver filesystem product.
It is intentionally strict: a passing package smoke, live canary, or scoped
root proof does not become a broad filesystem claim until restore, recovery,
security, and user-visible control paths are also proven.

## Current State

- `main` is at `d9ebf704197e50a0666ce4f055487e77c0f9c3a8` after PR
  `#462` merged the TIN-1621 retry/observability cut for package-backed
  large restores.
- Latest prerelease: `v0.12.13-rc4`.
- Public `Latest` release remains `v0.12.12`.
- Post-merge CI/Docs/Nix runs on `40b4514` are green:
  - CI `26378706285`
  - Docs `26378706284`
  - Nix CI `26378706243`
- Post-merge Docs run `26412349803` is green on `main@70e2eee`.
- Post-merge CI run `26412349802` and Nix CI run `26412349798` are green on
  `main@70e2eee`.
- Mainline package-backed storage large-restore run `26412362782` completed on
  `main@70e2eee` with `tcfs_binary_source=nix-package`,
  `pack_size_mib=3072`, `restore_headroom_margin_mib=2048`, and
  `require_https=true`. The run validated the production-smoke storage
  environment, built the Nix package successfully, pushed 30 files /
  3,222,239,922 bytes / 651 chunks with socket highwater 0, then failed
  fresh-tree restore on the 3.22 GB `.git/objects/pack/*.pack` after repeated
  Cloudflare/S3 `502` reads for one chunk. The artifact classifies the result
  as `regular file hash manifest mismatch`: 29/30 regular files restored and
  only 31,221 bytes present in the restore tree.
- Fresh package-backed storage large-restore run `26417405494` is running on
  `main@d9ebf70` with `tcfs_binary_source=nix-package`, `pack_size_mib=3072`,
  `restore_headroom_margin_mib=2048`, `download_chunk_retries=8`, and
  `require_https=true`. This is the classification rerun for `TIN-1621` after
  PR `#462`; do not close the ticket until the artifact proves exact 30/30
  restore or yields a new classified failure.
- PR `#459` pre-merge CI was green on head
  `4426eaa1f3591881756988a93371dd8bfd7a6458`, including CI
  `26404264594`, Docs `26404264596`, Nix CI `26404264593`, and CI Live
  Storage `26404264595`.
- PR `#460` merged the static TIN-1547 FileProvider surface contract guardrail
  at `aa104ce6273dcff5db1a36a1a8407530227291ac`.
- PR `#461` merged the TIN-1546 package-backed storage large-restore workflow
  at `70e2eee1db417be43f1ab62c319a3013097b45c1`.
- PR `#450` pre-merge CI was green, including CI `26379985552`, Docs
  `26379985540`, Nix CI `26379985542`, CI Live Storage `26379985538`, and
  Linux Package Container Smoke `26379985545`.
- Fresh storage posture canary `26246264661` is green on merged `main`, using
  `tcfs-storage-prod-smoke` with public HTTPS, public CA trust,
  allowed-prefix list/write/read/delete/delete-verify, and denied-prefix
  `PermissionDenied`.
- Branch validation run `26378404972` is green for the large-restore companion
  after PR `#448`: 1,074,101,203 bytes uploaded and restored under
  `gha/storage-posture/large/...`, exact fresh-tree restore, socket highwater
  0, and successful recovery despite transient S3/Cloudflare `502` read
  retries.
- Mainline run `26378842677` is green on `main@40b4514` for the same
  large-restore companion: 1,074,101,201 bytes uploaded/restored, exact
  fresh-tree restore, socket highwater 0, and successful recovery despite 42
  transient `502` read log lines.
- FileProvider PZM soak run `26380511749` is green for the public
  `v0.12.13-rc4` macOS arm64 `.pkg`: exact hydrate, five evict/rehydrate
  cycles, mutation remote pull, rename safety, and CLI conflict-status content
  hydrate all passed against `tcfs-storage-prod-smoke`.

## Claim Boundary

TCFS is close to alpha QA for trusted, named testers using
operator-managed infrastructure and scoped roots.

Do not claim yet:

- primary home-directory takeover
- broad `~/git`, `~/Documents`, dotfile, package-cache, or `.local` ownership
- self-service enrollment for arbitrary users
- meaningful lost-device revocation
- multitenant backend isolation
- iOS production readiness
- Windows Explorer readiness
- daily-driver readiness without caveats

## Tracker Reality

Completed alpha/M10 trackers:

- `TIN-131`: distribution install/upgrade matrix
- `TIN-132`: neo/honey live fleet acceptance packet
- `TIN-133`: production Finder/FileProvider hydration reality
- `TIN-1421`: real-storage CI lane
- `TIN-1422`: Linux postinstall smoke parity
- `TIN-1540`: hosted-reachable Linux smoke backend

Open alpha-gate trackers:

- `TIN-1546`: production S3/storage posture gate
- `TIN-1547`: FileProvider post-M10 product hardening

Open beta/daily-driver foundation:

- `TIN-1425`: first-run wizard
- `TIN-1417`: real per-device cryptographic identity and wrapped subkeys
- `TIN-1424`: pairing/admin-gated enrollment
- `TIN-1416`: subscription-based selective sync
- `TIN-1556`: stable root identity and broad-directory ownership
- `TIN-1419`: streaming large-file IO
- `TIN-1420`: xattr support
- `TIN-1549`: desktop status/progress/conflict recovery UX
- `TIN-1548`: iOS Files.app acceptance
- `TIN-1569`: Windows installer and Explorer/CFAPI posture
- `TIN-1617`: selected large-workdir onboarding pilot
- `TIN-1618`: large-workdir read-only inventory packet
- `TIN-1619`: selected large-workdir shadow pilot packet
- `TIN-1620`: one expendable live repo two-machine pilot
- `TIN-1621`: large-pack restore survives repeated S3 502s

## This Week: Alpha Closeout

Target window: 2026-05-24 through 2026-05-31.

### 1. Refresh Visibility

- [x] Publish a fresh Public FOSS Stewardship initiative status update.
- [x] Link this todo from the active alpha sprint board.
- [x] Comment on `TIN-1546` with the exact next large-restore packet steps.
- [x] Comment on `TIN-1547` with the exact FileProvider hardening closeout.

### 2. Close The `TIN-1546` Alpha-To-Beta Storage Gap

The alpha HTTPS/scoped credential packet is green. The remaining work is
large-restore/load posture, not small-object canary posture.

Required next packet:

- [x] Use a host that has, or can recreate, the `linux-xr-fast` shadow root.
- [x] Confirm restore host disk headroom before restore execution.
- [x] Use the headroom gate:

```bash
RESTORE_REQUIRE_HEADROOM=1 \
RESTORE_HEADROOM_MARGIN_BYTES=$((2 * 1024 * 1024 * 1024)) \
task lazy:git-repo-restore-proof
```

Evidence to archive:

- [x] `restore-proof.env`
- [x] reconcile dry-run and execute logs
- [x] restored regular-file bytes and restore throughput
- [x] partial restore bytes/count if execution fails
- [x] retry and timeout counts
- [x] transient error classification
- [x] socket highwater under the restore/load path
- [x] final explicit claim boundary

The alpha storage sub-claim is now green for scoped HTTPS credentials plus a
1 GiB synthetic Git-pack push/restore on merged main. Keep `TIN-1546` open for
beta because package-backed multi-GiB restore currently fails under repeated
S3/Cloudflare `502` chunk reads, and longer soak/load behavior plus benchmark
rows are still missing.

2026-05-24 progress:

- [x] Recreated a local shadow-first `linux-xr-fast` packet from clean source
  repo `xr/main` at `d362a939112e40d0dd0217ae34b0f63dbc862b11`.
- [x] Archived planning evidence at
  `docs/release/evidence/git-repo-canary-linux-xr-fast-20260524T222550Z`.
- [x] Kept the source repo unmodified and preserved the shadow root at
  `$HOME/TCFS Pilot/real-canaries/linux-xr-fast-shadow-20260524T222550Z`.
- [x] Confirmed the shadow includes large Git pack stress material, including
  a 2.4 GiB `.pack`, 235 MiB `.idx`, and 33 MiB `.rev`.
- [x] Built a local release `tcfs 0.12.13` binary for the next storage cut:
  `target/release/tcfs`
  (`1931d1bf9aff0371d5301ea8dcb87453bd47f3ee3a033cdcb9cb1e8866a83471`).
- [x] Confirmed local restore headroom for the shadow preflight: 2,872,328,021
  regular-file bytes plus a 2 GiB margin requires 5,019,811,669 bytes; the
  restore filesystem currently reports 46,338,109,440 free bytes.
- [x] Added a dispatch-only GitHub Actions lane,
  `.github/workflows/storage-large-restore-canary.yml`, so the large push and
  fresh-tree restore proof can consume the existing `tcfs-storage-prod-smoke`
  environment secrets without exposing them locally.
- [x] PR `#448` fixed the ANSI-colored tracing summary parser and allowed
  successful pushes with transient 5xx retry noise to proceed into restore.
- [x] Branch validation run `26378404972` passed the remote push/restore proof
  against `tcfs-storage-prod-smoke`: 30 files, 1,074,101,203 bytes uploaded and
  restored, one 1,074,069,982-byte Git pack, 365-second restore execution,
  2,942,743 B/s restored throughput, socket highwater 0, 76 transient `502`
  log lines, 36 OpenDAL retry rows, and 3 TCFS chunk-download retry rows.
- [x] Mainline run `26378842677` passed the same remote push/restore proof from
  `main@40b4514`: 30 files, 1,074,101,201 bytes uploaded and restored, one
  1,074,069,980-byte Git pack, 442,664 ms total upload elapsed,
  186-second restore execution, 5,774,737 B/s restored throughput, socket
  highwater 0, 42 transient `502` log lines, 20 OpenDAL retry rows, and
  1 TCFS chunk-download retry row.
- [x] Updated the dispatch lane so the default large-restore binary source is
  the current Nix `tcfs-cli` package (`tcfs_binary_source=nix-package`) instead
  of only a source-built `target/release/tcfs`.
- [x] Merged PR `#461` at
  `70e2eee1db417be43f1ab62c319a3013097b45c1`, making the Nix package path the
  default for the dispatch-only large-restore lane.
- [x] Dispatched mainline package-backed multi-GiB restore run `26412362782`
  against `tcfs-storage-prod-smoke` with `tcfs_binary_source=nix-package`.
- [x] Attached the `26412362782` result to the sprint truth: package-backed
  multi-GiB push passed; exact restore failed after repeated Cloudflare/S3
  `502` reads on one large-pack chunk. Keep `TIN-1546` open for stronger
  transient restore recovery and a clean package-backed rerun.
- [x] Added the first `TIN-1621` implementation cut in PR `#462`: larger
  default chunk-download retry budget, explicit workflow retry input, retry
  budget recording in restore packets, and full error-chain preservation for
  failed pulls.
- [ ] After `#462` lands, rerun the package-backed 3 GiB restore with
  `download_chunk_retries=8` and close `TIN-1621` only if the artifact restores
  30/30 files exactly.

### 3. Harden `TIN-1547` FileProvider

The exact public rc4 `.pkg` proof is green. This lane is now UX/recovery
hardening, not exact hydration rescue.

- [ ] Decide whether badge/progress assertions are an alpha gate or explicitly
  beta scope.
- [ ] Capture recovery UX behavior for missing config, storage denial, and
  hydrate failure.
- [x] Run a longer PZM desktop soak with no stale domain, registration, or
  config drift. The postinstall harness now accepts `--soak-cycles` so the
  existing evict/rehydrate proof can be repeated without changing the default
  one-pass release smoke.
- [x] Add a static FileProvider surface contract test for decoration
  declarations, fetch progress wiring, and custom action identifiers. This is
  a CI guardrail only; it does not replace a live Finder badge/progress packet.
- [ ] Keep installer-to-valid-config first-run proof under `TIN-1425`.
- [ ] Update `docs/ops/macos-fileprovider-reality.md` only with new evidence,
  not optimistic wording.

### 4. Keep Named Fleet Evidence Current

- [ ] Treat CI Live Storage as regression coverage.
- [ ] Keep the archived `neo-honey` transcript current for release-day
  acceptance, or record an explicit Linear supersede decision.

## Next: Beta Foundations

Target window: 2026-06-01 through 2026-06-14.

### 5. `TIN-1425` First-Run Wizard

Why first: every installer still lands binaries before the user has a valid
config, storage credentials, and unlocked encryption state.

Current code already has the headless base: `tcfs init` can generate/write
`master.key`, create `devices.json`, write `config.toml` unless
`--skip-config` is set, and `tcfs init --check` validates the local first-run
files.

- [x] Generate or import master key material for the trusted single-operator
  setup path.
- [x] Write a valid local `config.toml` from `tcfs init`.
- [x] Validate local first-run files with `tcfs init --check`.
- [x] Make daemon missing-config behavior explicit and recoverable; `tcfsd`
  now exits with a `tcfs init --config-out ...` recovery hint instead of
  running defaults.
- [x] Add installed-binary first-use smoke rows that run `tcfs init
  --non-interactive --config-out <temp>/config.toml`, then `tcfs init --check
  --config-out <temp>/config.toml`, then start `tcfsd` with that config.
- [x] Land PR `#450`, making `tcfsd` fail missing config with a
  `tcfs init --config-out ...` recovery hint and keeping ambient storage
  credential environment variables out of local install smoke unless storage is
  explicitly required.
- [x] Verify `tcfs status [ok]` before exit for storage-backed first-use
  packets; PR `#454` added `--require-storage-ok` Nix profile dispatch support
  and post-merge main run `26382186102` reached
  `storage: https://tcfs-smoke-s3.tinyland.dev [ok]` through the installed
  `tcfs init --config-out` path. The run passed, but `nix profile install`
  took 27 minutes before the smoke ran; keep profile-install latency visible in
  future release evidence.
- [x] Add an explicit `tcfs init --fileprovider-config-out <path>` path that
  writes the macOS HostApp/FileProvider bootstrap JSON from the same first-run
  state as `config.toml`, using the unified credential resolver and a `0600`
  temp file before atomic install.
- [x] Add `tcfs config fileprovider --out <path>` so package smokes can render
  the HostApp/FileProvider JSON from an existing config through installed Rust
  code instead of the legacy shell/TOML parser.
- [x] Extend the static FileProvider surface contract test so HostApp drift is
  caught if it stops reading `~/.config/tcfs/fileprovider/config.json` or stops
  enriching `master_key_file` into the Keychain `master_key_base64` payload.
- [ ] Dispatch the macOS postinstall smoke with
  `require_cli_fileprovider_config=true` on a package that includes
  `tcfs config fileprovider`, then prove the packaged HostApp consumes that
  Rust-owned JSON path and provisions shared Keychain config without a
  hand-authored `~/.config/tcfs/fileprovider/config.json`.
- [ ] Keep fleet join out of `tcfs init` until the remaining `TIN-1417` and
  `TIN-1424` product slices are complete; `tcfs init` should mean fresh local
  setup, while invite/pairing belongs to a separate safe enrollment path.

### 6. `TIN-1417` Per-Device Crypto

Why second: real self-enrollment and revocation are not product boundaries
until devices have real local private keys and per-device wrapped content keys.

- [x] Replace placeholder CLI enrollment public keys with real X25519 keys for
  the local `tcfs init` and `tcfs device enroll` paths.
- [x] Persist generated local device private keys in an explicitly protected
  `0600` file fallback beside the device registry.
- [ ] Add file-key wrapping per non-revoked device.
- [ ] Prove a revoked device cannot decrypt new content.
- [ ] Document migration from shared-master fleets.

2026-05-25 Phase 0 cut:

- [x] `tcfs-secrets::DeviceRegistry::enroll_local` generates a real
  age/X25519 keypair and stores only the public `age1...` recipient in
  `devices.json`.
- [x] `tcfs init` writes `device-<device_id>.age` beside `devices.json` and
  `tcfs init --check` rejects placeholder public keys or missing private-key
  files.
- [x] `tcfs device enroll` now uses the same local key-generation path instead
  of `age1-device-<hash>` placeholders.
- [ ] This does not yet change manifest wrapping, revoke semantics, pairing, or
  remote registry trust. Those remain the actual beta security boundary.

### 7. `TIN-1424` Pairing/Admin-Gated Enrollment

Why third: pairing depends on `TIN-1417`.

- [x] Add single-use invite redemption state.
- [x] Require admin/session gates for enrollment and revocation control RPCs.
- [x] Stop treating new QR payloads with raw S3 credentials as the default
  production path.
- [x] Wrap bootstrap material to the new device public key.
- [ ] Keep iOS fail-closed until real device enrollment proof exists.

2026-05-25 Phase 1 cut:

- [x] PR `#453` added daemon-side invite redemption keyed by
  `(invite_id, nonce)` and persists the claim before returning brokered
  credentials.
- [x] Replay now returns `success=false` with
  `invite has already been redeemed`.
- [x] Redemption persistence failures fail closed.
- [x] PR `#456` added admin/session gates for enrollment and revocation control
  RPCs and merged at
  `3c220d2ba41bb3b89cc03ecf6a8bca16a110da76` after green CI, live-storage,
  Nix, FileProvider staticlib, iOS typecheck, cargo-deny, and Secret Scan.
- [x] PR `#459` added `DeviceEnrollResponse.wrapped_bootstrap_age`, requires a
  real age/X25519 joining-device public key, wraps S3/bootstrap/master-key
  material to that key, rejects malformed public keys before invite claim, and
  stops returning raw S3/passphrase fields in new enrollment responses. It
  merged at `76ab051c1391fbc94bc6032135eadaeef061156f` after green CI,
  live-storage, Docs, Nix, FileProvider staticlib, iOS typecheck,
  cargo-deny, and Secret Scan.
- [ ] This does not yet productize pairing approval, client-side bootstrap
  persistence, iOS/Desktop join acceptance, per-device content-key wrapping, or
  true revocation denial for new content.

## Then: Daily-Driver Primitives

Target window: 2026-06-15 through 2026-06-30.

- [ ] `TIN-1617`: selected large-workdir onboarding pilot: inventory,
  shadow-root proof, one expendable live repo, then selected subtree rollout.
  Design recon: [Large Workdir Onboarding Design - 2026-05-25](large-workdir-onboarding-design-2026-05-25.md).
  Packet shape: inventory via `task lazy:large-workdir-inventory` or
  `scripts/large-workdir-inventory.py`; shadow packet via
  `task lazy:large-workdir-onboarding` or
  `scripts/home-canary-linux-xr-shadow.sh`; evidence under
  `docs/release/evidence/<run_id>/` with `source-inventory/`,
  `shadow-inventory/`, `push/`, `honey/`, `lifecycle/`, and `restore-proof/`
  as applicable. QA rows for the shadow pilot should stay on the minimal
  browse/hydrate/unsync/re-hydrate set: `T1`, `T2`, `T3`, `T4`, `T5`, `T6`,
  `T12`, `M1`, `M2`, `M3`, `M6`. The expendable live-repo step then adds
  `T10`, `T11`, `M5`, `M5-R`, and `M8`.
- [x] `TIN-1618`: first read-only inventory helper and regression test for
  candidate roots. It emits `inventory.json`, `inventory.env`, and
  `summary.md`; live pilot packet evidence is still pending.
- [ ] `TIN-1619`: shadow pilot packet for one selected large workdir.
- [ ] `TIN-1620`: one expendable live repo two-machine pilot.
- [ ] `TIN-1416`: subscription-based selective sync.
- [ ] `TIN-1556`: stable root IDs and broad-directory ownership.
- [ ] `TIN-1419`: streaming large-file IO for FUSE/FileProvider writes.
- [ ] `TIN-1420`: xattr capture/replay and manifest schema bump.
- [ ] `TIN-1549`: shared desktop status/progress/error/conflict vocabulary.

These are the features that make TCFS feel like a filesystem instead of a
collection of sync proofs.

## Platform Honesty

- [ ] `TIN-1548`: iOS remains proof-of-concept until a real Files.app
  acceptance lane and safe enrollment posture exist.
- [ ] `TIN-1569`: Windows remains skeleton until named-pipe daemon transport,
  MSI install, and Explorer/CFAPI proof exist.
- [ ] `TIN-1418`: multitenancy remains a design-doc and architecture
  workstream, not a near-term alpha/beta blocker unless the product claim
  changes to non-trusting shared backends.

## Stop Rules

Stop and file or escalate a prod-blocker if:

- storage transient errors are classified as missing objects
- a restore can hang without bounded read timeout behavior
- package install leaves a user with no supported path to valid config
- FileProvider delete/unsync/rename behavior can silently lose data
- invite bootstrap accepts tampered or unsigned material
- a revoked device can decrypt new content after removal without an explicit
  documented shared-master caveat
- desktop surfaces hide sync failure, conflict, or recovery state from users

## Immediate Next Commands

Read-only local classifier:

```bash
scripts/tcfs-alpha-gate-preflight.sh
```

Storage posture canary refresh:

```bash
scripts/storage-posture-canary-dispatch.sh \
  --environment tcfs-storage-prod-smoke \
  --runner-label ubuntu-24.04
```

Package-backed hosted large restore packet after `TIN-1621`:

```bash
gh workflow run storage-large-restore-canary.yml \
  --ref main \
  -f runner_label=ubuntu-24.04 \
  -f smoke_environment=tcfs-storage-prod-smoke \
  -f tcfs_binary_source=nix-package \
  -f pack_size_mib=3072 \
  -f restore_headroom_margin_mib=2048 \
  -f reconcile_timeout_secs=3600 \
  -f download_chunk_retries=8 \
  -f require_https=true
```

Named fleet acceptance, from the operator environment:

```bash
just neo-honey-smoke
```
