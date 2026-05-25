# TCFS Daily Driver Productionization Todo - 2026-05-24

Status timestamp: 2026-05-24T22:21:01Z

This is the current execution checklist for moving TCFS from an
evidence-backed alpha surface toward a robust daily-driver filesystem product.
It is intentionally strict: a passing package smoke, live canary, or scoped
root proof does not become a broad filesystem claim until restore, recovery,
security, and user-visible control paths are also proven.

## Current State

- `main` is clean at `43ce2274efe95c7ffd593d201707a88b3858f4b3`.
- No open GitHub PRs.
- Latest prerelease: `v0.12.13-rc4`.
- Public `Latest` release remains `v0.12.12`.
- Post-merge CI/Docs/Nix runs on `43ce227` are green:
  - CI `26246172625`
  - Docs `26246172578`
  - Nix CI `26246172626`
- Fresh storage posture canary `26246264661` is green on merged `main`, using
  `tcfs-storage-prod-smoke` with public HTTPS, public CA trust,
  allowed-prefix list/write/read/delete/delete-verify, and denied-prefix
  `PermissionDenied`.

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
- [ ] Use the headroom gate:

```bash
RESTORE_REQUIRE_HEADROOM=1 \
RESTORE_HEADROOM_MARGIN_BYTES=$((2 * 1024 * 1024 * 1024)) \
task lazy:git-repo-restore-proof
```

Evidence to archive:

- [ ] `restore-proof.env`
- [ ] reconcile dry-run and execute logs
- [ ] restored regular-file bytes and restore throughput
- [ ] partial restore bytes/count if execution fails
- [ ] retry and timeout counts
- [ ] transient error classification
- [ ] socket highwater under the restore/load path
- [ ] final explicit claim boundary

Close only the alpha storage sub-claim when the packet proves endpoint scope,
transport, and bounded failure classes. Keep `TIN-1546` open for beta if
large restore still fails, soak is missing, or transient recovery remains
ambiguous.

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
- [ ] Run the remote push/restore proof. The current shell is still pointed at
  local dev endpoints (`TCFS_S3_ENDPOINT=http://localhost:8333`,
  `TCFS_NATS_URL=nats://localhost:4222`), so the production-like restore cut
  must use an explicit HTTPS remote and scoped credentials or a correctly
  provisioned runner environment.

### 3. Harden `TIN-1547` FileProvider

The exact public rc4 `.pkg` proof is green. This lane is now UX/recovery
hardening, not exact hydration rescue.

- [ ] Decide whether badge/progress assertions are an alpha gate or explicitly
  beta scope.
- [ ] Capture recovery UX behavior for missing config, storage denial, and
  hydrate failure.
- [ ] Run a longer PZM desktop soak with no stale domain, registration, or
  config drift.
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

- [ ] Implement the CLI wizard path for fresh setup.
- [ ] Write a valid `config.toml`.
- [ ] Generate or import master key material.
- [ ] Verify `tcfs status [ok]` before exit.
- [ ] Make daemon/systemd/LaunchAgent missing-config behavior explicit and
  recoverable.
- [ ] Add package-smoke first-use rows that do not require hand-edited config.

### 6. `TIN-1417` Per-Device Crypto

Why second: real self-enrollment and revocation are not product boundaries
until devices have real local private keys and per-device wrapped content keys.

- [ ] Replace placeholder CLI enrollment public keys with real X25519 keys.
- [ ] Persist device private keys in a platform-appropriate secret store or
  explicitly protected fallback.
- [ ] Add file-key wrapping per non-revoked device.
- [ ] Prove a revoked device cannot decrypt new content.
- [ ] Document migration from shared-master fleets.

### 7. `TIN-1424` Pairing/Admin-Gated Enrollment

Why third: pairing depends on `TIN-1417`.

- [ ] Add single-use invite redemption state.
- [ ] Require admin/session gates for enrollment and revocation RPCs.
- [ ] Stop treating QR payloads with raw S3 credentials as a production path.
- [ ] Wrap bootstrap material to the new device public key.
- [ ] Keep iOS fail-closed until real device enrollment proof exists.

## Then: Daily-Driver Primitives

Target window: 2026-06-15 through 2026-06-30.

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

Large restore packet, on a host with the archived shadow root:

```bash
RESTORE_REQUIRE_HEADROOM=1 \
RESTORE_HEADROOM_MARGIN_BYTES=$((2 * 1024 * 1024 * 1024)) \
task lazy:git-repo-restore-proof
```

Named fleet acceptance, from the operator environment:

```bash
just neo-honey-smoke
```
